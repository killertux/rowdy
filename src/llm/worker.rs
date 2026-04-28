//! Per-turn streaming worker.
//!
//! `spawn_chat_turn` is a short-lived `tokio::spawn` that owns the LLM
//! client for one turn. It forwards each token delta as
//! `WorkerEvent::ChatDelta(Text(_))` and signals completion with `Done`
//! (or `Error` on failure). Cancellation lands in a later phase via a
//! oneshot or `CancelChat` command — for now the task simply runs to
//! completion or aborts when the worker join handle is dropped.
//!
//! The task does *not* mutate `App`. It only sends events; the action
//! layer's `apply_worker_event` mutates `app.chat` in response. This
//! preserves rowdy's "actions own all state mutation" invariant.

use futures::StreamExt;
use llm::LLMProvider;
use llm::chat::ChatMessage as LlmChatMessage;
use tokio::sync::mpsc::UnboundedSender;

use crate::state::chat::{ChatBlock, ChatMessage, ChatRole};
use crate::worker::WorkerEvent;

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

/// Spawn a tokio task that streams a chat turn. Caller is responsible
/// for building `client` (typically via `crate::llm::provider::build_client`)
/// — this keeps the keystore + secrets handling on the UI thread.
pub fn spawn_chat_turn(
    client: Box<dyn LLMProvider>,
    history: Vec<ChatMessage>,
    evt_tx: UnboundedSender<WorkerEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let messages: Vec<LlmChatMessage> = history.iter().map(translate_message).collect();
        let stream_result = client.chat_stream(&messages).await;
        let mut stream = match stream_result {
            Ok(s) => s,
            Err(err) => {
                let _ = evt_tx.send(WorkerEvent::ChatDelta(ChatDelta::Error(err.to_string())));
                return;
            }
        };
        let mut full = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    full.push_str(&chunk);
                    if evt_tx
                        .send(WorkerEvent::ChatDelta(ChatDelta::Text(chunk)))
                        .is_err()
                    {
                        // Receiver dropped — UI is shutting down. Bail.
                        return;
                    }
                }
                Err(err) => {
                    let _ = evt_tx.send(WorkerEvent::ChatDelta(ChatDelta::Error(err.to_string())));
                    return;
                }
            }
        }
        let _ = evt_tx.send(WorkerEvent::ChatDelta(ChatDelta::Done { full_text: full }));
    })
}

fn translate_message(msg: &ChatMessage) -> LlmChatMessage {
    let builder = match msg.role {
        // The `llm` crate's `ChatRole` only supports User and Assistant; our
        // `System` is folded into the LLMBuilder's `.system(...)` call before
        // the turn starts, so we never have a System message in the history
        // we're translating here. If one slips through (e.g. a hand-built
        // synthetic message for tool errors in phase 4) it goes out as
        // user-role text, which is the safer interpretation.
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
