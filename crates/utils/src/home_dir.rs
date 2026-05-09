use std::path::PathBuf;

use dirs::home_dir;

// ─── Standard subdirectory names under `$SAVFOX_HOME` ──────────────────────
//
// These are the canonical strings used by `savfox_home.join(...)` across
// the workspace. Centralising them here prevents typo drift (e.g. "model"
// vs "models", ".system" vs "system") and makes a future migration to a
// typed `SavfoxPaths` accessor straightforward — every site that uses the
// constant is found by a single grep instead of hunting raw string literals
// in twenty crates.
//
// `SESSIONS_SUBDIR` and `ARCHIVED_SESSIONS_SUBDIR` already live in
// `savfox_core::rollout` and are not duplicated here; `CONFIG_TOML_FILE`
// stays in `savfox_core::config` for the same reason.

pub const AUTH_SUBDIR: &str = "auth";
pub const LOGS_SUBDIR: &str = "logs";
pub const MODELS_SUBDIR: &str = "models";
pub const SKILLS_SUBDIR: &str = "skills";
pub const PLUGINS_SUBDIR: &str = "plugins";
pub const REGISTRY_SUBDIR: &str = "registry";
pub const MEMORY_SUBDIR: &str = "memory";
pub const AGENTS_SUBDIR: &str = "agents";
pub const SHELL_SNAPSHOTS_SUBDIR: &str = "shell_snapshots";
pub const CONFIG_BACKUPS_SUBDIR: &str = "config-backups";
pub const SANDBOX_SUBDIR: &str = ".sandbox";
pub const SYSTEM_SUBDIR: &str = ".system";
pub const GATEWAY_SUBDIR: &str = "gateway";

pub const GATEWAY_PID_FILE: &str = "gateway.pid";
pub const GATEWAY_LOG_FILE: &str = "gateway.log";

/// Returns the path to the Codex configuration directory, which can be
/// specified by the `SAVFOX_HOME` environment variable. If not set, defaults to
/// `~/.savfox`.
///
/// - If `SAVFOX_HOME` is set, the value must exist and be a directory. The value will be
///   canonicalized and this function will Err otherwise.
/// - If `SAVFOX_HOME` is not set, this function does not verify that the directory exists.
pub fn find_savfox_home() -> std::io::Result<PathBuf> {
    let savfox_home_env = std::env::var("SAVFOX_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_savfox_home_from_env(savfox_home_env.as_deref())
}

fn find_savfox_home_from_env(savfox_home_env: Option<&str>) -> std::io::Result<PathBuf> {
    // Honor the `SAVFOX_HOME` environment variable when it is set to allow users
    // (and tests) to override the default location.
    if let Some(val) = savfox_home_env {
        let path = PathBuf::from(val);
        let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("SAVFOX_HOME points to {val:?}, but that path does not exist"),
            ),
            _ => std::io::Error::new(
                err.kind(),
                format!("failed to read SAVFOX_HOME {val:?}: {err}"),
            ),
        })?;

        if !metadata.is_dir() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("SAVFOX_HOME points to {val:?}, but that path is not a directory"),
            ))
        } else {
            path.canonicalize().map_err(|err| {
                std::io::Error::new(
                    err.kind(),
                    format!("failed to canonicalize SAVFOX_HOME {val:?}: {err}"),
                )
            })
        }
    } else {
        let mut p = home_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not find home directory",
            )
        })?;
        p.push(".savfox");
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::ErrorKind;

    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::find_savfox_home_from_env;

    #[test]
    fn find_savfox_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-savfox-home");
        let missing_str = missing
            .to_str()
            .expect("missing savfox home path should be valid utf-8");

        let err = find_savfox_home_from_env(Some(missing_str)).expect_err("missing SAVFOX_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("SAVFOX_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_savfox_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("savfox-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file savfox home path should be valid utf-8");

        let err = find_savfox_home_from_env(Some(file_str)).expect_err("file SAVFOX_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_savfox_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp savfox home path should be valid utf-8");

        let resolved = find_savfox_home_from_env(Some(temp_str)).expect("valid SAVFOX_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_savfox_home_without_env_uses_default_home_dir() {
        let resolved = find_savfox_home_from_env(None).expect("default SAVFOX_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(".savfox");
        assert_eq!(resolved, expected);
    }
}
