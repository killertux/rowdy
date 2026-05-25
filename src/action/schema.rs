//! Schema panel (tree navigation + width persistence), background
//! introspection plumbing, and the autocomplete-cache event sinks.

use crate::app::{App, MAX_SCHEMA_WIDTH, MIN_SCHEMA_WIDTH};
use crate::state::schema::{ExpandOutcome, NodeId};
use crate::state::status::QueryStatus;
use crate::worker::{IntrospectTarget, WorkerCommand};

use super::{SchemaAction, chat, completion};

pub(super) fn schema_toggle_at(app: &mut App, id: NodeId) {
    app.schema.selected = Some(id);
    let outcome = app.schema.toggle_selected();
    maybe_dispatch(app, outcome);
}

pub(super) fn schema_scroll(app: &mut App, delta: i32) {
    let total = app.schema.visible_rows().len();
    if total == 0 {
        return;
    }
    app.schema.snap_to_selection = false;
    let max_offset = total.saturating_sub(1);
    let next = (app.schema.scroll_offset as i32).saturating_add(delta);
    let next = next.clamp(0, max_offset as i32) as usize;
    app.schema.scroll_offset = next;
}

pub(super) fn reload_schema_cache(app: &mut App) {
    let Some(name) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    let _ = app.cmd_tx.send(WorkerCommand::Reload { connection: name });
    app.status = QueryStatus::Notice {
        msg: "reloading schema cache…".into(),
    };
}

pub(super) fn resize_schema(app: &mut App, delta: i16) {
    let next = (app.schema.width as i32 + delta as i32)
        .clamp(MIN_SCHEMA_WIDTH as i32, MAX_SCHEMA_WIDTH as i32);
    app.schema.width = next as u16;
    persist_schema_width(app);
}

pub(super) fn set_schema_width(app: &mut App, value: u16) {
    app.schema.width = value.clamp(MIN_SCHEMA_WIDTH, MAX_SCHEMA_WIDTH);
    persist_schema_width(app);
}

fn persist_schema_width(app: &mut App) {
    if let Err(err) = app.config.set_schema_width(app.schema.width) {
        app.log
            .warn("config", format!("save schema_width failed: {err}"));
    }
}

pub(super) fn apply_schema(app: &mut App, action: SchemaAction) {
    match action {
        SchemaAction::Down => app.schema.move_selection(1),
        SchemaAction::Up => app.schema.move_selection(-1),
        SchemaAction::ExpandOrDescend => {
            let outcome = app.schema.expand_or_descend();
            maybe_dispatch(app, outcome);
        }
        SchemaAction::CollapseOrAscend => app.schema.collapse_or_ascend(),
        SchemaAction::Toggle => {
            let outcome = app.schema.toggle_selected();
            maybe_dispatch(app, outcome);
        }
        SchemaAction::Top => app.schema.select_first(),
        SchemaAction::Bottom => app.schema.select_last(),
    }
}

fn maybe_dispatch(app: &mut App, outcome: ExpandOutcome) {
    if let ExpandOutcome::Dispatch(targets) = outcome {
        for target in targets {
            dispatch_introspect(app, target);
        }
    }
}

fn dispatch_introspect(app: &mut App, target: IntrospectTarget) {
    let _ = app.cmd_tx.send(WorkerCommand::Introspect { target });
}

pub(super) fn on_cache_stage(app: &mut App, stage: crate::worker::CacheStage) {
    use crate::worker::CacheStage;
    if matches!(stage, CacheStage::Reloaded) {
        app.schema_reload_in_flight = false;
        app.status = QueryStatus::Notice {
            msg: "schema cache reloaded".into(),
        };
    }
    // Columns just landed — if the popover is currently waiting on
    // them (likely showing a "loading…" placeholder), recompute.
    if matches!(stage, CacheStage::Columns { .. }) && app.completion.is_some() {
        completion::refresh(app);
    }
}

pub(super) fn on_cache_failed(app: &mut App, stage: crate::worker::CacheStage, error: String) {
    app.log.warn(
        "autocomplete",
        format!("cache load failed at {stage:?}: {error}"),
    );
}

pub(super) fn on_schema_loaded(
    app: &mut App,
    target: IntrospectTarget,
    payload: crate::worker::SchemaPayload,
) {
    use crate::worker::SchemaPayload;
    if let Ok(mut guard) = app.schema_cache.write() {
        cache_introspect_payload(&mut guard, &target, &payload);
    }
    match payload {
        SchemaPayload::Catalogs(catalogs) => app.schema.populate_catalogs(catalogs),
        other => app.schema.populate(&target, other),
    }
    chat::complete_pending_for_target(app, &target, None);
}

pub(super) fn on_schema_failed(app: &mut App, target: IntrospectTarget, error: String) {
    if matches!(target, IntrospectTarget::Catalogs) {
        app.schema.fail_root_load(error.clone());
    } else {
        app.schema.record_failure(&target, error.clone());
    }
    chat::complete_pending_for_target(app, &target, Some(error));
}

/// Mirror an introspection result into the autocomplete `SchemaCache`.
/// `worker::prime_cache` and `worker::load_columns` already do this for
/// the cache-prime / lazy-column paths; the chat auto-expand path
/// reaches the cache through here instead so a schema tool that
/// triggered the introspect can re-run against fresh data.
fn cache_introspect_payload(
    cache: &mut crate::autocomplete::SchemaCache,
    target: &IntrospectTarget,
    payload: &crate::worker::SchemaPayload,
) {
    use crate::autocomplete::cache::{CachedColumn, CachedTable};
    use crate::worker::SchemaPayload;
    match (target, payload) {
        (IntrospectTarget::Catalogs, SchemaPayload::Catalogs(catalogs)) => {
            cache.catalogs = catalogs.iter().map(|c| c.name.clone()).collect();
        }
        (IntrospectTarget::Schemas { catalog }, SchemaPayload::Schemas(schemas)) => {
            cache.schemas.insert(
                catalog.clone(),
                schemas.iter().map(|s| s.name.clone()).collect(),
            );
        }
        (IntrospectTarget::Tables { catalog, schema }, SchemaPayload::Tables(tables)) => {
            let cached: Vec<CachedTable> = tables
                .iter()
                .map(|t| CachedTable {
                    name: t.name.clone(),
                    kind: t.kind,
                })
                .collect();
            cache
                .tables
                .insert((catalog.clone(), schema.clone()), cached);
        }
        (
            IntrospectTarget::Columns {
                catalog,
                schema,
                table,
            },
            SchemaPayload::Columns(columns),
        ) => {
            let cached: Vec<CachedColumn> = columns
                .iter()
                .map(|c| CachedColumn {
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                })
                .collect();
            cache
                .columns
                .insert((catalog.clone(), schema.clone(), table.clone()), cached);
        }
        // Indices aren't in the cache and aren't surfaced as a tool.
        _ => {}
    }
}
