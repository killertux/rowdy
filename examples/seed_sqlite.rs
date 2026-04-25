//! Creates a fresh SQLite database with a small e-commerce schema and seed data
//! you can poke at with `rowdy`.
//!
//! Usage:
//!   cargo run --example seed_sqlite -- [path]
//!
//! `path` defaults to `sample.db` in the current directory. Existing tables
//! are dropped before being re-created, so re-running is safe.
//!
//! After it finishes:
//!   cargo run -- --connection sqlite:./sample.db

use anyhow::Result;
use sqlx::ConnectOptions as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.db".into());
    let url = format!("sqlite:{path}");

    let options = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .disable_statement_logging();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    drop_objects(&pool).await?;
    create_schema(&pool).await?;
    seed_data(&pool).await?;

    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await?;
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders")
        .fetch_one(&pool)
        .await?;
    let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM order_items")
        .fetch_one(&pool)
        .await?;

    pool.close().await;

    println!("seeded {path}");
    println!("  users:       {user_count}");
    println!("  orders:      {order_count}");
    println!("  order_items: {item_count}");
    println!("\ntry:");
    println!("  cargo run -- --connection sqlite:{path}");
    Ok(())
}

async fn drop_objects(pool: &sqlx::SqlitePool) -> Result<()> {
    for stmt in [
        "DROP VIEW IF EXISTS recent_orders",
        "DROP TABLE IF EXISTS order_items",
        "DROP TABLE IF EXISTS orders",
        "DROP TABLE IF EXISTS products",
        "DROP TABLE IF EXISTS users",
    ] {
        sqlx::query(stmt).execute(pool).await?;
    }
    Ok(())
}

async fn create_schema(pool: &sqlx::SqlitePool) -> Result<()> {
    let schema = r#"
        CREATE TABLE users (
            id          INTEGER PRIMARY KEY,
            name        TEXT    NOT NULL,
            email       TEXT    NOT NULL UNIQUE,
            is_active   INTEGER NOT NULL DEFAULT 1,
            created_at  TEXT    NOT NULL
        );
        CREATE INDEX users_email_idx ON users(email);

        CREATE TABLE products (
            id           INTEGER PRIMARY KEY,
            name         TEXT    NOT NULL,
            category     TEXT    NOT NULL,
            price_cents  INTEGER NOT NULL,
            stock        INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX products_category_idx ON products(category);

        CREATE TABLE orders (
            id          INTEGER PRIMARY KEY,
            user_id     INTEGER NOT NULL REFERENCES users(id),
            status      TEXT    NOT NULL,
            total_cents INTEGER NOT NULL,
            ordered_at  TEXT    NOT NULL
        );
        CREATE INDEX orders_user_idx ON orders(user_id);

        CREATE TABLE order_items (
            id          INTEGER PRIMARY KEY,
            order_id    INTEGER NOT NULL REFERENCES orders(id),
            product_id  INTEGER NOT NULL REFERENCES products(id),
            quantity    INTEGER NOT NULL,
            price_cents INTEGER NOT NULL
        );
        CREATE INDEX order_items_order_idx ON order_items(order_id);

        CREATE VIEW recent_orders AS
            SELECT o.id, u.name AS user_name, o.status, o.total_cents, o.ordered_at
              FROM orders o
              JOIN users  u ON u.id = o.user_id
             ORDER BY o.ordered_at DESC;
    "#;
    for stmt in schema.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pool).await?;
    }
    Ok(())
}

async fn seed_data(pool: &sqlx::SqlitePool) -> Result<()> {
    seed_users(pool).await?;
    seed_products(pool).await?;
    seed_orders(pool).await?;
    seed_order_items(pool).await?;
    Ok(())
}

async fn seed_users(pool: &sqlx::SqlitePool) -> Result<()> {
    let names = [
        "Ada Lovelace",
        "Alan Turing",
        "Grace Hopper",
        "Linus Torvalds",
        "Margaret Hamilton",
        "Donald Knuth",
        "Edsger Dijkstra",
        "Barbara Liskov",
        "Tony Hoare",
        "Niklaus Wirth",
        "Bjarne Stroustrup",
        "Brian Kernighan",
        "Dennis Ritchie",
        "Ken Thompson",
        "Rob Pike",
        "Anders Hejlsberg",
        "Yukihiro Matsumoto",
        "Guido van Rossum",
        "Larry Wall",
        "James Gosling",
    ];
    for (i, name) in names.iter().enumerate() {
        let email = format!("{}@example.com", name.to_lowercase().replace(' ', "."));
        let is_active = if i % 7 == 0 { 0 } else { 1 };
        let created_at = format!("2024-{:02}-{:02}T09:00:00Z", (i % 12) + 1, (i % 28) + 1);
        sqlx::query("INSERT INTO users(name, email, is_active, created_at) VALUES (?, ?, ?, ?)")
            .bind(name)
            .bind(&email)
            .bind(is_active)
            .bind(&created_at)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn seed_products(pool: &sqlx::SqlitePool) -> Result<()> {
    let products = [
        ("USB-C Hub", "electronics", 3499, 42),
        ("Mechanical Keyboard", "electronics", 12900, 17),
        ("Wireless Mouse", "electronics", 4900, 88),
        ("4K Monitor", "electronics", 44900, 9),
        ("Standing Desk", "furniture", 59900, 4),
        ("Office Chair", "furniture", 32900, 11),
        ("Desk Lamp", "furniture", 7500, 23),
        ("Notebook A5", "stationery", 995, 300),
        ("Gel Pen 5-pack", "stationery", 1299, 180),
        ("Sticky Notes", "stationery", 699, 250),
        ("Coffee Beans 1kg", "kitchen", 2899, 60),
        ("French Press", "kitchen", 2499, 35),
        ("Travel Mug", "kitchen", 1899, 75),
    ];
    for (name, category, price, stock) in products {
        sqlx::query("INSERT INTO products(name, category, price_cents, stock) VALUES (?, ?, ?, ?)")
            .bind(name)
            .bind(category)
            .bind(price)
            .bind(stock)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn seed_orders(pool: &sqlx::SqlitePool) -> Result<()> {
    let statuses = ["pending", "paid", "shipped", "delivered", "refunded"];
    for i in 0..120i64 {
        let user_id = (i % 20) + 1;
        let status = statuses[(i as usize) % statuses.len()];
        let total = 1000 + (i * 137) % 50_000;
        let day = (i % 28) + 1;
        let month = (i % 12) + 1;
        let ordered_at = format!("2025-{month:02}-{day:02}T{:02}:00:00Z", (i % 24));
        sqlx::query(
            "INSERT INTO orders(user_id, status, total_cents, ordered_at) VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(status)
        .bind(total)
        .bind(&ordered_at)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn seed_order_items(pool: &sqlx::SqlitePool) -> Result<()> {
    let order_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM orders")
        .fetch_all(pool)
        .await?;
    let product_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM products")
        .fetch_all(pool)
        .await?;

    let mut counter: i64 = 0;
    for order_id in order_ids {
        let line_count = (order_id % 4) + 1;
        for line in 0..line_count {
            counter += 1;
            let product_id = product_ids[(counter as usize) % product_ids.len()];
            let quantity = ((line + 1) * 2) - (line / 2);
            let price = 500 + (counter * 73) % 30_000;
            sqlx::query(
                "INSERT INTO order_items(order_id, product_id, quantity, price_cents) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(order_id)
            .bind(product_id)
            .bind(quantity)
            .bind(price)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
