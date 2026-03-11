#![allow(clippy::expect_used)]
use std::path::Path;

use savfox_core::auth::SAVFOX_API_KEY_ENV_VAR;
use tempfile::TempDir;
use wiremock::MockServer;

pub struct TestSavfoxExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestSavfoxExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(
            savfox_utils::cargo_bin::cargo_bin("savfox-exec")
                .expect("should find binary for savfox-exec"),
        );
        cmd.current_dir(self.cwd.path())
            .env("SAVFOX_HOME", self.home.path())
            .env(SAVFOX_API_KEY_ENV_VAR, "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let mut cmd = self.cmd();
        let base = format!("{}/v1", server.uri());
        cmd.env("OPENAI_BASE_URL", base);
        cmd
    }

    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }
}

pub fn test_savfox_exec() -> TestSavfoxExecBuilder {
    TestSavfoxExecBuilder {
        home: TempDir::new().expect("create temp home"),
        cwd: TempDir::new().expect("create temp cwd"),
    }
}
