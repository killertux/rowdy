use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc::UnboundedSender;

use crate::worker::WorkerEvent;

use crate::autocomplete::SchemaCache;
use crate::config::ConfigStore;
use crate::connections::ConnectionStore;
use crate::datasource::DriverKind;
use crate::keybindings::keymap::Keymap;
use crate::llm::keystore::LlmKeyStore;
use crate::llm::worker::{PendingApprovalTool, PendingChatTool};
use crate::log::Logger;
use crate::state::chat::ChatPanel;
use crate::state::completion::CompletionState;
use crate::state::editor::EditorPanel;
use crate::state::focus::{Focus, PendingChord};
use crate::state::layout::LayoutCache;
use crate::state::overlay::Overlay;
use crate::state::results::ResultBlock;
use crate::state::right_panel::RightPanelMode;
use crate::state::schema::SchemaPanel;
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;
use crate::ui::theme::Theme;
use crate::user_config::UserConfigStore;
use crate::worker::{RequestCounter, RequestId, WorkerCommand};

pub const DEFAULT_SCHEMA_WIDTH: u16 = 32;
pub const MIN_SCHEMA_WIDTH: u16 = 12;
pub const MAX_SCHEMA_WIDTH: u16 = 80;

#[derive(Debug, Clone)]
pub struct InFlightQuery {
    pub req: RequestId,
    pub sql: String,
}

pub struct App {
    pub editor: EditorPanel,
    pub schema: SchemaPanel,
    pub chat: ChatPanel,
    /// Which panel the right side of the workspace shows. Toggles between
    /// schema and chat; defaults to schema so existing UX is preserved.
    pub right_panel: RightPanelMode,
    pub status: QueryStatus,
    pub results: Vec<ResultBlock>,
    pub focus: Focus,
    /// Persistent UI surface. Survives transient overlays (the run
    /// prompt, `:help`, etc.) — closing an overlay returns the user to
    /// whichever screen they were on.
    pub screen: Screen,
    /// Transient input-preempting layer. `Some` when a `:` prompt,
    /// confirm-run, in-flight connect, or help popover is up.
    pub overlay: Option<Overlay>,
    pub pending: PendingChord,
    pub theme: Theme,
    pub should_quit: bool,
    /// Set to a non-zero exit code by failed-auth flows so `main` can return
    /// the right status to the shell.
    pub exit_code: i32,
    pub cmd_tx: UnboundedSender<WorkerCommand>,
    /// Sink for `WorkerEvent`s from short-lived tokio tasks (currently
    /// the LLM streaming task). The long-running `worker::run` keeps its
    /// own clone; this one lets the action layer spawn ad-hoc tasks
    /// that funnel events into the same loop.
    pub evt_tx: UnboundedSender<WorkerEvent>,
    pub requests: RequestCounter,
    /// The currently in-flight query, if any. The SQL travels alongside so
    /// `on_query_done` can attach it to the resulting `ResultBlock` (used by
    /// `:export sql` for source-table inference).
    pub in_flight_query: Option<InFlightQuery>,
    pub config: ConfigStore,
    /// User-level defaults. Project `config` overrides per-field on
    /// read; runtime mutators write to `config` only.
    #[allow(dead_code)] // future `:set` writer; held for `:source` symmetry.
    pub user_config: UserConfigStore,
    /// `Arc` so `:source` can swap atomically.
    pub keymap: Arc<Keymap>,
    /// Set by the startup keybindings loader on parse failure.
    /// Drained once by `main` into the bottom bar status.
    pub startup_error: Option<String>,
    pub log: Logger,
    /// `Some` once the store is unlocked (or known plaintext). Until then
    /// connection management actions short-circuit.
    pub connection_store: Option<ConnectionStore>,
    /// Parallel keystore for LLM provider API keys. Populated together with
    /// `connection_store` (same `DerivedKey`, same plaintext-vs-encrypted
    /// mode), so a single password unlocks both.
    pub llm_keystore: Option<LlmKeyStore>,
    /// Name of the currently active connection (set on `Connected`). `None`
    /// while the worker has no datasource.
    pub active_connection: Option<String>,
    /// Driver kind of the active connection — snapshotted onto each
    /// `ResultBlock` so dialect-aware exports keep working after a
    /// `:conn use` switch to a different driver.
    pub active_dialect: Option<DriverKind>,
    /// Base `.rowdy/` directory — used to resolve session files and any
    /// other on-disk state.
    pub data_dir: PathBuf,
    /// Index of the currently active session within the active
    /// connection. Defaults to `0`; only meaningful when
    /// `active_connection.is_some()`. The corresponding on-disk file
    /// is `<data_dir>/sessions/<name>/session_<index>.sql`.
    pub active_session_index: usize,
    /// Sorted list of session indices that exist on disk for the
    /// active connection. Refreshed on connect, on `:session new`,
    /// and on `:session delete`. Always non-empty: `[0]` when no
    /// session file has been written yet.
    pub session_indices: Vec<usize>,
    /// Set whenever the editor buffer changes after a connection is active.
    /// Cleared by the debounced save (or the shutdown flush).
    pub editor_dirty: bool,
    /// When the next debounced session save should fire. Each edit pushes
    /// this 800ms into the future; the run loop watches it via
    /// `tokio::time::sleep_until`.
    pub pending_save_at: Option<tokio::time::Instant>,
    /// Shared autocomplete schema cache. The worker writes here on
    /// connect / `:reload` / lazy column loads; the engine reads here on
    /// every popover open. `Arc<RwLock<…>>` so the worker and the main
    /// loop can both hold handles without cloning the contents.
    pub schema_cache: Arc<RwLock<SchemaCache>>,
    /// Active autocomplete popover, if any. `Some` flips the keymap into
    /// "intercept popover keys before edtui" mode (see
    /// `event::translate_normal_key`).
    pub completion: Option<CompletionState>,
    /// When the user dismissed the popover with Esc at a given partial
    /// start (char offset in flattened buffer), don't auto-reopen at
    /// the same position. Cleared when the partial start moves.
    pub completion_snoozed_at: Option<usize>,
    /// Render-time layout cache used by the mouse handler. Populated as a
    /// side-effect of `ui::render`; consumed by the next `CtEvent::Mouse`
    /// to map (column, row) back to the panel that was clicked.
    pub layout: LayoutCache,
    /// Tool calls from the LLM stream that we paused while waiting for an
    /// introspection round-trip (auto-expand of an unloaded schema node).
    /// Drained by `action::chat::complete_pending_for_target` when the
    /// matching `WorkerEvent::SchemaLoaded` / `SchemaFailed` lands.
    pub pending_chat_tools: Vec<PendingChatTool>,
    /// Tool calls paused waiting for the user's y/n on an
    /// `Overlay::ConfirmToolUse` prompt. One pending entry at a time
    /// (the chat worker awaits each tool oneshot before consuming the
    /// next stream chunk), but the queue shape mirrors `pending_chat_tools`
    /// for consistency.
    pub pending_approval_tools: Vec<PendingApprovalTool>,
    /// Focus snapshot taken the first time an approval prompt yanks the
    /// user out of the chat composer. Restored once the queue empties so
    /// back-to-back approvals don't bounce focus on every keystroke and
    /// the user lands back where they were typing.
    pub focus_before_approval: Option<Focus>,
    /// Snapshot of the directory `rowdy` was launched from. The fs read
    /// tools (`read_file`, `list_directory`, `grep_files`) confine
    /// themselves to this subtree; storing it on `App` instead of calling
    /// `current_dir()` per tool means a future `cd` from inside an
    /// embedded shell can't shift the jail mid-session.
    pub project_root: PathBuf,
    /// Lazy `AGENTS.md` cache. Seeded at startup with the AGENTS.md
    /// living directly at `project_root` (no walk above the cwd).
    /// Grows on demand: every chat fs read tool (`read_file`,
    /// `list_directory`, `grep_files`) walks the touched directory's
    /// chain *up to* `project_root` and loads any AGENTS.md it finds
    /// in not-yet-visited directories. Cleared and re-seeded by
    /// `:source`. Rendered into the chat system prompt fresh on each
    /// turn so newly discovered content lands without a restart.
    pub agents_md: Arc<RwLock<crate::llm::agents_md::AgentsMdCache>>,
    /// User explicitly dismissed the inline result preview (`Q` /
    /// `:close`). Reset by every `dispatch_query`. The expanded-view
    /// path bypasses this; we only gate the inline split.
    pub preview_hidden: bool,
    /// Auto-update prompt waiting to be shown. Held here instead of
    /// going straight onto `overlay` so we don't preempt the password
    /// prompt, the connection picker, or the in-flight `Connecting`
    /// overlay at startup. Promoted to `Overlay::UpdateAvailable` by
    /// `update::try_promote_pending_prompt` once the user reaches a
    /// quiescent `Screen::Normal`.
    pub pending_update_prompt: Option<(String, String)>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cmd_tx: UnboundedSender<WorkerCommand>,
        evt_tx: UnboundedSender<WorkerEvent>,
        config: ConfigStore,
        user_config: UserConfigStore,
        keymap: Arc<Keymap>,
        startup_error: Option<String>,
        log: Logger,
        data_dir: PathBuf,
        schema_cache: Arc<RwLock<SchemaCache>>,
    ) -> Self {
        // Layered precedence: project pin > user pin > compiled
        // default. See `user_config::effective_theme` /
        // `effective_schema_width`.
        let project = config.state();
        let user = user_config.state();
        let theme_name =
            crate::user_config::effective_theme(project.theme.as_deref(), user.theme.as_deref());
        let width = crate::user_config::effective_schema_width(
            project.schema_width,
            user.schema_width,
            DEFAULT_SCHEMA_WIDTH,
        );
        let schema = SchemaPanel::new(width);
        let theme = Theme::by_name(&theme_name)
            .unwrap_or_else(|| Theme::for_kind(crate::ui::theme::ThemeKind::Dark));
        // Resolve once up front so both the struct field and the
        // `agents_md::load` call below see the same path. If
        // `current_dir` fails (extremely rare — the OS gave us no
        // cwd), fall back to `data_dir.parent()` so the fs tools have
        // *something* to jail against; the resolver still rejects
        // anything outside the chosen root.
        let project_root = std::env::current_dir().unwrap_or_else(|_| {
            data_dir
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| data_dir.clone())
        });
        let agents_md = Arc::new(RwLock::new(crate::llm::agents_md::AgentsMdCache::new()));
        let agents_md_seeded = agents_md.write().unwrap().seed_root(&project_root, &log);
        // Pre-populate the chat history with a system notice for
        // each AGENTS.md picked up at startup so users get
        // visibility on which files are influencing the agent's
        // behavior. Notices are in-memory only (not persisted) —
        // they reflect *this* session's load, not the connection's
        // chat transcript.
        let mut chat = crate::state::chat::ChatPanel::new();
        for path in &agents_md_seeded {
            chat.push_message(crate::state::chat::ChatMessage::system_text(format!(
                "Loaded AGENTS.md ({path})"
            )));
        }
        Self {
            editor: EditorPanel::new(),
            schema,
            chat,
            right_panel: RightPanelMode::default(),
            status: QueryStatus::Idle,
            results: Vec::new(),
            focus: Focus::Editor,
            screen: Screen::Normal,
            overlay: None,
            pending: PendingChord::None,
            theme,
            should_quit: false,
            exit_code: 0,
            cmd_tx,
            evt_tx,
            requests: RequestCounter::new(),
            in_flight_query: None,
            config,
            user_config,
            keymap,
            startup_error,
            log,
            connection_store: None,
            llm_keystore: None,
            active_connection: None,
            active_dialect: None,
            data_dir: data_dir.clone(),
            active_session_index: 0,
            session_indices: vec![0],
            editor_dirty: false,
            pending_save_at: None,
            schema_cache,
            completion: None,
            completion_snoozed_at: None,
            layout: LayoutCache::default(),
            pending_chat_tools: Vec::new(),
            pending_approval_tools: Vec::new(),
            focus_before_approval: None,
            project_root,
            agents_md,
            preview_hidden: false,
            pending_update_prompt: None,
        }
    }
}
