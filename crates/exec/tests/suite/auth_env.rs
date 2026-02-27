#![allow(clippy::unwrap_used, clippy::expect_used)]
use core_test_support::responses::{ev_completed, mount_sse_once_match, sse, start_mock_server};
use core_test_support::test_savfox_exec::test_savfox_exec;
use wiremock::matchers::header;

#[tokio::test(flavor = "multi_session", worker_sessions = 2)]
async fn exec_uses_savfox_api_key_env_var() -> anyhow::Result<()> {
    let test = test_savfox_exec();
    let server = start_mock_server().await;
    let repo_root = savfox_utils_cargo_bin::repo_root()?;

    mount_sse_once_match(
        &server,
        header("Authorization", "Bearer dummy"),
        sse(vec![ev_completed("request_0")]),
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&repo_root)
        .arg("echo testing savfox api key")
        .assert()
        .success();

    Ok(())
}
