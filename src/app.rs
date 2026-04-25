use tokio::sync::mpsc::UnboundedSender;

use crate::state::editor::EditorPanel;
use crate::state::focus::{Focus, Mode, PendingChord};
use crate::state::results::ResultBlock;
use crate::state::schema::SchemaPanel;
use crate::state::status::QueryStatus;
use crate::ui::theme::Theme;
use crate::worker::{IntrospectTarget, RequestCounter, RequestId, WorkerCommand};

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
    pub cmd_tx: UnboundedSender<WorkerCommand>,
    pub requests: RequestCounter,
    pub in_flight_query: Option<RequestId>,
}

impl App {
    pub fn new(cmd_tx: UnboundedSender<WorkerCommand>) -> Self {
        let mut schema = SchemaPanel::new(DEFAULT_SCHEMA_WIDTH);
        let requests = RequestCounter::new();

        // Kick off the initial catalog load so the panel is populated as soon
        // as the worker has data; the UI shows a "loading…" state until then.
        let req = requests.next();
        schema.begin_root_load();
        let _ = cmd_tx.send(WorkerCommand::Introspect {
            req,
            target: IntrospectTarget::Catalogs,
        });

        Self {
            editor: EditorPanel::new(),
            schema,
            status: QueryStatus::Idle,
            results: Vec::new(),
            focus: Focus::Editor,
            mode: Mode::Normal,
            pending: PendingChord::None,
            theme: Theme::default(),
            should_quit: false,
            cmd_tx,
            requests,
            in_flight_query: None,
        }
    }
}
