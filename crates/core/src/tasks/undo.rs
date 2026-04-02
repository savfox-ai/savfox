use std::sync::Arc;

use async_trait::async_trait;
use savfox_protocol::models::ResponseItem;
use savfox_protocol::user_input::UserInput;
use savfox_utils::git::{RestoreGhostCommitOptions, restore_ghost_commit_with_options};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::protocol::{EventMsg, UndoCompletedEvent, UndoStartedEvent};
use crate::savfox::TurnContext;
use crate::state::TaskKind;
use crate::tasks::{SessionTask, SessionTaskContext};

pub(crate) struct UndoTask;

impl UndoTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionTask for UndoTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let _ = session
            .session
            .services
            .otel_manager
            .counter("savfox.task.undo", 1, &[]);
        let sess = session.clone_session();
        sess.send_event(
            ctx.as_ref(),
            EventMsg::UndoStarted(UndoStartedEvent {
                message: Some("Undo in progress...".to_owned()),
            }),
        )
        .await;

        if cancellation_token.is_cancelled() {
            sess.send_event(
                ctx.as_ref(),
                EventMsg::UndoCompleted(UndoCompletedEvent {
                    success: false,
                    message: Some("Undo cancelled.".to_owned()),
                }),
            )
            .await;
            return None;
        }

        let history = sess.clone_history().await;
        let mut items = history.raw_items().to_vec();
        let mut completed = UndoCompletedEvent {
            success: false,
            message: None,
        };

        let Some((idx, ghost_commit)) =
            items
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, item)| match item {
                    ResponseItem::GhostSnapshot { ghost_commit } => {
                        Some((idx, ghost_commit.clone()))
                    }
                    _ => None,
                })
        else {
            completed.message = Some("No ghost snapshot available to undo.".to_owned());
            sess.send_event(ctx.as_ref(), EventMsg::UndoCompleted(completed))
                .await;
            return None;
        };

        let commit_id = ghost_commit.id().to_owned();
        let repo_path = ctx.cwd.clone();
        let ghost_snapshot = ctx.ghost_snapshot.clone();
        let restore_result = tokio::task::spawn_blocking(move || {
            let options = RestoreGhostCommitOptions::new(&repo_path).ghost_snapshot(ghost_snapshot);
            restore_ghost_commit_with_options(&options, &ghost_commit)
        })
        .await;

        match restore_result {
            Ok(Ok(())) => {
                items.remove(idx);
                sess.replace_history(items).await;
                let short_id: String = commit_id.chars().take(7).collect();
                info!(commit_id = commit_id, "Undo restored ghost snapshot");
                completed.success = true;
                completed.message = Some(format!("Undo restored snapshot {short_id}."));
            }
            Ok(Err(err)) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                warn!("{message}");
                completed.message = Some(message);
            }
            Err(err) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                error!("{message}");
                completed.message = Some(message);
            }
        }

        sess.send_event(ctx.as_ref(), EventMsg::UndoCompleted(completed))
            .await;
        None
    }
}
