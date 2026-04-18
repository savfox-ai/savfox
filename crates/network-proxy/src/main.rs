use anyhow::Result;
use clap::Parser;
use savfox_network_proxy::{Args, NetworkProxy};

const WINDOWS_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

fn main() -> Result<()> {
    run_on_configured_stack(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(WINDOWS_STACK_SIZE_BYTES)
            .build()?;
        runtime.block_on(async_main())
    })
}

async fn async_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let _ = args;
    let proxy = NetworkProxy::builder().build().await?;
    proxy.run().await?.wait().await
}

#[cfg(windows)]
fn run_on_configured_stack<F>(main_fn: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("savfox-network-proxy-main".to_owned())
        .stack_size(WINDOWS_STACK_SIZE_BYTES)
        .spawn(main_fn)?;

    match handle.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(not(windows))]
fn run_on_configured_stack<F>(main_fn: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    main_fn()
}
