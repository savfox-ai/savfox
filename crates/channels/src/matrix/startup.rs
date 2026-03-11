use std::path::PathBuf;

use tracing::info;

use super::config::load_matrix_channel_configs;

pub async fn log_configured_matrix_startup(savfox_home: &PathBuf) -> anyhow::Result<()> {
    for config in load_matrix_channel_configs(savfox_home).await? {
        info!(
            channel_id = %config.id,
            homeserver = %config.homeserver,
            "Matrix channel starting with homeserver URL: {}",
            config.homeserver
        );
    }
    Ok(())
}
