use crate::datasource::schema::TableKind;
use crate::datasource::{CatalogInfo, ColumnInfo, IndexInfo};
use crate::worker::IntrospectTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Catalog,
    Schema,
    Table,
    View,
    Folder, // synthetic "columns" / "indices" container under a table
    Column,
    Index,
}

#[derive(Debug, Clone)]
pub enum LoadState {
    NotLoaded,
    Loading,
    Loaded,
    Failed(String),
}

impl LoadState {
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

#[derive(Debug)]
pub struct SchemaNode {
    pub id: NodeId,
    pub label: String,
    pub kind: NodeKind,
    pub children: Vec<NodeId>,
    pub expanded: bool,
    pub load_state: LoadState,
    /// What to dispatch when this node is expanded for the first time.
    /// `None` means the node either has nothing to load (a leaf) or its
    /// children are synthesized locally on expand (e.g. a table).
    pub on_expand: Option<IntrospectTarget>,
}

#[derive(Debug)]
pub struct SchemaPanel {
    nodes: Vec<SchemaNode>,
    pub roots: Vec<NodeId>,
    pub root_load_state: LoadState,
    pub selected: Option<NodeId>,
    pub width: u16,
}

/// Outcome of an `expand_or_descend`/`toggle` action: signals whether the
/// action layer should dispatch one or more introspection requests to the
/// worker. Multiple targets are returned when expanding a table fans out
/// into both columns and indices loads at once.
#[derive(Debug)]
pub enum ExpandOutcome {
    Nothing,
    Dispatch(Vec<IntrospectTarget>),
}

impl SchemaPanel {
    pub fn new(width: u16) -> Self {
        Self {
            nodes: Vec::new(),
            roots: Vec::new(),
            root_load_state: LoadState::NotLoaded,
            selected: None,
            width,
        }
    }

    pub fn node(&self, id: NodeId) -> &SchemaNode {
        &self.nodes[id.0]
    }

    fn node_mut(&mut self, id: NodeId) -> &mut SchemaNode {
        &mut self.nodes[id.0]
    }

    pub fn parent_of(&self, child: NodeId) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|n| n.children.contains(&child))
            .map(|n| n.id)
    }

    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        let mut out = Vec::new();
        for &root in &self.roots {
            self.collect_visible(root, 0, &mut out);
        }
        out
    }

    fn collect_visible(&self, id: NodeId, depth: usize, out: &mut Vec<VisibleRow>) {
        let node = self.node(id);
        out.push(VisibleRow { id, depth });
        if node.expanded {
            for &child in &node.children {
                self.collect_visible(child, depth + 1, out);
            }
        }
    }

    // ---- selection movement ------------------------------------------------

    pub fn move_selection(&mut self, delta: i32) {
        let visible = self.visible_rows();
        if visible.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|sel| visible.iter().position(|r| r.id == sel))
            .unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, visible.len() as i32 - 1) as usize;
        self.selected = Some(visible[next].id);
    }

    pub fn select_first(&mut self) {
        if let Some(row) = self.visible_rows().first() {
            self.selected = Some(row.id);
        }
    }

    pub fn select_last(&mut self) {
        if let Some(row) = self.visible_rows().last() {
            self.selected = Some(row.id);
        }
    }

    // ---- expansion ---------------------------------------------------------

    /// Vim-tree `l`. Returns whether the action layer should dispatch a load.
    pub fn expand_or_descend(&mut self) -> ExpandOutcome {
        let Some(id) = self.selected else {
            return ExpandOutcome::Nothing;
        };
        self.expand_node(id)
    }

    /// Vim-tree `o`/Enter — same as `l` for now (toggle is identical when
    /// children are loaded; first expansion still has to dispatch).
    pub fn toggle_selected(&mut self) -> ExpandOutcome {
        let Some(id) = self.selected else {
            return ExpandOutcome::Nothing;
        };
        if self.node(id).expanded {
            self.node_mut(id).expanded = false;
            ExpandOutcome::Nothing
        } else {
            self.expand_node(id)
        }
    }

    fn expand_node(&mut self, id: NodeId) -> ExpandOutcome {
        let kind = self.node(id).kind;
        let load_state = self.node(id).load_state.clone();
        match load_state {
            LoadState::Loading => ExpandOutcome::Nothing,
            LoadState::Failed(_) | LoadState::NotLoaded => self.begin_loading(id, kind),
            LoadState::Loaded => self.descend_or_toggle(id),
        }
    }

    fn begin_loading(&mut self, id: NodeId, kind: NodeKind) -> ExpandOutcome {
        if matches!(kind, NodeKind::Table | NodeKind::View) {
            return self.synthesize_table_children(id);
        }
        let Some(target) = self.node(id).on_expand.clone() else {
            self.node_mut(id).load_state = LoadState::Loaded;
            self.node_mut(id).expanded = true;
            return ExpandOutcome::Nothing;
        };
        self.node_mut(id).load_state = LoadState::Loading;
        ExpandOutcome::Dispatch(vec![target])
    }

    fn descend_or_toggle(&mut self, id: NodeId) -> ExpandOutcome {
        let node = self.node(id);
        if node.children.is_empty() {
            return ExpandOutcome::Nothing;
        }
        if !node.expanded {
            self.node_mut(id).expanded = true;
            return ExpandOutcome::Nothing;
        }
        if let Some(&first) = self.node(id).children.first() {
            self.selected = Some(first);
        }
        ExpandOutcome::Nothing
    }

    /// Vim-tree `h`. Collapse if expanded, else move to parent.
    pub fn collapse_or_ascend(&mut self) {
        let Some(id) = self.selected else { return };
        if self.node(id).expanded {
            self.node_mut(id).expanded = false;
            return;
        }
        if let Some(parent) = self.parent_of(id) {
            self.selected = Some(parent);
        }
    }

    // ---- population from worker --------------------------------------------

    pub fn begin_root_load(&mut self) {
        self.root_load_state = LoadState::Loading;
    }

    pub fn fail_root_load(&mut self, error: String) {
        self.root_load_state = LoadState::Failed(error);
    }

    pub fn populate_catalogs(&mut self, catalogs: Vec<CatalogInfo>) {
        let new_roots: Vec<NodeId> = catalogs
            .into_iter()
            .map(|c| {
                let target = IntrospectTarget::Schemas {
                    catalog: c.name.clone(),
                };
                self.push_node(c.name, NodeKind::Catalog, Some(target))
            })
            .collect();
        self.roots = new_roots;
        self.root_load_state = LoadState::Loaded;
        if self.selected.is_none() {
            self.selected = self.roots.first().copied();
        }
    }

    pub fn populate(&mut self, target: &IntrospectTarget, payload: SchemaPayload) {
        let Some(node_id) = self.find_by_target(target) else {
            return;
        };
        let new_children = self.build_children(target, payload);
        let node = self.node_mut(node_id);
        node.children = new_children;
        node.expanded = true;
        node.load_state = LoadState::Loaded;
    }

    pub fn record_failure(&mut self, target: &IntrospectTarget, error: String) {
        let Some(node_id) = self.find_by_target(target) else {
            return;
        };
        self.node_mut(node_id).load_state = LoadState::Failed(error);
    }

    fn find_by_target(&self, target: &IntrospectTarget) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|n| n.on_expand.as_ref() == Some(target))
            .map(|n| n.id)
    }

    fn build_children(&mut self, target: &IntrospectTarget, payload: SchemaPayload) -> Vec<NodeId> {
        match (target, payload) {
            (IntrospectTarget::Schemas { catalog }, SchemaPayload::Schemas(schemas)) => schemas
                .into_iter()
                .map(|s| {
                    let on_expand = IntrospectTarget::Tables {
                        catalog: catalog.clone(),
                        schema: s.name.clone(),
                    };
                    self.push_node(s.name, NodeKind::Schema, Some(on_expand))
                })
                .collect(),
            (IntrospectTarget::Tables { catalog, schema }, SchemaPayload::Tables(tables)) => tables
                .into_iter()
                .map(|t| {
                    let _ = (catalog, schema);
                    let kind = match t.kind {
                        TableKind::Table => NodeKind::Table,
                        TableKind::View => NodeKind::View,
                    };
                    self.push_node(t.name, kind, None)
                })
                .collect(),
            (IntrospectTarget::Columns { .. }, SchemaPayload::Columns(cols)) => cols
                .into_iter()
                .map(|c| self.push_node(format_column(&c), NodeKind::Column, None))
                .collect(),
            (IntrospectTarget::Indices { .. }, SchemaPayload::Indices(indices)) => indices
                .into_iter()
                .map(|i| self.push_node(format_index(&i), NodeKind::Index, None))
                .collect(),
            // Catalogs handled via `populate_catalogs`; mismatches are ignored.
            _ => Vec::new(),
        }
    }

    fn synthesize_table_children(&mut self, table_id: NodeId) -> ExpandOutcome {
        let Some(path) = self.path_for_table(table_id) else {
            return ExpandOutcome::Nothing;
        };
        let TablePath {
            catalog,
            schema,
            table,
        } = path;
        let columns_target = IntrospectTarget::Columns {
            catalog: catalog.clone(),
            schema: schema.clone(),
            table: table.clone(),
        };
        let indices_target = IntrospectTarget::Indices {
            catalog,
            schema,
            table,
        };
        let columns_id = self.push_node("columns", NodeKind::Folder, Some(columns_target.clone()));
        let indices_id = self.push_node("indices", NodeKind::Folder, Some(indices_target.clone()));
        let table_node = self.node_mut(table_id);
        table_node.children = vec![columns_id, indices_id];
        table_node.expanded = true;
        table_node.load_state = LoadState::Loaded;

        // Auto-trigger both folder loads so the user sees columns and indices
        // appear without having to expand each folder individually.
        self.node_mut(columns_id).load_state = LoadState::Loading;
        self.node_mut(indices_id).load_state = LoadState::Loading;
        ExpandOutcome::Dispatch(vec![columns_target, indices_target])
    }

    fn path_for_table(&self, table_id: NodeId) -> Option<TablePath> {
        let schema_id = self.parent_of(table_id)?;
        let catalog_id = self.parent_of(schema_id)?;
        Some(TablePath {
            catalog: self.node(catalog_id).label.clone(),
            schema: self.node(schema_id).label.clone(),
            table: self.node(table_id).label.clone(),
        })
    }

    fn push_node(
        &mut self,
        label: impl Into<String>,
        kind: NodeKind,
        on_expand: Option<IntrospectTarget>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len());
        // Tables and views synthesize their children locally on first expand,
        // so they start NotLoaded even though they carry no `on_expand`.
        let needs_load = on_expand.is_some() || matches!(kind, NodeKind::Table | NodeKind::View);
        let load_state = if needs_load {
            LoadState::NotLoaded
        } else {
            LoadState::Loaded
        };
        self.nodes.push(SchemaNode {
            id,
            label: label.into(),
            kind,
            children: Vec::new(),
            expanded: false,
            load_state,
            on_expand,
        });
        id
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VisibleRow {
    pub id: NodeId,
    pub depth: usize,
}

struct TablePath {
    catalog: String,
    schema: String,
    table: String,
}

fn format_column(c: &ColumnInfo) -> String {
    let nullable_marker = match c.nullable {
        Some(true) => "?",
        Some(false) => " ",
        None => " ",
    };
    format!(
        "{name:<16} {ty}{nullable_marker}",
        name = c.name,
        ty = c.type_name
    )
}

fn format_index(i: &IndexInfo) -> String {
    let unique = if i.unique { " (unique)" } else { "" };
    format!("{name}{unique}", name = i.name)
}

// Re-exported here to avoid a circular use in the action layer.
pub use crate::worker::SchemaPayload;
