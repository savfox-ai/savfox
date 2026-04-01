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
