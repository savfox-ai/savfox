#![allow(unsafe_code)]
#![allow(clippy::exit)]

use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use savfox_core::SAVFOX_APPLY_PATCH_ARG1;
use tempfile::TempDir;

const LINUX_SANDBOX_ARG0: &str = "savfox-linux-sandbox";
const APPLY_PATCH_ARG0: &str = "apply_patch";
const MISSPELLED_APPLY_PATCH_ARG0: &str = "applypatch";
const TOKIO_WORKER_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

#[must_use]
pub fn arg0_dispatch() -> Option<TempDir> {
    // Determine if we were invoked via the special alias.
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_default();
    let exe_name = Path::new(&argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if exe_name == LINUX_SANDBOX_ARG0 {
        // Safety: [`run_main`] never returns.
        savfox_linux_sandbox::run_main();
    } else if exe_name == APPLY_PATCH_ARG0 || exe_name == MISSPELLED_APPLY_PATCH_ARG0 {
        savfox_apply_patch::main();
    }

    let argv1 = args.next().unwrap_or_default();
    if argv1 == SAVFOX_APPLY_PATCH_ARG1 {
        let patch_arg = args.next().and_then(|s| s.to_str().map(str::to_owned));
        let exit_code = if let Some(patch_arg) = patch_arg {
            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();
            match savfox_apply_patch::apply_patch(&patch_arg, &mut stdout, &mut stderr) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        } else {
            eprintln!("Error: {SAVFOX_APPLY_PATCH_ARG1} requires a UTF-8 PATCH argument.");
            1
        };
        std::process::exit(exit_code);
    }

    // This modifies the environment, which is not session-safe, so do this
    // before creating any sessions/the Tokio runtime.
    load_dotenv();

    match prepend_path_entry_for_savfox_aliases() {
        Ok(path_entry) => Some(path_entry),
        Err(err) => {
            // It is possible that Savfox will proceed successfully even if
            // updating the PATH fails, so warn the user and move on.
            eprintln!("WARNING: proceeding, even though we could not update PATH: {err}");
            None
        }
    }
}

/// While we want to deploy the Savfox CLI as a single executable for simplicity,
/// we also want to expose some of its functionality as distinct CLIs, so we use
/// the "arg0 trick" to determine which CLI to dispatch. This effectively allows
/// us to simulate deploying multiple executables as a single binary on Mac and
/// Linux (but not Windows).
///
/// When the current executable is invoked through the hard-link or alias named
/// `savfox-linux-sandbox` we *directly* execute
/// [`savfox_linux_sandbox::run_main`] (which never returns). Otherwise we:
///
/// 1. Load `.env` values from `~/.savfox/.env` before creating any sessions.
/// 2. Construct a Tokio multi-session runtime.
/// 3. Derive the path to the current executable (so children can re-invoke the sandbox) when
///    running on Linux.
/// 4. Execute the provided async `main_fn` inside that runtime, forwarding any error. Note that
///    `main_fn` receives `savfox_linux_sandbox_exe: Option<PathBuf>`, as an argument, which is
///    generally needed as part of constructing [`savfox_core::config::Config`].
///
/// This function should be used to wrap any `main()` function in binary crates
/// in this workspace that depends on these helper CLIs.
pub fn arg0_dispatch_or_else<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(Option<PathBuf>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // Retain the TempDir so it exists for the lifetime of the invocation of
    // this executable. Admittedly, we could invoke `keep()` on it, but it
    // would be nice to avoid leaving temporary directories behind, if possible.
    let _path_entry = arg0_dispatch();

    // Regular invocation – create a Tokio runtime and execute the provided
    // async entry-point.
    //
    // On Windows debug builds we observed startup stack overflows in worker
    // threads with the default runtime stack size. Use a larger stack to keep
    // gateway startup stable.
    let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
    runtime_builder.enable_all();
    runtime_builder.thread_stack_size(TOKIO_WORKER_STACK_SIZE_BYTES);
    let runtime = runtime_builder.build()?;
    runtime.block_on(async move {
        let savfox_linux_sandbox_exe: Option<PathBuf> = if cfg!(target_os = "linux") {
            std::env::current_exe().ok()
        } else {
            None
        };

        main_fn(savfox_linux_sandbox_exe).await
    })
}

const ILLEGAL_ENV_VAR_PREFIX: &str = "SAVFOX_";

/// Load env vars from ~/.savfox/.env.
///
/// Security: Do not allow `.env` files to create or modify any variables
/// with names starting with `SAVFOX_`.
fn load_dotenv() {
    if let Ok(savfox_home) = savfox_core::config::find_savfox_home()
        && let Ok(iter) = dotenvy::from_path_iter(savfox_home.join(".env"))
    {
        set_filtered(iter);
    }
}

/// Helper to set vars from a dotenvy iterator while filtering out `SAVFOX_` keys.
fn set_filtered<I>(iter: I)
where
    I: IntoIterator<Item = Result<(String, String), dotenvy::Error>>,
{
    for (key, value) in iter.into_iter().flatten() {
        if !key.to_ascii_uppercase().starts_with(ILLEGAL_ENV_VAR_PREFIX) {
            // It is safe to call set_var() because our process is
            // single-sessioned at this point in its execution.
            unsafe { std::env::set_var(&key, &value) };
        }
    }
}

/// Creates a temporary directory with either:
///
/// - UNIX: `apply_patch` symlink to the current executable
/// - WINDOWS: `apply_patch.bat` batch script to invoke the current executable with the "secret"
///   --savfox-run-as-apply-patch flag.
///
/// This temporary directory is prepended to the PATH environment variable so
/// that `apply_patch` can be on the PATH without requiring the user to
/// install a separate `apply_patch` executable, simplifying the deployment of
/// Savfox CLI.
/// Note: In debug builds the temp-dir guard is disabled to ease local testing.
///
/// IMPORTANT: This function modifies the PATH environment variable, so it MUST
/// be called before multiple sessions are spawned.
pub fn prepend_path_entry_for_savfox_aliases() -> std::io::Result<TempDir> {
    let savfox_home = savfox_core::config::find_savfox_home()?;
    #[cfg(not(debug_assertions))]
    {
        // Guard against placing helpers in system temp directories outside debug builds.
        let temp_root = std::env::temp_dir();
        if savfox_home.starts_with(&temp_root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Refusing to create helper binaries under temporary dir {temp_root:?} (savfox_home: {savfox_home:?})"
                ),
            ));
        }
    }

    std::fs::create_dir_all(&savfox_home)?;
    // Use a SAVFOX_HOME-scoped temp root to avoid cluttering the top-level directory.
    let temp_root = savfox_home.join("tmp").join("path");
    std::fs::create_dir_all(&temp_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Ensure only the current user can access the temp directory.
        std::fs::set_permissions(&temp_root, std::fs::Permissions::from_mode(0o700))?;
    }

    let temp_dir = tempfile::Builder::new()
        .prefix("savfox-arg0")
        .tempdir_in(&temp_root)?;
    let path = temp_dir.path();

    for filename in &[
        APPLY_PATCH_ARG0,
        MISSPELLED_APPLY_PATCH_ARG0,
        #[cfg(target_os = "linux")]
        LINUX_SANDBOX_ARG0,
    ] {
        let exe = std::env::current_exe()?;

        #[cfg(unix)]
        {
            let link = path.join(filename);
            symlink(&exe, &link)?;
        }

        #[cfg(windows)]
        {
            let batch_script = path.join(format!("{filename}.bat"));
            std::fs::write(
                &batch_script,
                format!(
                    r#"@echo off
"{}" {SAVFOX_APPLY_PATCH_ARG1} %*
"#,
                    exe.display()
                ),
            )?;
        }
    }

    #[cfg(unix)]
    const PATH_SEPARATOR: &str = ":";

    #[cfg(windows)]
    const PATH_SEPARATOR: &str = ";";

    let path_element = path.display();
    let updated_path_env_var = match std::env::var("PATH") {
        Ok(existing_path) => {
            format!("{path_element}{PATH_SEPARATOR}{existing_path}")
        }
        Err(_) => {
            format!("{path_element}")
        }
    };

    unsafe {
        std::env::set_var("PATH", updated_path_env_var);
    }

    Ok(temp_dir)
}
