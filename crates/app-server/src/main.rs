use std::path::PathBuf;

use savfox_app_server::run_main;
use savfox_arg0::arg0_dispatch_or_else;
use savfox_common::CliConfigOverrides;
use savfox_core::config_loader::LoaderOverrides;

// Debug-only test hook: lets integration tests point the server at a temporary
// managed config file without writing to /etc.
const MANAGED_CONFIG_PATH_ENV_VAR: &str = "SAVFOX_APP_SERVER_MANAGED_CONFIG_PATH";

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|savfox_linux_sandbox_exe| async move {
        let managed_config_path = managed_config_path_from_debug_env();
        let loader_overrides = LoaderOverrides {
            managed_config_path,
            ..Default::default()
        };

        run_main(
            savfox_linux_sandbox_exe,
            CliConfigOverrides::default(),
            loader_overrides,
            false,
        )
        .await?;
        Ok(())
    })
}

fn managed_config_path_from_debug_env() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        if let Ok(value) = std::env::var(MANAGED_CONFIG_PATH_ENV_VAR) {
            return if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            };
        }
    }

    None
}
