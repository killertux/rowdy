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
    /// Index of the first visible row inside `visible_rows()`. Render keeps
    /// this clamped so the selected node stays inside the viewport.
    pub scroll_offset: usize,
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
            scroll_offset: 0,
        }
    }

    /// Index of the selected node within `visible_rows()`, if any.
    pub fn selected_index(&self) -> Option<usize> {
        let sel = self.selected?;
        self.visible_rows().iter().position(|r| r.id == sel)
    }

    /// Clamp `scroll_offset` so it never points past the last possible window
    /// and so the currently-selected row sits inside the viewport. Called
    /// from the render layer because `viewport_rows` depends on the rendered
    /// area, which the state layer doesn't know.
    pub fn clamp_scroll(&mut self, viewport_rows: usize) {
        let total = self.visible_rows().len();
        let view = viewport_rows.max(1);
        let max_offset = total.saturating_sub(view);
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
        if let Some(idx) = self.selected_index() {
            if idx < self.scroll_offset {
                self.scroll_offset = idx;
            } else if idx >= self.scroll_offset + view {
                self.scroll_offset = idx + 1 - view;
            }
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

#[cfg(test)]
mod tests {
    //! Tree-mutation tests for `SchemaPanel`. These don't talk to a
    //! datasource — they fabricate `CatalogInfo`/`TableInfo`/etc. and
    //! drive `populate_*` directly so the action-side bookkeeping
    //! (load state, expanded flags, parent walks) can be pinned.
    use super::*;
    use crate::datasource::schema::TableKind;
    use crate::datasource::{CatalogInfo, ColumnInfo, IndexInfo, SchemaInfo, TableInfo};

    fn catalog(name: &str) -> CatalogInfo {
        CatalogInfo { name: name.into() }
    }

    fn schema(name: &str) -> SchemaInfo {
        SchemaInfo { name: name.into() }
    }

    fn table(name: &str) -> TableInfo {
        TableInfo {
            name: name.into(),
            kind: TableKind::Table,
        }
    }

    fn view(name: &str) -> TableInfo {
        TableInfo {
            name: name.into(),
            kind: TableKind::View,
        }
    }

    fn column(name: &str, ty: &str, nullable: Option<bool>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            type_name: ty.into(),
            nullable,
        }
    }

    fn index(name: &str, unique: bool) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            unique,
        }
    }

    fn dispatched(outcome: ExpandOutcome) -> Vec<IntrospectTarget> {
        match outcome {
            ExpandOutcome::Dispatch(v) => v,
            ExpandOutcome::Nothing => Vec::new(),
        }
    }

    /// Walk down: catalog → schema → table, populating each level. Returns
    /// the table NodeId so tests can drive table-level expansion.
    fn build_to_table(p: &mut SchemaPanel) -> NodeId {
        p.populate_catalogs(vec![catalog("db")]);
        // Expand the catalog so its Schemas-target dispatches.
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        // Move to the schema, expand it.
        p.move_selection(1);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Tables {
                catalog: "db".into(),
                schema: "public".into(),
            },
            SchemaPayload::Tables(vec![table("users")]),
        );
        p.move_selection(1);
        p.selected.expect("table selected after descent")
    }

    // ----- bootstrap ----------------------------------------------------

    #[test]
    fn populate_catalogs_seeds_roots_and_selection() {
        let mut p = SchemaPanel::new(32);
        assert!(matches!(p.root_load_state, LoadState::NotLoaded));
        p.populate_catalogs(vec![catalog("db1"), catalog("db2")]);
        assert!(matches!(p.root_load_state, LoadState::Loaded));
        assert_eq!(p.roots.len(), 2);
        // First root selected automatically — vim convention.
        assert_eq!(p.selected, Some(p.roots[0]));
    }

    #[test]
    fn populate_catalogs_preserves_selection_when_already_set() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db1")]);
        let original = p.selected;
        // Re-populating doesn't reset the selection if one is already held.
        p.populate_catalogs(vec![catalog("db1"), catalog("db2")]);
        assert_eq!(p.selected, original);
    }

    // ----- selection movement ------------------------------------------

    #[test]
    fn move_selection_clamps_at_bounds() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("a"), catalog("b"), catalog("c")]);
        // Up from the top stays at the top.
        p.move_selection(-5);
        assert_eq!(p.selected, Some(p.roots[0]));
        // Down past bottom stops at last visible.
        p.move_selection(5);
        assert_eq!(p.selected, Some(p.roots[2]));
    }

    #[test]
    fn select_first_and_last_respect_visible_rows() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("a"), catalog("b")]);
        p.select_last();
        assert_eq!(p.selected, Some(p.roots[1]));
        p.select_first();
        assert_eq!(p.selected, Some(p.roots[0]));
    }

    #[test]
    fn move_selection_noop_on_empty_tree() {
        let mut p = SchemaPanel::new(32);
        p.move_selection(3);
        assert_eq!(p.selected, None);
        p.select_first();
        assert_eq!(p.selected, None);
    }

    // ----- expand_or_descend / load state cycle ------------------------

    #[test]
    fn first_expand_dispatches_load() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let outcome = p.expand_or_descend();
        let targets = dispatched(outcome);
        assert_eq!(
            targets,
            vec![IntrospectTarget::Schemas {
                catalog: "db".into()
            }]
        );
        // Catalog is now Loading until populate(...) lands.
        assert!(matches!(p.node(p.roots[0]).load_state, LoadState::Loading));
    }

    #[test]
    fn second_expand_while_loading_is_inert() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend(); // → Loading
        let outcome = p.expand_or_descend();
        // No re-dispatch while we're still waiting on the first one.
        assert!(matches!(outcome, ExpandOutcome::Nothing));
        assert!(matches!(p.node(p.roots[0]).load_state, LoadState::Loading));
    }

    #[test]
    fn loaded_then_expand_descends_to_first_child() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        // After populate the catalog is expanded with a child node.
        assert!(p.node(p.roots[0]).expanded);
        let schema_id = p.node(p.roots[0]).children[0];
        // Selecting and expanding again moves the selection into the child.
        let _ = p.expand_or_descend();
        assert_eq!(p.selected, Some(schema_id));
    }

    #[test]
    fn expand_table_synthesizes_columns_and_indices_folders() {
        let mut p = SchemaPanel::new(32);
        let table_id = build_to_table(&mut p);
        let outcome = p.expand_or_descend();
        let targets = dispatched(outcome);
        // Both folder targets are dispatched in one go.
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|t| matches!(
            t,
            IntrospectTarget::Columns { table, .. } if table == "users"
        )));
        assert!(targets.iter().any(|t| matches!(
            t,
            IntrospectTarget::Indices { table, .. } if table == "users"
        )));
        // Both folder children are Loading; the table itself is Loaded.
        let table_node = p.node(table_id);
        assert!(matches!(table_node.load_state, LoadState::Loaded));
        assert!(table_node.expanded);
        assert_eq!(table_node.children.len(), 2);
        for child_id in &table_node.children {
            assert!(matches!(
                p.node(*child_id).load_state,
                LoadState::Loading
            ));
        }
    }

    #[test]
    fn views_synthesize_folders_just_like_tables() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        p.move_selection(1);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Tables {
                catalog: "db".into(),
                schema: "public".into(),
            },
            SchemaPayload::Tables(vec![view("v_recent")]),
        );
        p.move_selection(1);
        let outcome = p.expand_or_descend();
        let targets = dispatched(outcome);
        assert_eq!(targets.len(), 2);
    }

    // ----- collapse_or_ascend ------------------------------------------

    #[test]
    fn collapse_then_ascend() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        // Catalog is currently expanded.
        assert!(p.node(p.roots[0]).expanded);
        p.collapse_or_ascend();
        // First call collapses the catalog (still selected on it).
        assert!(!p.node(p.roots[0]).expanded);
        // Move to the (now hidden) child node would be invalid; selection
        // sticks on the catalog and a second `h` walks to its parent —
        // but catalogs have no parent, so nothing happens.
        let before = p.selected;
        p.collapse_or_ascend();
        assert_eq!(p.selected, before);
    }

    #[test]
    fn collapse_from_child_walks_up() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        let schema_id = p.node(p.roots[0]).children[0];
        p.move_selection(1);
        assert_eq!(p.selected, Some(schema_id));
        // Schema isn't expanded; `h` walks to its parent (the catalog).
        p.collapse_or_ascend();
        assert_eq!(p.selected, Some(p.roots[0]));
    }

    // ----- toggle_selected ---------------------------------------------

    #[test]
    fn toggle_collapses_an_expanded_loaded_node() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        assert!(p.node(p.roots[0]).expanded);
        let outcome = p.toggle_selected();
        assert!(matches!(outcome, ExpandOutcome::Nothing));
        assert!(!p.node(p.roots[0]).expanded);
    }

    #[test]
    fn toggle_on_unloaded_dispatches() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let outcome = p.toggle_selected();
        assert_eq!(
            dispatched(outcome),
            vec![IntrospectTarget::Schemas {
                catalog: "db".into()
            }]
        );
    }

    // ----- populate / record_failure -----------------------------------

    #[test]
    fn populate_columns_uses_format_column() {
        let mut p = SchemaPanel::new(32);
        let _table_id = build_to_table(&mut p);
        let _ = p.expand_or_descend(); // synthesize columns/indices folders + dispatch
        let target = IntrospectTarget::Columns {
            catalog: "db".into(),
            schema: "public".into(),
            table: "users".into(),
        };
        p.populate(
            &target,
            SchemaPayload::Columns(vec![
                column("id", "INT", Some(false)),
                column("email", "TEXT", Some(true)),
            ]),
        );
        // Locate the columns folder and walk its children.
        let cols_folder = p.find_by_target(&target).expect("columns folder exists");
        let cols_node = p.node(cols_folder);
        assert!(matches!(cols_node.load_state, LoadState::Loaded));
        assert_eq!(cols_node.children.len(), 2);
        // Format includes type and nullability marker.
        let first = &p.node(cols_node.children[0]).label;
        assert!(first.contains("id"));
        assert!(first.contains("INT"));
    }

    #[test]
    fn populate_indices_renders_unique_marker() {
        let mut p = SchemaPanel::new(32);
        let _ = build_to_table(&mut p);
        let _ = p.expand_or_descend();
        let target = IntrospectTarget::Indices {
            catalog: "db".into(),
            schema: "public".into(),
            table: "users".into(),
        };
        p.populate(
            &target,
            SchemaPayload::Indices(vec![index("pk_users", true), index("idx_email", false)]),
        );
        let folder = p.find_by_target(&target).expect("indices folder exists");
        let labels: Vec<&str> = p
            .node(folder)
            .children
            .iter()
            .map(|id| p.node(*id).label.as_str())
            .collect();
        assert!(labels[0].contains("pk_users"));
        assert!(labels[0].contains("(unique)"));
        assert!(!labels[1].contains("(unique)"));
    }

    #[test]
    fn record_failure_flips_load_state() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.record_failure(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            "permission denied".into(),
        );
        match &p.node(p.roots[0]).load_state {
            LoadState::Failed(msg) => assert_eq!(msg, "permission denied"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(p.node(p.roots[0]).load_state.is_failed());
    }

    #[test]
    fn populate_with_unknown_target_is_silent() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        // A target that doesn't match any node — must not panic, must not
        // leave bogus state behind.
        p.populate(
            &IntrospectTarget::Tables {
                catalog: "missing".into(),
                schema: "missing".into(),
            },
            SchemaPayload::Tables(vec![table("ghost")]),
        );
        assert_eq!(p.roots.len(), 1);
    }

    // ----- visible_rows / parent_of ------------------------------------

    #[test]
    fn visible_rows_skip_collapsed_subtrees() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public"), schema("internal")]),
        );
        // Catalog expanded → both schemas visible.
        assert_eq!(p.visible_rows().len(), 3); // catalog + 2 schemas
        // Collapse the catalog; only the catalog row should remain.
        p.collapse_or_ascend();
        assert_eq!(p.visible_rows().len(), 1);
    }

    #[test]
    fn parent_of_walks_up_one_level() {
        let mut p = SchemaPanel::new(32);
        p.populate_catalogs(vec![catalog("db")]);
        let _ = p.expand_or_descend();
        p.populate(
            &IntrospectTarget::Schemas {
                catalog: "db".into(),
            },
            SchemaPayload::Schemas(vec![schema("public")]),
        );
        let schema_id = p.node(p.roots[0]).children[0];
        assert_eq!(p.parent_of(schema_id), Some(p.roots[0]));
        // Roots have no parent.
        assert_eq!(p.parent_of(p.roots[0]), None);
    }
}
