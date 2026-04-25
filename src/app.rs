use tokio::sync::mpsc::UnboundedSender;

use crate::config::ConfigStore;
use crate::connections::ConnectionStore;
use crate::log::Logger;
use crate::state::editor::EditorPanel;
use crate::state::focus::{Focus, Mode, PendingChord};
use crate::state::results::ResultBlock;
use crate::state::schema::SchemaPanel;
use crate::state::status::QueryStatus;
use crate::ui::theme::Theme;
use crate::worker::{RequestCounter, RequestId, WorkerCommand};

pub const DEFAULT_SCHEMA_WIDTH: u16 = 32;
pub const MIN_SCHEMA_WIDTH: u16 = 12;
pub const MAX_SCHEMA_WIDTH: u16 = 80;

pub struct App {
    pub editor: EditorPanel,
    pub schema: SchemaPanel,
    pub status: QueryStatus,
    pub results: Vec<ResultBlock>,
    pub focus: Focus,
    pub mode: Mode,
    pub pending: PendingChord,
    pub theme: Theme,
    pub should_quit: bool,
    /// Set to a non-zero exit code by failed-auth flows so `main` can return
    /// the right status to the shell.
    pub exit_code: i32,
    pub cmd_tx: UnboundedSender<WorkerCommand>,
    pub requests: RequestCounter,
    pub in_flight_query: Option<RequestId>,
    pub config: ConfigStore,
    pub log: Logger,
    /// `Some` once the store is unlocked (or known plaintext). Until then
    /// connection management actions short-circuit.
    pub connection_store: Option<ConnectionStore>,
    /// Name of the currently active connection (set on `Connected`). `None`
    /// while the worker has no datasource.
    pub active_connection: Option<String>,
}

impl App {
    pub fn new(cmd_tx: UnboundedSender<WorkerCommand>, config: ConfigStore, log: Logger) -> Self {
        let initial = config.state();
        let schema = SchemaPanel::new(initial.schema_width);
        let theme = Theme::for_kind(initial.theme);
        Self {
            editor: EditorPanel::new(),
            schema,
            status: QueryStatus::Idle,
            results: Vec::new(),
            focus: Focus::Editor,
            mode: Mode::Normal,
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
            active_connection: None,
        }
    }
}
