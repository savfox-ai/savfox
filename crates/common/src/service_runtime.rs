use serde::de::DeserializeOwned;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error};
use tracing_subscriber::EnvFilter;

pub const DEFAULT_CHANNEL_CAPACITY: usize = 128;

#[must_use]
pub fn env_filter_from_default(default_directive: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_directive))
}

pub fn init_stderr_tracing(default_directive: &str) {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter_from_default(default_directive))
        .try_init();
}

#[must_use]
pub fn spawn_stdin_json_reader<T>(
    sender: mpsc::Sender<T>,
    decode_error_label: &'static str,
) -> JoinHandle<()>
where
    T: DeserializeOwned + Send + 'static,
{
    tokio::spawn(async move {
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await.unwrap_or_default() {
            match serde_json::from_str::<T>(&line) {
                Ok(msg) => {
                    if sender.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(err) => error!("{decode_error_label}: {err}"),
            }
        }

        debug!("stdin reader finished (EOF)");
    })
}
