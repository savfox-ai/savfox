pub mod cache;
pub mod collaboration_mode_presets;
pub mod manager;
pub mod model_info;
pub mod model_presets;

#[cfg(any(test, feature = "test-support"))]
pub use collaboration_mode_presets::test_builtin_collaboration_mode_presets;

/// Client version sent with `/models` requests.
///
/// We use `0.0.0` (matching codex) so the server does not filter out newer
/// models based on `minimal_client_version`.
#[must_use]
pub fn client_version_to_whole() -> String {
    "0.0.0".to_owned()
}
