//! Global SQLite cache stored at `{savfox_home}/gateway/cached_data.sqlite`.
//!
//! This is a **rebuildable cache** — all data can be reconstructed from disk
//! manifests or external sources.  Schema changes are tracked via a
//! `schema_version` table and applied incrementally on startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::info;

static DB_POOLS: OnceLock<Mutex<HashMap<PathBuf, SqlitePool>>> = OnceLock::new();

/// Current schema version — bump this when adding new migrations.
const SCHEMA_VERSION: u32 = 1;

/// Return (or create) a [`SqlitePool`] for the given `savfox_home`.
///
/// The pool is cached per canonical DB path so subsequent calls are cheap.
/// On first access the database file is created and [`init_schema`] is run.
pub(crate) async fn get_pool(savfox_home: &Path) -> Result<SqlitePool, String> {
    let pools = DB_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let db_dir = savfox_home.join("gateway");
    let db_path = db_dir.join("cached_data.sqlite");

    {
        let guard = pools.lock().await;
        if let Some(pool) = guard.get(&db_path) {
            return Ok(pool.clone());
        }
    }

    tokio::fs::create_dir_all(&db_dir)
        .await
        .map_err(|e| format!("create gateway dir: {e}"))?;
    let url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("sqlite connect: {e}"))?;
    run_migrations(&pool).await?;

    let mut guard = pools.lock().await;
    guard.insert(db_path, pool.clone());
    Ok(pool)
}

/// Read the current schema version from the database (0 if table doesn't exist).
async fn get_current_version(pool: &SqlitePool) -> Result<u32, String> {
    // Ensure version table exists.
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL DEFAULT 0)")
        .execute(pool)
        .await
        .map_err(|e| format!("create schema_version table: {e}"))?;

    let row: Option<(i32,)> = sqlx::query_as("SELECT version FROM schema_version LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read schema version: {e}"))?;

    Ok(row.map(|(v,)| v as u32).unwrap_or(0))
}

/// Set the stored schema version.
async fn set_version(pool: &SqlitePool, version: u32) -> Result<(), String> {
    sqlx::query("DELETE FROM schema_version")
        .execute(pool)
        .await
        .map_err(|e| format!("clear schema_version: {e}"))?;
    sqlx::query("INSERT INTO schema_version (version) VALUES (?1)")
        .bind(version as i32)
        .execute(pool)
        .await
        .map_err(|e| format!("set schema_version: {e}"))?;
    Ok(())
}

/// Run all pending migrations up to [`SCHEMA_VERSION`].
async fn run_migrations(pool: &SqlitePool) -> Result<(), String> {
    let current = get_current_version(pool).await?;

    if current >= SCHEMA_VERSION {
        return Ok(());
    }

    // Migration 0 → 1: initial schema.
    if current < 1 {
        migration_v1(pool).await?;
    }

    // Future migrations go here:
    // if current < 2 { migration_v2(pool).await?; }

    set_version(pool, SCHEMA_VERSION).await?;
    info!(
        from = current,
        to = SCHEMA_VERSION,
        "gateway cache schema migrated"
    );
    Ok(())
}

/// V1: initial table creation (skills, skill_roots, skill_env).
async fn migration_v1(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS skills (
            name              TEXT PRIMARY KEY,
            description       TEXT,
            version           TEXT,
            category          TEXT NOT NULL DEFAULT '',
            path              TEXT NOT NULL DEFAULT '',
            enabled           INTEGER NOT NULL DEFAULT 1,
            installed         INTEGER NOT NULL DEFAULT 0,
            eligible          INTEGER NOT NULL DEFAULT 1,
            flock             TEXT,
            primary_env       TEXT,
            env_set           INTEGER,
            allowlist_blocked INTEGER NOT NULL DEFAULT 0,
            missing_deps      TEXT,
            disabled_reason   TEXT,
            updated_at        INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("create skills table: {e}"))?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_skills_category ON skills(category);")
        .execute(pool)
        .await
        .map_err(|e| format!("create skills index: {e}"))?;

    sqlx::query("CREATE TABLE IF NOT EXISTS skill_roots (path TEXT PRIMARY KEY);")
        .execute(pool)
        .await
        .map_err(|e| format!("create skill_roots table: {e}"))?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS skill_env (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .execute(pool)
    .await
    .map_err(|e| format!("create skill_env table: {e}"))?;

    Ok(())
}
