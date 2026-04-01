use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Load a JSON file, returning `T::default()` if the file doesn't exist.
pub(crate) async fn load_json<T: DeserializeOwned + Default>(
    path: &Path,
    context: &str,
) -> Result<T, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content)
            .map_err(|err| format!("failed to parse {context}: {err}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(format!("failed to read {context}: {err}")),
    }
}

/// Load a JSON file, returning `None` if the file doesn't exist.
pub(crate) async fn load_json_opt<T: DeserializeOwned>(
    path: &Path,
    context: &str,
) -> Result<Option<T>, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content)
            .map(Some)
            .map_err(|err| format!("failed to parse {context}: {err}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("failed to read {context}: {err}")),
    }
}

/// Save a value as pretty-printed JSON, creating parent directories as needed.
pub(crate) async fn save_json<T: Serialize + ?Sized>(
    path: &Path,
    data: &T,
    context: &str,
) -> Result<(), String> {
    ensure_parent_dir(path).await?;
    let content = serde_json::to_string_pretty(data)
        .map_err(|err| format!("failed to serialize {context}: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write {context}: {err}"))
}

/// Create parent directories for a path, if they don't exist.
pub(crate) async fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create directory {}: {err}", parent.display()))?;
    }
    Ok(())
}

/// Load a JSON file as a `serde_json::Value`, defaulting to `{}` on any error.
pub(crate) async fn load_json_value(path: &Path) -> serde_json::Value {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or(serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    }
}

/// Current time in milliseconds since UNIX epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
