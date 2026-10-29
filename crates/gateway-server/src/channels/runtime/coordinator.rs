//! Channel execution has separate bounds for accepted work, gateway executions,
//! and executions in one conversation. Admission and idle retirement share a lock.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::warn;

use crate::channel::GatewayChannel;
use crate::session::SessionStore;

pub(crate) struct InboundTask {
    pub platform: &'static str,
    pub channel_id: String,
    pub prompt: String,
    pub name: Option<String>,
    pub meta: Option<super::StartThreadMeta>,
    pub gateway_channel: Arc<GatewayChannel>,
    pub session_store: Arc<SessionStore>,
}

const SESSION_QUEUE_CAPACITY: usize = 32;
const MAX_SESSION_EXECUTIONS: usize = 2;
const MAX_GATEWAY_EXECUTIONS: usize = 16;
const MAX_ACCEPTED_TASKS: usize = 128;

struct Admitted<T> {
    task: T,
    // Keep the admission slot through execution, not just through dequeueing.
    _admission: OwnedSemaphorePermit,
}

struct Coordinator<T> {
    inboxes: Mutex<HashMap<String, mpsc::Sender<Admitted<T>>>>,
    admission: Arc<Semaphore>,
    execution: Arc<Semaphore>,
    session_executions: usize,
    queue_capacity: usize,
    max_actors: usize,
}

impl<T: Send + 'static> Coordinator<T> {
    fn new(accepted: usize, executions: usize, session_executions: usize) -> Self {
        Self {
            inboxes: Mutex::new(HashMap::new()),
            admission: Arc::new(Semaphore::new(accepted)),
            execution: Arc::new(Semaphore::new(executions)),
            session_executions,
            queue_capacity: SESSION_QUEUE_CAPACITY,
            max_actors: accepted,
        }
    }

    async fn dispatch<F, Fut>(self: &Arc<Self>, key: String, task: T, run: F) -> bool
    where
        F: Fn(T) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Ok(admission) = self.admission.clone().try_acquire_owned() else {
            return false;
        };
        let mut task = Admitted {
            task,
            _admission: admission,
        };
        let mut inboxes = self.inboxes.lock().await;
        if let Some(sender) = inboxes.get(&key) {
            match sender.try_send(task) {
                Ok(()) => return true,
                Err(mpsc::error::TrySendError::Full(_)) => return false,
                Err(mpsc::error::TrySendError::Closed(returned)) => task = returned,
            }
        }
        // Completed actors normally retire immediately. Also bound registry
        // entries while their cleanup is waiting to be scheduled.
        if !inboxes.contains_key(&key) && inboxes.len() >= self.max_actors {
            return false;
        }
        let (sender, receiver) = mpsc::channel(self.queue_capacity);
        if sender.try_send(task).is_err() {
            return false;
        }
        inboxes.insert(key.clone(), sender.clone());
        let coordinator = self.clone();
        tokio::spawn(async move {
            coordinator.run(key, sender, receiver, run).await;
        });
        true
    }

    async fn retire_if_idle(
        &self,
        key: &str,
        sender: &mpsc::Sender<Admitted<T>>,
        receiver: &mut mpsc::Receiver<Admitted<T>>,
    ) -> bool {
        let mut inboxes = self.inboxes.lock().await;
        // A send may win the lock before retirement. Drain accepted work
        // instead of dropping it. No sender escapes the registry in production.
        if !receiver.is_empty() {
            return false;
        }
        receiver.close();
        if inboxes
            .get(key)
            .is_some_and(|entry| entry.same_channel(sender))
        {
            inboxes.remove(key);
        }
        true
    }

    async fn run<F, Fut>(
        self: Arc<Self>,
        key: String,
        sender: mpsc::Sender<Admitted<T>>,
        mut receiver: mpsc::Receiver<Admitted<T>>,
        run: F,
    ) where
        F: Fn(T) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut running = JoinSet::new();
        loop {
            let next = if running.is_empty() {
                if self.retire_if_idle(&key, &sender, &mut receiver).await {
                    return;
                }
                receiver.recv().await
            } else {
                tokio::select! {
                    completed = running.join_next() => {
                        if let Some(Err(error)) = completed {
                            warn!(session_key = %key, %error, "channel execution task failed");
                        }
                        continue;
                    }
                    next = receiver.recv(), if running.len() < self.session_executions => next,
                }
            };
            let Some(admitted) = next else {
                while running.join_next().await.is_some() {}
                self.retire_if_idle(&key, &sender, &mut receiver).await;
                return;
            };
            // Wait inside the bounded actor, before spawning another execution.
            // The inbox retains backpressure while the global budget is busy.
            let Ok(execution) = self.execution.clone().acquire_owned().await else {
                return;
            };
            let run = run.clone();
            running.spawn(async move {
                let _execution = execution;
                let Admitted { task, _admission } = admitted;
                run(task).await;
                drop(_admission);
            });
        }
    }
}

/// Return true only when bounded, live processing accepts the task. Durable
/// upstream inboxes must retain rejected work for a later retry.
pub(crate) async fn dispatch(session_key: String, task: InboundTask) -> bool {
    static COORDINATOR: OnceLock<Arc<Coordinator<InboundTask>>> = OnceLock::new();
    let coordinator = COORDINATOR.get_or_init(|| {
        Arc::new(Coordinator::new(
            MAX_ACCEPTED_TASKS,
            MAX_GATEWAY_EXECUTIONS,
            MAX_SESSION_EXECUTIONS,
        ))
    });
    let accepted = coordinator
        .dispatch(session_key.clone(), task, |task| async move {
            super::spawn_start_thread_pipeline_with_meta(
                task.gateway_channel,
                task.session_store,
                task.platform,
                task.channel_id,
                task.prompt,
                task.name,
                task.meta,
            )
            .await;
        })
        .await;
    if !accepted {
        warn!(
            session_key,
            "channel execution capacity exhausted; inbound task was not admitted"
        );
    }
    accepted
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn bounds_running_and_accepted_work_and_releases_slots() {
        let coordinator = Arc::new(Coordinator::new(8, 3, 2));
        let release = Arc::new(Semaphore::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel();
        let run = {
            let release = release.clone();
            move |session: &'static str| {
                let release = release.clone();
                let started = started_tx.clone();
                let done = done_tx.clone();
                async move {
                    started.send(session).unwrap();
                    release.acquire().await.unwrap().forget();
                    done.send(session).unwrap();
                }
            }
        };
        for key in ["a", "a", "a", "a", "b", "b", "b", "b"] {
            assert!(coordinator.dispatch(key.to_owned(), key, run.clone()).await);
        }
        assert!(!coordinator.dispatch("c".to_owned(), "c", run.clone()).await);
        let mut starts = Vec::new();
        for _ in 0..3 {
            starts.push(
                tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert!(starts.iter().filter(|key| **key == "a").count() <= 2);
        assert!(starts.iter().filter(|key| **key == "b").count() <= 2);
        assert!(started_rx.try_recv().is_err());
        assert_eq!(coordinator.execution.available_permits(), 0);
        release.add_permits(8);
        for _ in 0..8 {
            tokio::time::timeout(Duration::from_secs(2), done_rx.recv())
                .await
                .unwrap()
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.admission.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(coordinator.dispatch("c".to_owned(), "c", run).await);
        release.add_permits(1);
    }

    #[tokio::test]
    async fn idle_retirement_preserves_successfully_admitted_work() {
        let coordinator = Arc::new(Coordinator::new(4, 1, 1));
        let (sender, mut receiver) = mpsc::channel(2);
        coordinator
            .inboxes
            .lock()
            .await
            .insert("key".to_owned(), sender.clone());
        assert!(
            coordinator
                .dispatch("key".to_owned(), 7, |_| async {})
                .await
        );
        // Dispatch wins the registry lock just before idle retirement.
        assert!(
            !coordinator
                .retire_if_idle("key", &sender, &mut receiver)
                .await
        );
        assert_eq!(receiver.recv().await.unwrap().task, 7);
        assert!(
            coordinator
                .retire_if_idle("key", &sender, &mut receiver)
                .await
        );
        assert!(sender.is_closed());
        let (ran_tx, mut ran_rx) = mpsc::unbounded_channel();
        assert!(
            coordinator
                .dispatch("key".to_owned(), 8, move |value| {
                    let ran = ran_tx.clone();
                    async move {
                        ran.send(value).unwrap();
                    }
                })
                .await
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), ran_rx.recv())
                .await
                .unwrap(),
            Some(8)
        );
    }

    #[tokio::test]
    async fn completed_conversations_release_the_actor_registry_immediately() {
        let coordinator = Arc::new(Coordinator::new(4, 1, 1));
        for index in 0..20 {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let done = Arc::new(Mutex::new(Some(done_tx)));
            assert!(
                coordinator
                    .dispatch(index.to_string(), (), move |_| {
                        let done = done.clone();
                        async move {
                            done.lock().await.take().unwrap().send(()).unwrap();
                        }
                    })
                    .await
            );
            done_rx.await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !coordinator.inboxes.lock().await.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(coordinator.admission.available_permits(), 4);
    }
}
