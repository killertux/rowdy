//! `Action::Completion(_)` dispatcher and the auto-trigger / refresh
//! plumbing the editor and worker layers call into.
//!
//! `apply` is the user-facing entry; `refresh` and `maybe_auto_trigger`
//! are exported because the editor-event handler and the worker
//! cache-update handler both need to wake the popover after their own
//! state changes.

use crate::action::{CompletionAction, schedule_session_save};
use crate::app::App;
use crate::autocomplete;
use crate::state::focus::Focus;
use crate::state::status::QueryStatus;
use crate::worker::WorkerCommand;

pub fn apply(app: &mut App, action: CompletionAction) {
    match action {
        CompletionAction::Open => open(app, OpenSource::Manual),
        CompletionAction::Close => close(app, true),
        CompletionAction::Up => {
            if let Some(state) = app.completion.as_mut() {
                state.move_selection(-1);
            }
        }
        CompletionAction::Down => {
            if let Some(state) = app.completion.as_mut() {
                state.move_selection(1);
            }
        }
        CompletionAction::Accept => accept(app),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenSource {
    /// User pressed Ctrl+Space — always opens (snooze ignored, status
    /// notice on empty results).
    Manual,
    /// Auto-triggered after a keystroke. Skips snoozed positions and
    /// stays silent on empty results.
    Auto,
}

/// Wrapper around `classify` + `compute` that handles cache reads,
/// resolve context, lazy loads, and the Loading placeholder. Used by
/// both `open` and `refresh` so we don't have two slightly-different
/// versions of the same logic.
struct Computed {
    items: Vec<autocomplete::CompletionItem>,
    partial: String,
    anchor_char_offset: usize,
    needs_loads: Vec<autocomplete::TableBinding>,
}

fn compute(app: &App) -> Option<Computed> {
    if app.focus != Focus::Editor {
        return None;
    }
    let cursor = crate::state::editor::current_statement_with_cursor(&app.editor.state);
    let dialect = app
        .active_dialect
        .unwrap_or(crate::datasource::DriverKind::Sqlite);
    let cache = app.schema_cache.read().ok()?;
    let resolve = autocomplete::ResolveContext {
        default_catalog: cache.default_catalog.as_deref(),
        default_schema: cache.default_schema.as_deref(),
    };
    let result = autocomplete::classify(
        &cursor.statement,
        cursor.cursor_byte_in_stmt,
        dialect,
        resolve,
    );
    let needs_loads = bindings_needing_columns(&cache, &result);
    let mut items = autocomplete::compute(
        &result.context,
        &cache,
        &result.partial,
        &result.bindings,
        dialect,
    );
    drop(cache);

    // If column completion has nothing to show *yet* but loads are
    // pending, surface a placeholder so the user knows we're working
    // on it.
    if items.is_empty()
        && !needs_loads.is_empty()
        && matches!(
            result.context,
            autocomplete::CompletionContext::Column { .. }
                | autocomplete::CompletionContext::Mixed
        )
    {
        items.push(loading_placeholder(&needs_loads[0]));
    }

    let partial_chars = result.partial.chars().count();
    let anchor_char_offset = cursor.cursor_char_in_buffer.saturating_sub(partial_chars);
    Some(Computed {
        items,
        partial: result.partial,
        anchor_char_offset,
        needs_loads,
    })
}

fn loading_placeholder(b: &autocomplete::TableBinding) -> autocomplete::CompletionItem {
    autocomplete::CompletionItem {
        label: format!("loading {} columns…", b.table),
        kind: autocomplete::CompletionKind::Loading,
        detail: None,
        insert: String::new(),
        trail: autocomplete::InsertTrail::None,
    }
}

/// Tables referenced by `result.bindings` (or by the qualified column
/// context) whose columns aren't in the cache yet. Caller fires
/// `LoadCompletionColumns` for each so the worker fills them in.
fn bindings_needing_columns(
    cache: &autocomplete::SchemaCache,
    result: &autocomplete::ClassifyResult,
) -> Vec<autocomplete::TableBinding> {
    use autocomplete::CompletionContext;
    let candidate_bindings: Vec<&autocomplete::TableBinding> = match &result.context {
        CompletionContext::Column { qualifier: Some(b) } => vec![b],
        CompletionContext::Column { qualifier: None } | CompletionContext::Mixed => {
            result.bindings.iter().collect()
        }
        _ => return Vec::new(),
    };
    candidate_bindings
        .into_iter()
        // CTE bindings have no real (catalog, schema, table) — there's
        // nothing the worker could load for them.
        .filter(|b| !b.is_cte())
        .filter(|b| {
            !cache
                .columns
                .contains_key(&(b.catalog.clone(), b.schema.clone(), b.table.clone()))
        })
        .filter(|b| !b.catalog.is_empty() && !b.schema.is_empty() && !b.table.is_empty())
        .cloned()
        .collect()
}

fn fire_column_loads(app: &App, bindings: &[autocomplete::TableBinding]) {
    for b in bindings {
        let _ = app.cmd_tx.send(WorkerCommand::LoadCompletionColumns {
            catalog: b.catalog.clone(),
            schema: b.schema.clone(),
            table: b.table.clone(),
        });
    }
}

fn open(app: &mut App, source: OpenSource) {
    let Some(c) = compute(app) else {
        if source == OpenSource::Manual {
            app.status = QueryStatus::Notice {
                msg: "no completions here".into(),
            };
        }
        return;
    };

    // Auto-trigger respects the snooze: Esc dismisses for the current
    // partial-start. Manual Ctrl+Space ignores it (user explicitly
    // asked to reopen).
    if source == OpenSource::Auto && app.completion_snoozed_at == Some(c.anchor_char_offset) {
        return;
    }

    fire_column_loads(app, &c.needs_loads);

    if c.items.is_empty() {
        if source == OpenSource::Manual {
            app.status = QueryStatus::Notice {
                msg: "no completions here".into(),
            };
        }
        return;
    }

    app.completion_snoozed_at = None;
    app.completion = Some(crate::state::completion::CompletionState::new(
        c.items,
        c.anchor_char_offset,
        c.partial,
    ));
}

fn close(app: &mut App, manual: bool) {
    if manual && let Some(state) = &app.completion {
        app.completion_snoozed_at = Some(state.anchor_offset);
    }
    app.completion = None;
}

fn accept(app: &mut App) {
    let Some(state) = app.completion.take() else {
        return;
    };
    let Some(item) = state.items.get(state.selected) else {
        return;
    };
    // Loading placeholders are decorative — a user pressing Enter on
    // one shouldn't mangle the buffer. Drop accept and reopen so the
    // popover refreshes once the load lands.
    if item.kind == autocomplete::CompletionKind::Loading {
        app.completion = Some(state);
        return;
    }
    let dialect = app
        .active_dialect
        .unwrap_or(crate::datasource::DriverKind::Sqlite);
    // Keywords / functions / loading items go in as displayed (the
    // engine already shaped `insert` correctly); identifier kinds get
    // dialect-quoted if the name needs it.
    use autocomplete::CompletionKind;
    let to_insert = match item.kind {
        CompletionKind::Keyword | CompletionKind::Function | CompletionKind::Loading => {
            item.insert.clone()
        }
        CompletionKind::Table
        | CompletionKind::View
        | CompletionKind::Column
        | CompletionKind::Cte => autocomplete::insert::quote_if_needed(&item.insert, dialect),
    };
    autocomplete::insert::apply_completion(
        &mut app.editor.state,
        state.anchor_offset,
        &to_insert,
        item.trail,
    );
    schedule_session_save(app);
}

/// Recompute the popover after each edit. Closes it if the cursor
/// drifted out of the original token, or if the new partial yields no
/// candidates. Otherwise updates `partial` + `items` + clamps `selected`.
pub fn refresh(app: &mut App) {
    if app.completion.is_none() {
        return;
    }
    let Some(c) = compute(app) else {
        app.completion = None;
        return;
    };
    let anchor_changed = app
        .completion
        .as_ref()
        .map(|s| s.anchor_offset != c.anchor_char_offset)
        .unwrap_or(true);
    if anchor_changed {
        app.completion = None;
        // Different word — drop any prior snooze.
        app.completion_snoozed_at = None;
        return;
    }
    fire_column_loads(app, &c.needs_loads);
    if c.items.is_empty() {
        app.completion = None;
        return;
    }
    if let Some(state) = app.completion.as_mut() {
        state.partial = c.partial;
        state.replace_items(c.items);
    }
}

/// Auto-trigger heuristic: open the popover after a keystroke when the
/// user just typed `.` or has 2+ identifier chars in the current
/// partial. Insert mode + editor focus only; respects the snooze flag.
pub fn maybe_auto_trigger(app: &mut App) {
    if app.completion.is_some() {
        return;
    }
    if app.focus != Focus::Editor {
        return;
    }
    if app.editor.editor_mode() != edtui::EditorMode::Insert {
        return;
    }
    let cursor = crate::state::editor::current_statement_with_cursor(&app.editor.state);
    let prefix = &cursor.statement[..cursor.cursor_byte_in_stmt];
    let just_after_dot = prefix.ends_with('.');
    let partial_len = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .count();
    if !just_after_dot && partial_len < 2 {
        return;
    }
    open(app, OpenSource::Auto);
}
