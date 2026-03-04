//! Entry-point for the `savfox-exec` binary.
//!
//! When this CLI is invoked normally, it parses the standard `savfox-exec` CLI
//! options and launches the non-interactive Savfox agent. However, if it is
//! invoked with arg0 as `savfox-linux-sandbox`, we instead treat the invocation
//! as a request to run the logic for the standalone `savfox-linux-sandbox`
//! executable (i.e., parse any -s args and then run a *sandboxed* command under
//! Landlock + seccomp.
//!
//! This allows us to ship a completely separate set of functionality as part
//! of the `savfox-exec` binary.
use clap::Parser;
use savfox_arg0::arg0_dispatch_or_else;
use savfox_exec::{Cli, run_main};

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|savfox_linux_sandbox_exe| async move {
        let cli = Cli::parse();
        run_main(cli, savfox_linux_sandbox_exe).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn top_cli_parses_resume_prompt_after_config_flag() {
        const PROMPT: &str = "echo resume-with-global-flags-after-subcommand";
        let cli = TopCli::parse_from([
            "savfox-exec",
            "resume",
            "--last",
            "--json",
            "--model",
            "gpt-5.2-savfox",
            "--config",
            "reasoning_level=xhigh",
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
            PROMPT,
        ]);

        let Some(savfox_exec::Command::Resume(args)) = cli.inner.command else {
            panic!("expected resume command");
        };
        let effective_prompt = args.prompt.clone().or_else(|| {
            if args.last {
                args.session_id.clone()
            } else {
                None
            }
        });
        assert_eq!(effective_prompt.as_deref(), Some(PROMPT));
        assert_eq!(cli.config_overrides.raw_overrides.len(), 1);
        assert_eq!(
            cli.config_overrides.raw_overrides[0],
            "reasoning_level=xhigh"
        );
    }
}
