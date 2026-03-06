use std::path::PathBuf;

use serde_json::Value;
use tracing::info;

fn first_non_empty_config_string(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
    })
}

pub async fn log_configured_matrix_startup(savfox_home: &PathBuf) -> anyhow::Result<()> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;
    for config in all_configs {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("matrix") {
            continue;
        }

        let homeserver = config
            .config
            .as_object()
            .and_then(|raw| {
                first_non_empty_config_string(raw, &["homeserver", "homeserver_url", "server_url"])
            })
            .unwrap_or_else(|| "https://matrix.org".to_string());

        info!(
            channel_id = %config.id,
            homeserver = %homeserver,
            "Matrix bridge starting with homeserver URL: {homeserver}"
        );
    }
    Ok(())
}
