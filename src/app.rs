use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc::UnboundedSender;

use crate::autocomplete::SchemaCache;
use crate::config::ConfigStore;
use crate::connections::ConnectionStore;
use crate::datasource::DriverKind;
use crate::llm::keystore::LlmKeyStore;
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
    pub requests: RequestCounter,
    /// The currently in-flight query, if any. The SQL travels alongside so
    /// `on_query_done` can attach it to the resulting `ResultBlock` (used by
    /// `:export sql` for source-table inference).
    pub in_flight_query: Option<InFlightQuery>,
    pub config: ConfigStore,
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
}

impl App {
    pub fn new(
        cmd_tx: UnboundedSender<WorkerCommand>,
        config: ConfigStore,
        log: Logger,
        data_dir: PathBuf,
        schema_cache: Arc<RwLock<SchemaCache>>,
    ) -> Self {
        let initial = config.state();
        let schema = SchemaPanel::new(initial.schema_width);
        let theme = Theme::for_kind(initial.theme);
        Self {
            editor: EditorPanel::new(),
            schema,
            chat: ChatPanel::new(),
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
            requests: RequestCounter::new(),
            in_flight_query: None,
            config,
            log,
            connection_store: None,
            llm_keystore: None,
            active_connection: None,
            active_dialect: None,
            data_dir,
            editor_dirty: false,
            pending_save_at: None,
            schema_cache,
            completion: None,
            completion_snoozed_at: None,
            layout: LayoutCache::default(),
        }
    }
}
