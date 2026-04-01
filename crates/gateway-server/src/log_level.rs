use std::sync::OnceLock;

use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::reload::Handle;

type ReloadHandle = Handle<EnvFilter, tracing_subscriber::Registry>;

static RELOAD_HANDLE: OnceLock<ReloadHandle> = OnceLock::new();

/// Store the reload handle so it can be used later to change the log level at
/// runtime.
pub(crate) fn set_reload_handle(handle: ReloadHandle) {
    let _ = RELOAD_HANDLE.set(handle);
}

/// Get the current effective log filter string.
pub(crate) fn get_level() -> Result<Value, String> {
    let handle = RELOAD_HANDLE
        .get()
        .ok_or_else(|| "log reload handle not initialized".to_string())?;
    let current = handle
        .with_current(|filter| filter.to_string())
        .map_err(|e| e.to_string())?;
    Ok(json!({ "level": current }))
}

/// Set the log filter at runtime.
///
/// Accepts standard `RUST_LOG` syntax, e.g.:
/// - `"debug"`
/// - `"info"`
/// - `"warn"`
/// - `"savfox_gateway_server=debug,info"`
pub(crate) fn set_level(filter: &str) -> Result<Value, String> {
    let handle = RELOAD_HANDLE
        .get()
        .ok_or_else(|| "log reload handle not initialized".to_string())?;
    let new_filter = filter
        .parse::<EnvFilter>()
        .map_err(|e| format!("invalid filter '{filter}': {e}"))?;
    handle.reload(new_filter).map_err(|e| e.to_string())?;
    tracing::info!(filter = %filter, "log level changed");
    Ok(json!({ "level": filter }))
}
