//! Runs the same introspection queries the SQLite driver uses, against a
//! real database file, so we can see what SQLite actually returns.
//!
//!   cargo run --example probe_sqlite -- ./sample.db

use anyhow::Result;
use sqlx::Row;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.db".into());
    let url = format!("sqlite:{path}");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;

    println!("== schemas ==");
    let rows = sqlx::query("SELECT name FROM pragma_database_list ORDER BY seq")
        .fetch_all(&pool)
        .await?;
    for r in &rows {
        let name: String = r.try_get("name")?;
        println!("  {name}");
    }

    let schema = "main";

    println!("\n== tables in {schema} ==");
    let sql = format!(
        "SELECT name, type FROM \"{schema}\".sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
         ORDER BY name"
    );
    let tables = sqlx::query(&sql).fetch_all(&pool).await?;
    let table_names: Vec<String> = tables
        .iter()
        .filter_map(|r| r.try_get::<String, _>("name").ok())
        .collect();
    for n in &table_names {
        println!("  {n}");
    }

    for table in &table_names {
        println!("\n== columns of {schema}.{table} ==");
        let res = sqlx::query("SELECT name, type, \"notnull\" FROM pragma_table_info(?, ?)")
            .bind(table)
            .bind(schema)
            .fetch_all(&pool)
            .await;
        match res {
            Ok(rows) => {
                if rows.is_empty() {
                    println!("  (empty result)");
                }
                for r in rows {
                    let name: String = r.try_get("name")?;
                    let ty: String = r.try_get("type")?;
                    let notnull: i64 = r.try_get("notnull")?;
                    println!("  {name}  {ty}  notnull={notnull}");
                }
            }
            Err(e) => println!("  ERROR: {e}"),
        }

        println!("== indices of {schema}.{table} ==");
        let res = sqlx::query("SELECT name, \"unique\" FROM pragma_index_list(?, ?)")
            .bind(table)
            .bind(schema)
            .fetch_all(&pool)
            .await;
        match res {
            Ok(rows) => {
                if rows.is_empty() {
                    println!("  (empty result)");
                }
                for r in rows {
                    let name: String = r.try_get("name")?;
                    let unique: i64 = r.try_get("unique")?;
                    println!("  {name}  unique={unique}");
                }
            }
            Err(e) => println!("  ERROR: {e}"),
        }
    }

    pool.close().await;
    Ok(())
}
