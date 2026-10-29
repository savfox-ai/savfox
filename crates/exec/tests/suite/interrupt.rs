#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use core_test_support::test_savfox_exec::test_savfox_exec;
use savfox_core::auth::SAVFOX_API_KEY_ENV_VAR;
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_exits_with_130_in_both_output_modes() -> Result<()> {
    for json in [false, true] {
        let test = test_savfox_exec();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
            .mount(&server)
            .await;
        let mut command = Command::new(savfox_utils::cargo_bin::cargo_bin("savfox-exec")?);
        command
            .current_dir(test.cwd_path())
            .env("SAVFOX_HOME", test.home_path())
            .env(SAVFOX_API_KEY_ENV_VAR, "dummy")
            .env("OPENAI_BASE_URL", format!("{}/v1", server.uri()))
            .args(["--skip-git-repo-check", "wait for the model"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if json {
            command.arg("--json");
        }
        let child = command.spawn()?;
        let pid = child.id().context("child has a process id")?;
        timeout(Duration::from_secs(15), async {
            loop {
                if server
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|request| request.url.path() == "/v1/responses")
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("exec never reached its in-flight model request")?;
        let signal = Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await?;
        anyhow::ensure!(signal.success(), "could not send SIGINT to exec");
        let output = timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .context("exec hung after Ctrl+C")??;
        anyhow::ensure!(
            output.status.code() == Some(130),
            "unexpected exit: {:?}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            let events = String::from_utf8(output.stdout)?
                .lines()
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            anyhow::ensure!(
                events.iter().any(|event| event["type"] == "turn.failed"
                    && event["error"]["message"] == "Turn interrupted"),
                "missing interrupted JSONL terminal event: {events:?}"
            );
        }
    }
    Ok(())
}
