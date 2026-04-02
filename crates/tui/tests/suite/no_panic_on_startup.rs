use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::select;
use tokio::time::timeout;

/// Regression test for https://github.com/openai/savfox/issues/8803.
#[tokio::test]
async fn malformed_rules_should_not_panic() -> anyhow::Result<()> {
    // run_savfox_cli() does not work on Windows due to PTY limitations.
    if cfg!(windows) {
        return Ok(());
    }

    let tmp = tempfile::tempdir()?;
    let savfox_home = tmp.path();
    std::fs::write(
        savfox_home.join("rules"),
        "rules should be a directory not a file",
    )?;

    // TODO(mbolin): Figure out why using a temp dir as the cwd causes this test
    // to hang.
    let cwd = std::env::current_dir()?;
    let config_contents = format!(
        r#"
# Pick a local provider so the CLI doesn't prompt for OpenAI auth in this test.
model_provider = "ollama"

[projects]
"{cwd}" = {{ trust_level = "trusted" }}
"#,
        cwd = cwd.display()
    );
    std::fs::write(savfox_home.join("config.toml"), config_contents)?;

    let SavfoxCliOutput { exit_code, output } = run_savfox_cli(savfox_home, cwd).await?;
    assert_ne!(0, exit_code, "Savfox CLI should exit nonzero.");
    assert!(
        output.contains("ERROR: Failed to initialize savfox:"),
        "expected startup error in output, got: {output}"
    );
    assert!(
        output.contains("failed to read rules files"),
        "expected rules read error in output, got: {output}"
    );
    Ok(())
}

struct SavfoxCliOutput {
    exit_code: i32,
    output: String,
}

async fn run_savfox_cli(
    savfox_home: impl AsRef<Path>,
    cwd: impl AsRef<Path>,
) -> anyhow::Result<SavfoxCliOutput> {
    let savfox_cli = savfox_utils::cargo_bin::cargo_bin("savfox")?;
    let mut env = HashMap::new();
    env.insert(
        "SAVFOX_HOME".to_owned(),
        savfox_home.as_ref().display().to_string(),
    );

    let args = vec!["-c".to_owned(), "analytics.enabled=false".to_owned()];
    let spawned = savfox_utils::pty::spawn_pty_process(
        savfox_cli.to_string_lossy().as_ref(),
        &args,
        cwd.as_ref(),
        &env,
        &None,
    )
    .await?;
    let mut output = Vec::new();
    let mut output_rx = spawned.output_rx;
    let mut exit_rx = spawned.exit_rx;
    let writer_tx = spawned.session.writer_sender();
    let exit_code_result = timeout(Duration::from_secs(10), async {
        // Read PTY output until the process exits while replying to cursor
        // position queries so the TUI can initialize without a real terminal.
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        // The TUI asks for the cursor position via ESC[6n.
                        // Respond with a valid position to unblock startup.
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                        }
                        output.extend_from_slice(&chunk);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                result = &mut exit_rx => break result,
            }
        }
    })
    .await;
    let exit_code = match exit_code_result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            spawned.session.terminate();
            anyhow::bail!("timed out waiting for savfox CLI to exit");
        }
    };
    // Drain any output that raced with the exit notification.
    while let Ok(chunk) = output_rx.try_recv() {
        output.extend_from_slice(&chunk);
    }

    let output = String::from_utf8_lossy(&output);
    Ok(SavfoxCliOutput {
        exit_code,
        output: output.to_string(),
    })
}
