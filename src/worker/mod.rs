// Most variants and fields below are sent or read once Chunk B wires up the
// schema introspection and cancel paths. Module-level allow is the cleanest
// way to mark this stubbed-but-stable surface.
#![allow(dead_code)]

pub mod request;

use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::datasource::{
    CatalogInfo, ColumnInfo, Datasource, DatasourceError, IndexInfo, QueryResult, SchemaInfo,
    TableInfo,
};
pub use request::{RequestCounter, RequestId};

#[derive(Debug)]
pub enum WorkerCommand {
    Execute {
        req: RequestId,
        sql: String,
    },
    Cancel,
    Introspect {
        req: RequestId,
        target: IntrospectTarget,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntrospectTarget {
    Catalogs,
    Schemas {
        catalog: String,
    },
    Tables {
        catalog: String,
        schema: String,
    },
    Columns {
        catalog: String,
        schema: String,
        table: String,
    },
    Indices {
        catalog: String,
        schema: String,
        table: String,
    },
}

#[derive(Debug)]
pub enum WorkerEvent {
    QueryDone {
        req: RequestId,
        result: QueryResult,
    },
    QueryFailed {
        req: RequestId,
        error: DatasourceError,
    },
    SchemaLoaded {
        req: RequestId,
        target: IntrospectTarget,
        payload: SchemaPayload,
    },
    SchemaFailed {
        req: RequestId,
        target: IntrospectTarget,
        error: DatasourceError,
    },
}

#[derive(Debug)]
pub enum SchemaPayload {
    Catalogs(Vec<CatalogInfo>),
    Schemas(Vec<SchemaInfo>),
    Tables(Vec<TableInfo>),
    Columns(Vec<ColumnInfo>),
    Indices(Vec<IndexInfo>),
}

/// Runs until either the command channel closes or `Close` is received.
pub async fn run(
    datasource: Box<dyn Datasource>,
    mut commands: UnboundedReceiver<WorkerCommand>,
    events: UnboundedSender<WorkerEvent>,
) {
    let datasource: Arc<dyn Datasource> = Arc::from(datasource);
    let mut current_query: Option<JoinHandle<()>> = None;

    while let Some(cmd) = commands.recv().await {
        match cmd {
            WorkerCommand::Close => break,
            WorkerCommand::Cancel => cancel_query(&datasource, &mut current_query),
            WorkerCommand::Execute { req, sql } => {
                spawn_query(&datasource, &events, &mut current_query, req, sql);
            }
            WorkerCommand::Introspect { req, target } => {
                spawn_introspect(&datasource, &events, req, target);
            }
        }
    }
}

fn cancel_query(datasource: &Arc<dyn Datasource>, current: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = current.take() {
        handle.abort();
    }
    let datasource = datasource.clone();
    tokio::spawn(async move {
        let _ = datasource.cancel().await;
    });
}

fn spawn_query(
    datasource: &Arc<dyn Datasource>,
    events: &UnboundedSender<WorkerEvent>,
    current: &mut Option<JoinHandle<()>>,
    req: RequestId,
    sql: String,
) {
    if let Some(prev) = current.take() {
        prev.abort();
    }
    let datasource = datasource.clone();
    let events = events.clone();
    *current = Some(tokio::spawn(async move {
        let event = handle_execute(datasource.as_ref(), req, sql).await;
        let _ = events.send(event);
    }));
}

fn spawn_introspect(
    datasource: &Arc<dyn Datasource>,
    events: &UnboundedSender<WorkerEvent>,
    req: RequestId,
    target: IntrospectTarget,
) {
    let datasource = datasource.clone();
    let events = events.clone();
    tokio::spawn(async move {
        let event = handle_introspect(datasource.as_ref(), req, target).await;
        let _ = events.send(event);
    });
}

async fn handle_execute(datasource: &dyn Datasource, req: RequestId, sql: String) -> WorkerEvent {
    match datasource.execute(&sql).await {
        Ok(result) => WorkerEvent::QueryDone { req, result },
        Err(error) => WorkerEvent::QueryFailed { req, error },
    }
}

async fn handle_introspect(
    datasource: &dyn Datasource,
    req: RequestId,
    target: IntrospectTarget,
) -> WorkerEvent {
    let outcome = match &target {
        IntrospectTarget::Catalogs => datasource
            .introspect_catalogs()
            .await
            .map(SchemaPayload::Catalogs),
        IntrospectTarget::Schemas { catalog } => datasource
            .introspect_schemas(catalog)
            .await
            .map(SchemaPayload::Schemas),
        IntrospectTarget::Tables { catalog, schema } => datasource
            .introspect_tables(catalog, schema)
            .await
            .map(SchemaPayload::Tables),
        IntrospectTarget::Columns {
            catalog,
            schema,
            table,
        } => datasource
            .introspect_columns(catalog, schema, table)
            .await
            .map(SchemaPayload::Columns),
        IntrospectTarget::Indices {
            catalog,
            schema,
            table,
        } => datasource
            .introspect_indices(catalog, schema, table)
            .await
            .map(SchemaPayload::Indices),
    };
    match outcome {
        Ok(payload) => WorkerEvent::SchemaLoaded {
            req,
            target,
            payload,
        },
        Err(error) => WorkerEvent::SchemaFailed { req, target, error },
    }
}
