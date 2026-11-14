//! Process-group helpers shared by pipe/pty and shell command execution.
//!
//! This module centralizes the OS-specific pieces that ensure a spawned
//! command can be cleaned up reliably:
//! - `set_process_group` is called in `pre_exec` so the child starts its own process group.
//! - `detach_from_tty` starts a new session so non-interactive children do not inherit the
//!   controlling TTY.
//! - `kill_process_group_by_pid` targets the whole group (children/grandchildren)
//! - `kill_process_group` targets a known process group ID directly instead of a single PID.
//! - `set_parent_death_signal` (Linux only) arranges for the child to receive a `SIGTERM` when the
//!   parent exits, and re-checks the parent PID to avoid races during fork/exec.
//!
//! Windows child-tree termination uses taskkill; other process-group helpers are Unix-only.

use std::io;

use tokio::process::{Child, Command};

#[cfg(unix)]
/// Configure a child process so it can be detached from the caller's TTY and,
/// on Linux, receive `SIGTERM` when the original parent exits.
pub fn configure_command_pre_exec(command: &mut Command, should_detach_from_tty: bool) {
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };

    unsafe {
        command.pre_exec(move || {
            if should_detach_from_tty {
                detach_from_tty()?;
            }

            #[cfg(target_os = "linux")]
            {
                set_parent_death_signal(parent_pid)?;
            }

            Ok(())
        });
    }
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn configure_command_pre_exec(_command: &mut Command, _should_detach_from_tty: bool) {}

#[cfg(target_os = "linux")]
/// Ensure the child receives SIGTERM when the original parent dies.
///
/// This should run in `pre_exec` and uses `parent_pid` captured before spawn to
/// avoid a race where the parent exits between fork and exec.
pub fn set_parent_death_signal(parent_pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::getppid() } != parent_pid {
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
/// No-op on non-Linux platforms.
pub fn set_parent_death_signal(_parent_pid: i32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Detach from the controlling TTY by starting a new session.
pub fn detach_from_tty() -> io::Result<()> {
    let result = unsafe { libc::setsid() };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPERM) {
            return set_process_group();
        }
        return Err(err);
    }
    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn detach_from_tty() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Put the calling process into its own process group.
///
/// Intended for use in `pre_exec` so the child becomes the group leader.
pub fn set_process_group() -> io::Result<()> {
    let result = unsafe { libc::setpgid(0, 0) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn set_process_group() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Kill the process group for the given PID (best-effort).
///
/// This resolves the PGID for `pid` and sends SIGKILL to the whole group.
pub fn kill_process_group_by_pid(pid: u32) -> io::Result<()> {
    use std::io::ErrorKind;

    let pid = pid as libc::pid_t;
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
        return Ok(());
    }

    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn kill_process_group_by_pid(_pid: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Kill a specific process group ID (best-effort).
pub fn kill_process_group(process_group_id: u32) -> io::Result<()> {
    use std::io::ErrorKind;

    let pgid = process_group_id as libc::pid_t;
    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn kill_process_group(_process_group_id: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Kill the process group for a tokio child (best-effort).
pub fn kill_child_process_group(child: &mut Child) -> io::Result<()> {
    if let Some(pid) = child.id() {
        return kill_process_group_by_pid(pid);
    }

    Ok(())
}

#[cfg(windows)]
/// Terminate the Windows process tree while the root PID is still available.
/// Use an argv-only native utility; never interpolate command text into a shell.
pub fn kill_child_process_group(child: &mut Child) -> io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    kill_process_tree_by_pid(pid)
}

/// OS utility startup can take several seconds under compilation or antivirus load.
/// Keep one shared bound for async and blocking execution paths.
#[cfg(windows)]
pub const WINDOWS_TREE_TERMINATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Bounded native tree cleanup for blocking Windows capture workers.
#[cfg(windows)]
pub fn kill_process_tree_by_pid(pid: u32) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    let utility = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
    )
    .join("System32")
    .join("taskkill.exe");
    let mut utility = std::process::Command::new(utility)
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + WINDOWS_TREE_TERMINATION_TIMEOUT;
    loop {
        if let Some(status) = utility.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "taskkill failed for process tree {pid}: {status}"
                )))
            };
        }
        if std::time::Instant::now() >= deadline {
            utility.kill()?;
            utility.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "taskkill deadline exceeded",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Async tree termination for executor paths. Windows utility execution is bounded
/// so a stalled OS utility cannot occupy a Tokio worker or delay shutdown forever.
#[cfg(windows)]
pub async fn kill_child_process_group_async(child: &mut Child) -> io::Result<()> {
    use std::process::Stdio;
    let Some(pid) = child.id() else {
        return Ok(());
    };
    let utility = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
    )
    .join("System32")
    .join("taskkill.exe");
    let mut command = Command::new(utility);
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    run_tree_termination_utility(command, WINDOWS_TREE_TERMINATION_TIMEOUT).await
}

#[cfg(windows)]
async fn run_tree_termination_utility(
    mut command: Command,
    timeout: std::time::Duration,
) -> io::Result<()> {
    let mut utility = command.spawn()?;
    if let Ok(result) = tokio::time::timeout(timeout, utility.wait()).await {
        let status = result?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "process tree termination utility failed: {status}"
            )))
        }
    } else {
        utility.start_kill()?;
        utility.wait().await?;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "process tree termination utility exceeded deadline",
        ))
    }
}

#[cfg(not(windows))]
pub async fn kill_child_process_group_async(child: &mut Child) -> io::Result<()> {
    kill_child_process_group(child)
}

#[cfg(not(any(unix, windows)))]
/// No-op on unsupported platforms.
pub fn kill_child_process_group(_child: &mut Child) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use std::process::Stdio;

    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    // Failure paths must not leave a test's descendants waiting on a gate forever.
    struct TestTreeCleanup {
        root: u32,
        descendant: Option<u32>,
        armed: bool,
    }
    impl Drop for TestTreeCleanup {
        fn drop(&mut self) {
            if self.armed {
                let _ = kill_process_tree_by_pid(self.root);
                if let Some(pid) = self.descendant {
                    let _ = kill_process_tree_by_pid(pid);
                }
            }
        }
    }

    #[tokio::test]
    async fn windows_tree_utility_timeout_does_not_block_runtime() -> io::Result<()> {
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 60",
            ])
            .creation_flags(0x08000000)
            .kill_on_drop(true);
        let timeout = run_tree_termination_utility(command, std::time::Duration::from_millis(100));
        let progress = async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let (result, ()) = tokio::join!(timeout, progress);
        assert_eq!(
            result.expect_err("deadline").kind(),
            io::ErrorKind::TimedOut
        );
        Ok(())
    }

    #[tokio::test]
    async fn windows_tree_termination_stops_descendant_writes() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("descendant-was-alive");
        let script = directory.path().join("child.ps1");
        let gate = directory.path().join("allow-write");
        std::fs::write(
            &script,
            "$deadline=[DateTime]::UtcNow.AddSeconds(60); while (-not (Test-Path $env:SAVFOX_TEST_GATE) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }; if (Test-Path $env:SAVFOX_TEST_GATE) { [IO.File]::WriteAllText($env:SAVFOX_TEST_MARKER, 'alive') }; Start-Sleep -Seconds 60",
        )?;
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", r#"$p = Start-Process powershell.exe -WindowStyle Hidden -PassThru -ArgumentList @('-NoProfile', '-File', ('"' + $env:SAVFOX_TEST_SCRIPT + '"')); [Console]::WriteLine($p.Id); Start-Sleep -Seconds 60"#])
            .env("SAVFOX_TEST_GATE", &gate)
            .env("SAVFOX_TEST_MARKER", &marker)
            .env("SAVFOX_TEST_SCRIPT", &script)
            .creation_flags(0x08000000)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut cleanup = TestTreeCleanup {
            root: child.id().expect("root pid"),
            descendant: None,
            armed: true,
        };
        let mut line = String::new();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            stdout.read_line(&mut line),
        )
        .await??;
        cleanup.descendant = line.trim().parse::<u32>().ok();
        assert!(
            cleanup.descendant.is_some(),
            "descendant must have started: {line}"
        );
        kill_child_process_group_async(&mut child).await?;
        child.wait().await?;
        // Permit writes only after cleanup reports completion; utility startup
        // latency cannot race the assertion. A survivor will now create marker.
        std::fs::write(&gate, "go")?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(!marker.exists(), "descendant survived tree termination");
        cleanup.armed = false;
        Ok(())
    }
}
