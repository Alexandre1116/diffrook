use anyhow::Context;
use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::{str::FromStr, time::Duration};

pub async fn open(path: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.replace('\\', "/")))?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await?;
    for sql in [
        "CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE COLLATE NOCASE, password_hash TEXT NOT NULL, created_at TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, expires_at INTEGER NOT NULL)",
        "CREATE TABLE IF NOT EXISTS objects (kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(kind,id))",
        "CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, body TEXT NOT NULL, created_at TEXT NOT NULL, status TEXT NOT NULL)",
        "CREATE INDEX IF NOT EXISTS runs_created ON runs(created_at DESC)",
        "CREATE TABLE IF NOT EXISTS webhook_deliveries (connection_id TEXT NOT NULL, delivery_id TEXT NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(connection_id,delivery_id))",
        "CREATE TABLE IF NOT EXISTS scheduler_marks (automation_id TEXT PRIMARY KEY, last_slot TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS scheduler_marks_v2 (automation_id TEXT NOT NULL, repository TEXT NOT NULL, last_slot TEXT NOT NULL, PRIMARY KEY(automation_id,repository))",
        "CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    ] {
        sqlx::query(sql).execute(&pool).await.context("initialize SQLite schema")?;
    }
    Ok(pool)
}

pub async fn object(pool: &SqlitePool, kind: &str, id: &str) -> anyhow::Result<Option<Value>> {
    let row = sqlx::query_scalar::<_, String>("SELECT body FROM objects WHERE kind=? AND id=?")
        .bind(kind)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(|s| serde_json::from_str(&s).context("parse stored object"))
        .transpose()
}

pub async fn objects(pool: &SqlitePool, kind: &str) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT body FROM objects WHERE kind=? ORDER BY created_at DESC",
    )
    .bind(kind)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).context("parse stored object"))
        .collect()
}

pub async fn save_object(pool: &SqlitePool, kind: &str, value: &Value) -> anyhow::Result<()> {
    let id = value["id"].as_str().context("object id missing")?;
    let created = value["created_at"].as_str().context("created_at missing")?;
    let updated = value["updated_at"].as_str().context("updated_at missing")?;
    sqlx::query("INSERT INTO objects(kind,id,body,created_at,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,updated_at=excluded.updated_at")
        .bind(kind).bind(id).bind(serde_json::to_string(value)?).bind(created).bind(updated).execute(pool).await?;
    Ok(())
}

pub async fn delete_object(pool: &SqlitePool, kind: &str, id: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query("DELETE FROM objects WHERE kind=? AND id=?")
        .bind(kind)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected()
        > 0)
}
