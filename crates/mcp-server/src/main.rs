use savfox_arg0::arg0_dispatch_or_else;
use savfox_mcp_server::run_main;

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|savfox_linux_sandbox_exe| async move {
        run_main(savfox_linux_sandbox_exe).await?;
        Ok(())
    })
}
