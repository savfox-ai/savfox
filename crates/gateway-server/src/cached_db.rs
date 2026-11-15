//! Global SQLite cache stored at `{savfox_home}/gateway/cached_data.sqlite`.
//!
//! This is a **rebuildable cache** — all data can be reconstructed from disk
//! manifests or external sources. The current schema is initialized on startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use savfox_utils::home_dir::GATEWAY_SUBDIR;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

static DB_POOLS: OnceLock<Mutex<HashMap<PathBuf, SqlitePool>>> = OnceLock::new();

/// Return (or create) a [`SqlitePool`] for the given `savfox_home`.
///
/// The pool is cached per canonical DB path so subsequent calls are cheap.
/// On first access the database file is created and [`init_schema`] is run.
pub(crate) async fn get_pool(savfox_home: &Path) -> Result<SqlitePool, String> {
    let pools = DB_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let db_dir = savfox_home.join(GATEWAY_SUBDIR);
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
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(|e| format!("sqlite connect: {e}"))?;
    init_schema(&pool).await?;

    let mut guard = pools.lock().await;
    guard.insert(db_path, pool.clone());
    Ok(pool)
}

/// Create the current cache tables (skills, skill_roots, skill_env).
async fn init_schema(pool: &SqlitePool) -> Result<(), String> {
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

#[cfg(test)]
mod path_tests {
    use super::*;
    #[tokio::test]
    async fn cache_filename_is_a_native_path_including_spaces_and_uri_characters() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("cache # 设置");
        let pool = get_pool(&path).await.unwrap();
        assert!(
            path.join(GATEWAY_SUBDIR)
                .join("cached_data.sqlite")
                .is_file()
        );
        let (tables,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('skills', 'skill_roots', 'skill_env')",
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(tables, 3);
        pool.close().await;
    }
}
