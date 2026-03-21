use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info};

use super::audit_log::AuditEvent;

pub struct AuditStore {
    log_path: PathBuf,
    enabled: Arc<RwLock<bool>>,
    tx: mpsc::Sender<AuditEvent>,
}

const CHANNEL_SIZE: usize = 1024;
const MAX_FILE_SIZE_MB: u64 = 100;

impl AuditStore {
    pub fn new(savfox_home: &std::path::Path) -> Self {
        let log_path = savfox_home.join("logs").join("audit.log");
        let (tx, rx) = mpsc::channel::<AuditEvent>(CHANNEL_SIZE);
        let enabled = Arc::new(RwLock::new(true));

        let store = Self {
            log_path,
            enabled: Arc::clone(&enabled),
            tx,
        };

        tokio::spawn(Self::writer_task(
            rx,
            store.log_path.clone(),
            Arc::clone(&enabled),
        ));

        store
    }

    pub fn from_home(savfox_home: &std::path::Path) -> Self {
        Self::new(savfox_home)
    }

    async fn writer_task(
        mut rx: mpsc::Receiver<AuditEvent>,
        log_path: PathBuf,
        enabled: Arc<RwLock<bool>>,
    ) {
        let Some(parent) = log_path.parent() else {
            error!(
                "Audit log path has no parent directory: {}",
                log_path.display()
            );
            return;
        };
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            error!("Failed to create audit log directory: {}", e);
            return;
        }

        info!("Audit store initialized: {}", log_path.display());

        while let Some(event) = rx.recv().await {
            let is_enabled = *enabled.read().await;
            if !is_enabled {
                continue;
            }

            let json = event.to_json();
            if let Err(e) = Self::append_to_file(&log_path, &json).await {
                error!("Failed to write audit event: {}", e);
            }
        }

        debug!("Audit writer task stopped");
    }

    async fn append_to_file(path: &PathBuf, line: &str) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;

        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        file.sync_data().await?;

        Ok(())
    }

    pub async fn log(&self, event: AuditEvent) {
        if self.tx.send(event).await.is_err() {
            error!("Failed to send audit event to writer");
        }
    }

    pub async fn set_enabled(&self, enabled: bool) {
        let mut current = self.enabled.write().await;
        *current = enabled;
        info!(
            "Audit logging {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }

    pub async fn is_enabled(&self) -> bool {
        *self.enabled.read().await
    }

    pub async fn rotate(&self) -> std::io::Result<()> {
        if !self.log_path.exists() {
            return Ok(());
        }

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let rotated_path = self
            .log_path
            .with_file_name(format!("audit_{}.log", timestamp));

        tokio::fs::rename(&self.log_path, &rotated_path).await?;
        info!("Rotated audit log to {}", rotated_path.display());

        Ok(())
    }

    pub async fn get_recent_events(&self, limit: usize) -> Vec<AuditEvent> {
        if !self.log_path.exists() {
            return Vec::new();
        }

        let content = match tokio::fs::read_to_string(&self.log_path).await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        content
            .lines()
            .filter_map(|line| serde_json::from_str::<AuditEvent>(line).ok())
            .rev()
            .take(limit)
            .collect()
    }

    pub async fn prune_old_logs(&self, days_to_keep: u32) -> std::io::Result<usize> {
        let logs_dir = self
            .log_path
            .parent()
            .ok_or_else(|| std::io::Error::other("audit log path has no parent directory"))?;
        let mut entries = tokio::fs::read_dir(logs_dir).await?;
        let mut pruned = 0;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(days_to_keep as i64);

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "log").unwrap_or(false) {
                if let Ok(metadata) = entry.metadata().await {
                    if let Ok(modified) = metadata.modified() {
                        let modified: chrono::DateTime<chrono::Utc> = modified.into();
                        if modified < cutoff && path != self.log_path {
                            tokio::fs::remove_file(&path).await?;
                            pruned += 1;
                        }
                    }
                }
            }
        }

        if pruned > 0 {
            info!("Pruned {} old audit log files", pruned);
        }

        Ok(pruned)
    }
}

#[derive(Clone)]
pub struct AuditLogger {
    store: Arc<AuditStore>,
}

impl AuditLogger {
    pub fn new(store: Arc<AuditStore>) -> Self {
        Self { store }
    }

    pub async fn log(&self, event: AuditEvent) {
        self.store.log(event).await;
    }

    pub async fn log_auth_success(&self, user_id: &str, method: &str) {
        self.store
            .log(AuditEvent::auth_success(user_id, method))
            .await;
    }

    pub async fn log_auth_failure(&self, user_id: Option<&str>, method: &str, reason: &str) {
        self.store
            .log(AuditEvent::auth_failure(
                user_id.map(|s| s.to_string()),
                method,
                reason,
            ))
            .await;
    }

    pub async fn log_command(
        &self,
        sender_id: &str,
        channel: &str,
        command: &str,
        args: Option<&str>,
    ) {
        self.store
            .log(AuditEvent::command_executed(
                sender_id,
                channel,
                command,
                args.map(|s| s.to_string()),
            ))
            .await;
    }

    pub async fn log_tool_invocation(
        &self,
        tool_name: &str,
        user_id: Option<&str>,
        session_id: Option<&str>,
        success: bool,
    ) {
        self.store
            .log(AuditEvent::tool_invoked(
                tool_name,
                user_id.map(|s| s.to_string()),
                session_id.map(|s| s.to_string()),
                success,
            ))
            .await;
    }

    pub async fn log_security_alert(
        &self,
        severity: super::audit_log::AuditSeverity,
        message: &str,
        details: std::collections::HashMap<String, String>,
    ) {
        self.store
            .log(AuditEvent::security_alert(severity, message, details))
            .await;
    }

    pub async fn is_enabled(&self) -> bool {
        self.store.is_enabled().await
    }
}
