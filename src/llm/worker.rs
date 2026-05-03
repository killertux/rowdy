//! Per-turn streaming worker.
//!
//! Owns one chat turn end-to-end: open `chat_stream_with_tools`, forward
//! text deltas, route tool calls through the action layer (which has the
//! `App` mutable handle the tools need), append results to the
//! conversation, and re-stream until the model returns without invoking
//! any further tools.
//!
//! Events flow over `WorkerEvent`:
//! - [`WorkerEvent::ChatDelta`] carries text deltas, completion, errors.
//! - [`WorkerEvent::ChatToolRequest`] is a request/reply pair: the worker
//!   sends a `oneshot::Sender<ToolReply>`, the action layer fills it
//!   in, and the worker resumes the loop.
//!
//! Cancellation lands in a future phase via a separate cancel channel.

use futures::StreamExt;
use llm::LLMProvider;
use llm::chat::{ChatMessage as LlmChatMessage, StreamChunk, Tool};
use llm::{FunctionCall, ToolCall};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::state::chat::{ChatBlock, ChatMessage, ChatRole};
use crate::worker::{IntrospectTarget, WorkerEvent};

/// Cap on tool-call rounds in a single turn so a misbehaving model can't
/// pin the worker. Sized to comfortably fit codebase-exploration tasks
/// (grep → read → grep → describe → write_buffer chains can run 6-8
/// rounds on their own); 20 leaves headroom without letting a runaway
/// loop stall the chat indefinitely.
const MAX_TOOL_ROUNDS: usize = 20;

/// How many times to (re)attempt the initial `chat_stream_with_tools`
/// call before giving up and surfacing the error. Covers transient
/// network blips and provider-side hiccups (DNS, TLS handshake,
/// short-lived 5xx). Mid-stream errors deliberately don't retry —
/// we may have already painted partial assistant text and silently
/// re-streaming would produce duplicates.
const MAX_STREAM_ATTEMPTS: u8 = 3;
/// Backoff before retry N (1-indexed). 250ms / 500ms keeps a wedged
/// connection from blowing up the user's session while still bounding
/// the total wait under a second on permanent failures.
const STREAM_RETRY_BACKOFF_MS: [u64; (MAX_STREAM_ATTEMPTS - 1) as usize] = [250, 500];

/// One streaming-delta event surfaced into the main event loop.
#[derive(Debug)]
pub enum ChatDelta {
    /// Token (or token-cluster) appended to the current assistant turn.
    Text(String),
    /// Stream finished cleanly. Carries the full text in case the action
    /// layer wants to flush it to disk in one piece — the `Text` deltas
    /// already cover the live UI.
    Done { full_text: String },
    /// Stream errored out. The string is the user-facing message.
    Error(String),
}

/// Reply payload for a `ChatToolRequest`. The action layer fills `result`
/// with the tool's JSON return value (success or an `{ "error": "..." }`
/// shape — both are JSON the LLM can read), and `display` with the
/// human-readable text we already painted into the chat panel.
#[derive(Debug)]
pub struct ToolReply {
    pub result: Value,
    /// Pretty form of `result` to splice into the LLM's `tool_result`
    /// message body. Currently we re-serialize `result`, but exposing the
    /// hook lets future phases truncate large outputs (full row dumps,
    /// etc.) without losing what the LLM sees.
    pub display: String,
}

pub struct ChatTurn {
    pub client: Box<dyn LLMProvider>,
    pub history: Vec<ChatMessage>,
    pub evt_tx: UnboundedSender<WorkerEvent>,
    /// Tool catalog the LLM sees. Built by the action layer at submit
    /// time, filtered by the user's `ReadToolsMode` preference (Off
    /// strips the fs read tools from the list entirely).
    pub tools: Vec<Tool>,
}

/// A tool call whose execution is paused until an introspection result
/// arrives. Schema tools that hit a cache miss queue one of these,
/// dispatch a `WorkerCommand::Introspect`, and let the resulting
/// `WorkerEvent::SchemaLoaded`/`SchemaFailed` arrival drain the queue —
/// at which point we re-run the tool against the freshly-populated
/// cache and reply on the oneshot.
#[derive(Debug)]
pub struct PendingChatTool {
    pub target: IntrospectTarget,
    pub call_id: String,
    pub tool_name: String,
    pub args_json: String,
    pub reply: oneshot::Sender<ToolReply>,
}

/// A filesystem read tool paused waiting for explicit user approval.
/// Created in `action::chat::on_tool_request` when
/// [`crate::user_config::ReadToolsMode::Ask`] is active and the LLM
/// tried to call `read_file`, `list_directory`, or `grep_files`.
/// Drained by the `ToolApproveAccept`/`Deny` action handlers — accept
/// replays into the regular `finalize_tool` path; deny replies with a
/// refusal payload so the LLM can recover instead of waiting forever.
#[derive(Debug)]
pub struct PendingApprovalTool {
    pub call_id: String,
    pub tool_name: String,
    pub args_json: String,
    pub reply: oneshot::Sender<ToolReply>,
}

/// Spawn a tokio task that drives one chat turn (and any follow-up
/// tool-result turns) to completion.
pub fn spawn_chat_turn(turn: ChatTurn) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_turn(turn).await;
    })
}

async fn run_turn(turn: ChatTurn) {
    let ChatTurn {
        client,
        history,
        evt_tx,
        tools,
    } = turn;

    let mut messages: Vec<LlmChatMessage> = history.iter().map(translate_message).collect();
    let mut full_text = String::new();

    for round in 0..=MAX_TOOL_ROUNDS {
        let mut stream = match open_stream_with_retry(&*client, &messages, &tools).await {
            Ok(s) => s,
            Err(err) => {
                send_error(&evt_tx, err);
                return;
            }
        };

        let mut round_text = String::new();
        let mut completed_tool_calls: Vec<ToolCall> = Vec::new();
        let mut completed_results: Vec<ToolCall> = Vec::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamChunk::Text(s)) => {
                    full_text.push_str(&s);
                    round_text.push_str(&s);
                    if evt_tx
                        .send(WorkerEvent::ChatDelta(ChatDelta::Text(s)))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(StreamChunk::ToolUseComplete { tool_call, .. }) => {
                    let (tx, rx) = oneshot::channel::<ToolReply>();
                    if evt_tx
                        .send(WorkerEvent::ChatToolRequest {
                            call_id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                            args_json: tool_call.function.arguments.clone(),
                            reply: tx,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let reply = match rx.await {
                        Ok(r) => r,
                        Err(_) => {
                            // Receiver dropped without replying — surface
                            // an error rather than hanging silently.
                            send_error(&evt_tx, "tool dispatch dropped".into());
                            return;
                        }
                    };
                    completed_tool_calls.push(tool_call.clone());
                    completed_results.push(ToolCall {
                        id: tool_call.id.clone(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: tool_call.function.name.clone(),
                            arguments: serde_json::to_string(&reply.result)
                                .unwrap_or_else(|_| reply.display.clone()),
                        },
                    });
                }
                Ok(_) => {} // ToolUseStart, ToolUseInputDelta, Done — fine to ignore.
                Err(err) => {
                    send_error(&evt_tx, err.to_string());
                    return;
                }
            }
        }

        if completed_tool_calls.is_empty() {
            // Model finished without calling another tool — done.
            let _ = evt_tx.send(WorkerEvent::ChatDelta(ChatDelta::Done { full_text }));
            return;
        }

        if round == MAX_TOOL_ROUNDS {
            send_error(&evt_tx, "tool-call budget exceeded — aborting turn".into());
            return;
        }

        // Append the assistant's tool-use turn + the tool-result turn,
        // then loop for the model's follow-up.
        messages.push(
            LlmChatMessage::assistant()
                .tool_use(completed_tool_calls)
                .content(round_text)
                .build(),
        );
        messages.push(
            LlmChatMessage::user()
                .tool_result(completed_results)
                .content("")
                .build(),
        );
    }
}

/// Open the LLM stream, retrying transient failures up to
/// `MAX_STREAM_ATTEMPTS` total tries. Returns the final error message
/// (annotated with the attempt count) when every attempt fails, so the
/// surfaced error tells the user this wasn't a one-shot blip.
///
/// Only the *initial* connect retries — once we've started consuming
/// chunks we don't restart, since partial deltas have already painted
/// into the chat panel.
async fn open_stream_with_retry(
    client: &dyn LLMProvider,
    messages: &[LlmChatMessage],
    tools: &[Tool],
) -> Result<
    std::pin::Pin<
        Box<dyn futures::stream::Stream<Item = Result<StreamChunk, llm::error::LLMError>> + Send>,
    >,
    String,
> {
    let mut last_err: Option<String> = None;
    for attempt in 1..=MAX_STREAM_ATTEMPTS {
        match client.chat_stream_with_tools(messages, Some(tools)).await {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt < MAX_STREAM_ATTEMPTS {
                    let backoff = STREAM_RETRY_BACKOFF_MS[(attempt - 1) as usize];
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
            }
        }
    }
    Err(format!(
        "chat stream failed after {MAX_STREAM_ATTEMPTS} attempts: {}",
        last_err.unwrap_or_else(|| "unknown error".into())
    ))
}

fn send_error(evt_tx: &UnboundedSender<WorkerEvent>, msg: String) {
    let _ = evt_tx.send(WorkerEvent::ChatDelta(ChatDelta::Error(msg)));
}

fn translate_message(msg: &ChatMessage) -> LlmChatMessage {
    let builder = match msg.role {
        // The `llm` crate's `ChatRole` only supports User and Assistant;
        // our `System` is folded into the LLMBuilder's `.system(...)`
        // call before the turn starts, so we never have a System message
        // in the history we're translating here. If one slips through it
        // goes out as user-role text, which is the safer interpretation.
        ChatRole::User | ChatRole::System => LlmChatMessage::user(),
        ChatRole::Assistant => LlmChatMessage::assistant(),
    };
    builder.content(extract_text(msg)).build()
}

fn extract_text(msg: &ChatMessage) -> String {
    let mut out = String::new();
    for block in &msg.blocks {
        if let ChatBlock::Text(t) = block {
            out.push_str(t);
        }
    }
    out
}
