//! `savfox dashboard` — Open the web UI in the default browser.

use clap::Parser;

/// Open the gateway web dashboard in the default browser
#[derive(Debug, Parser)]
pub struct DashboardCommand {
    /// Gateway host
    #[clap(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Gateway port
    #[clap(long, default_value_t = 18881)]
    pub port: u16,
    /// Use HTTPS
    #[clap(long)]
    pub https: bool,
}

pub async fn run(cmd: DashboardCommand) -> Result<(), Box<dyn std::error::Error>> {
    let scheme = if cmd.https { "https" } else { "http" };
    let url = format!("{scheme}://{}:{}", cmd.host, cmd.port);

    eprintln!("Opening dashboard: {url}");

    // Try to open in default browser
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", &url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }

    Ok(())
}
