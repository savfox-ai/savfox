use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::cdp::CdpClient;
use crate::page::Page;

#[derive(Debug, Clone)]
pub struct BrowserLaunchOptions {
    pub executable_path: Option<PathBuf>,
    pub user_data_dir: Option<PathBuf>,
    pub headless: bool,
    pub extra_args: Vec<String>,
}

impl Default for BrowserLaunchOptions {
    fn default() -> Self {
        Self {
            executable_path: None,
            user_data_dir: None,
            headless: true,
            extra_args: Vec::new(),
        }
    }
}

pub struct Browser {
    process: Mutex<Option<Child>>,
    cdp_client: Arc<CdpClient>,
    user_data_dir: PathBuf,
    debug_port: u16,
}

impl Browser {
    pub async fn launch() -> Result<Self> {
        Self::launch_with_launch_options(BrowserLaunchOptions::default()).await
    }

    pub async fn launch_with_options(
        executable_path: Option<&Path>,
        user_data_dir: Option<&Path>,
    ) -> Result<Self> {
        let options = BrowserLaunchOptions {
            executable_path: executable_path.map(Path::to_path_buf),
            user_data_dir: user_data_dir.map(Path::to_path_buf),
            ..Default::default()
        };
        Self::launch_with_launch_options(options).await
    }

    pub async fn launch_with_launch_options(options: BrowserLaunchOptions) -> Result<Self> {
        let debug_port = find_available_port().await?;

        let temp_dir = options.user_data_dir.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("savfox-browser-{}", uuid::Uuid::now_v7()))
        });

        if !temp_dir.exists() {
            tokio::fs::create_dir_all(&temp_dir).await?;
        }

        let browser_path = options
            .executable_path
            .ok_or_else(|| anyhow!("Chrome executable not found"))
            .or_else(|_: anyhow::Error| find_chrome_executable())?;

        info!(
            path = %browser_path.display(),
            port = debug_port,
            "Launching browser"
        );

        let mut cmd = Command::new(&browser_path);
        let mut args = vec![
            "--remote-debugging-port".to_string(),
            debug_port.to_string(),
            "--user-data-dir".to_string(),
            temp_dir.to_string_lossy().to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-client-side-phishing-detection".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-sync".to_string(),
            "--disable-translate".to_string(),
            "--metrics-recording-only".to_string(),
        ];
        if options.headless {
            args.push("--headless=new".to_string());
            args.push("--hide-scrollbars".to_string());
            args.push("--mute-audio".to_string());
        }
        args.extend(options.extra_args);
        args.push("about:blank".to_string());
        cmd.args(args);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut process = cmd.spawn()?;

        let stdout = process.stdout.take().expect("stdout");
        let stderr = process.stderr.take().expect("stderr");

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(browser_stdout = %line);
            }
        });

        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(browser_stderr = %line);
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let websocket_url = format!("ws://127.0.0.1:{debug_port}/devtools/browser");
        let cdp_client = Arc::new(CdpClient::connect(&websocket_url).await?);

        Ok(Self {
            process: Mutex::new(Some(process)),
            cdp_client,
            user_data_dir: temp_dir,
            debug_port,
        })
    }

    pub async fn new_page(&self) -> Result<Page> {
        let target_id = self
            .cdp_client
            .send_request(
                "Target.createTarget",
                serde_json::json!({
                    "url": "about:blank",
                }),
            )
            .await?
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Failed to get target ID"))?
            .to_string();

        let session_id = self
            .cdp_client
            .send_request(
                "Target.attachToTarget",
                serde_json::json!({
                    "targetId": target_id,
                    "flatten": true,
                }),
            )
            .await?
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Failed to attach to target"))?
            .to_string();

        Ok(Page::new(target_id, session_id, self.cdp_client.clone()))
    }

    pub async fn pages(&self) -> Result<Vec<Page>> {
        let targets = self
            .cdp_client
            .send_request("Target.getTargets", serde_json::json!({}))
            .await?
            .get("targetInfos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("Failed to get targets"))?
            .clone();

        let mut pages = Vec::new();
        for target in targets {
            if target.get("type").and_then(|v| v.as_str()) == Some("page") {
                if let Some(target_id) = target.get("targetId").and_then(|v| v.as_str()) {
                    let session_id = self
                        .cdp_client
                        .send_request(
                            "Target.attachToTarget",
                            serde_json::json!({
                                "targetId": target_id,
                                "flatten": true,
                            }),
                        )
                        .await?
                        .get("sessionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();

                    pages.push(Page::new(
                        target_id.to_string(),
                        session_id,
                        self.cdp_client.clone(),
                    ));
                }
            }
        }

        Ok(pages)
    }

    pub async fn close(&self) -> Result<()> {
        let mut process = self.process.lock().await;
        if let Some(mut p) = process.take() {
            let _ = p.kill().await;
            info!("Browser process terminated");
        }

        if self.user_data_dir.exists() {
            if let Err(err) = tokio::fs::remove_dir_all(&self.user_data_dir).await {
                warn!("Failed to remove user data dir: {}", err);
            }
        }

        Ok(())
    }

    pub fn debug_port(&self) -> u16 {
        self.debug_port
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Ok(mut proc) = self.process.try_lock() {
            if let Some(mut p) = proc.take() {
                let _ = p.start_kill();
            }
        }
    }
}

async fn find_available_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

fn find_chrome_executable() -> Result<PathBuf> {
    let candidates = if cfg!(target_os = "windows") {
        vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else {
        vec![
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    };

    for path in candidates {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }

    if let Ok(path) = which::which("chrome") {
        return Ok(path);
    }
    if let Ok(path) = which::which("chromium") {
        return Ok(path);
    }
    if let Ok(path) = which::which("google-chrome") {
        return Ok(path);
    }

    Err(anyhow!("Chrome/Chromium executable not found"))
}
