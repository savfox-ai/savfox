//! Bounded SSE delivery with an independent terminal result and invocation cancellation.
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::sync::{mpsc, oneshot, watch};

pub(crate) const STREAM_CAPACITY: usize = 64;
pub(crate) const MAX_FRAME_BYTES: usize = 16 * 1024;
pub(crate) const MAX_INPUT_DELTA_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StreamFailure {
    Disconnected,
    SlowConsumer,
    FrameTooLarge,
    Agent(String),
    ProducerDropped,
}

impl StreamFailure {
    pub(crate) fn agent(message: &str) -> Self {
        Self::Agent(message.chars().take(2048).collect())
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Disconnected => "client_disconnected",
            Self::SlowConsumer => "slow_consumer",
            Self::FrameTooLarge => "stream_frame_too_large",
            Self::Agent(_) => "agent_error",
            Self::ProducerDropped => "stream_producer_dropped",
        }
    }
}

impl fmt::Display for StreamFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => formatter.write_str("SSE client disconnected"),
            Self::SlowConsumer => {
                formatter.write_str("SSE client exhausted the bounded output queue")
            }
            Self::FrameTooLarge => formatter.write_str("SSE output exceeded the frame size limit"),
            Self::Agent(message) => formatter.write_str(message),
            Self::ProducerDropped => {
                formatter.write_str("SSE producer ended without a terminal result")
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct StreamCancellation {
    state: watch::Sender<Option<StreamFailure>>,
}

impl StreamCancellation {
    fn new() -> Self {
        Self {
            state: watch::channel(None).0,
        }
    }

    pub(crate) fn cancel(&self, failure: StreamFailure) {
        self.state.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(failure);
            true
        });
    }

    pub(crate) fn reason(&self) -> Option<StreamFailure> {
        self.state.borrow().clone()
    }

    pub(crate) async fn cancelled(&self) -> StreamFailure {
        let mut receiver = self.state.subscribe();
        loop {
            if let Some(failure) = receiver.borrow_and_update().clone() {
                return failure;
            }
            // `self` retains a sender throughout this wait.
            if receiver.changed().await.is_err() {
                return StreamFailure::ProducerDropped;
            }
        }
    }
}

type TerminalResult = Result<Vec<String>, StreamFailure>;

#[derive(Clone)]
pub(crate) struct StreamSender {
    frames: mpsc::Sender<String>,
    terminal: Arc<Mutex<Option<oneshot::Sender<TerminalResult>>>>,
    cancellation: StreamCancellation,
}

impl StreamSender {
    pub(crate) fn send(&self, frame: String) -> bool {
        if self.cancellation.reason().is_some() {
            return false;
        }
        let failure = if frame.len() > MAX_FRAME_BYTES {
            StreamFailure::FrameTooLarge
        } else {
            match self.frames.try_send(frame) {
                Ok(()) => return true,
                Err(mpsc::error::TrySendError::Full(_)) => StreamFailure::SlowConsumer,
                Err(mpsc::error::TrySendError::Closed(_)) => StreamFailure::Disconnected,
            }
        };
        self.cancellation.cancel(failure);
        false
    }

    pub(crate) fn complete(self, frames: Vec<String>) {
        let terminal =
            if frames.len() > 2 || frames.iter().any(|frame| frame.len() > MAX_FRAME_BYTES) {
                self.cancellation.cancel(StreamFailure::FrameTooLarge);
                Err(StreamFailure::FrameTooLarge)
            } else if let Some(failure) = self.cancellation.reason() {
                Err(failure)
            } else {
                Ok(frames)
            };
        self.finish(terminal);
    }

    pub(crate) fn fail(self, failure: StreamFailure) {
        self.cancellation.cancel(failure);
        self.finish(Err(self
            .cancellation
            .reason()
            .unwrap_or(StreamFailure::ProducerDropped)));
    }

    fn finish(&self, terminal: TerminalResult) {
        if let Ok(mut sender) = self.terminal.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(terminal);
        }
    }
}

pub(crate) struct BoundedStream {
    frames: mpsc::Receiver<String>,
    terminal: oneshot::Receiver<TerminalResult>,
    final_frames: VecDeque<String>,
    cancellation: StreamCancellation,
    cancelled: Pin<Box<dyn Future<Output = StreamFailure> + Send>>,
    terminal_received: bool,
    finished: bool,
}

impl Stream for BoundedStream {
    type Item = Result<String, StreamFailure>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.terminal_received
            && let Poll::Ready(failure) = this.cancelled.as_mut().poll(context)
        {
            // In particular, a persistent terminal may retain a sender after
            // HTTP delivery fails. Do not wait for that process to complete in
            // order to tell its subscriber that the stream is incomplete.
            this.frames.close();
            while this.frames.try_recv().is_ok() {}
            this.terminal_received = true;
            return Poll::Ready(Some(Err(failure)));
        }
        if let Some(frame) = this.final_frames.pop_front() {
            return Poll::Ready(Some(Ok(frame)));
        }
        if this.terminal_received {
            this.finished = true;
            return Poll::Ready(None);
        }
        match this.frames.poll_recv(context) {
            Poll::Ready(Some(frame)) => return Poll::Ready(Some(Ok(frame))),
            Poll::Pending => return Poll::Pending,
            Poll::Ready(None) => {}
        }
        match Pin::new(&mut this.terminal).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.terminal_received = true;
                match result.unwrap_or(Err(StreamFailure::ProducerDropped)) {
                    Ok(frames) => {
                        this.final_frames = frames.into();
                        if let Some(frame) = this.final_frames.pop_front() {
                            Poll::Ready(Some(Ok(frame)))
                        } else {
                            this.finished = true;
                            Poll::Ready(None)
                        }
                    }
                    Err(failure) => Poll::Ready(Some(Err(failure))),
                }
            }
        }
    }
}

impl Drop for BoundedStream {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.cancel(StreamFailure::Disconnected);
        }
    }
}

pub(crate) fn bounded_stream() -> (StreamSender, BoundedStream, StreamCancellation) {
    let (tx, rx) = mpsc::channel(STREAM_CAPACITY);
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let cancellation = StreamCancellation::new();
    let receiver_cancellation = cancellation.clone();
    (
        StreamSender {
            frames: tx,
            terminal: Arc::new(Mutex::new(Some(terminal_tx))),
            cancellation: cancellation.clone(),
        },
        BoundedStream {
            frames: rx,
            terminal: terminal_rx,
            final_frames: VecDeque::new(),
            cancellation: cancellation.clone(),
            cancelled: Box::pin(async move { receiver_cancellation.cancelled().await }),
            terminal_received: false,
            finished: false,
        },
        cancellation,
    )
}

/// Await cleanup after dropping a cancelled collector: dropping its future alone
/// does not stop a core turn or remove outstanding approval registrations.
pub(crate) async fn run_cancellable<T, E>(
    cancellation: &StreamCancellation,
    invocation: impl Future<Output = Result<T, E>>,
    cleanup: impl Future<Output = ()>,
) -> Result<Result<T, E>, StreamFailure> {
    let outcome = tokio::select! {
        biased;
        failure = cancellation.cancelled() => Err(failure),
        result = invocation => Ok(result),
    };
    cleanup.await;
    if let Some(failure) = cancellation.reason() {
        Err(failure)
    } else {
        outcome
    }
}

pub(crate) fn text_chunks(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let mut end = text.len().min(1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = text.split_at(end);
        text = rest;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures_util::StreamExt;

    use super::*;

    #[tokio::test]
    async fn overflow_cancels_and_retains_an_observable_terminal_error() {
        let (sender, mut stream, cancellation) = bounded_stream();
        for index in 0..STREAM_CAPACITY {
            assert!(sender.send(index.to_string()));
        }
        assert!(!sender.send("lost delta".into()));
        assert_eq!(cancellation.cancelled().await, StreamFailure::SlowConsumer);
        assert_eq!(stream.next().await, Some(Err(StreamFailure::SlowConsumer)));
        assert_eq!(stream.next().await, None);
        // Retaining the original producer does not delay the error.
        assert!(!sender.send("later terminal output".into()));
        sender.complete(vec!["stop".into(), "[DONE]".into()]);
    }

    #[tokio::test]
    async fn dropping_the_receiver_cancels_only_its_invocation_and_awaits_cleanup() {
        let (_sender, stream, cancellation) = bounded_stream();
        let (_other_sender, _other_stream, other_cancellation) = bounded_stream();
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup_finished = cleaned.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            run_cancellable(
                &task_cancellation,
                std::future::pending::<Result<(), ()>>(),
                async move {
                    // Model asynchronous approval removal / core shutdown.
                    tokio::task::yield_now().await;
                    cleanup_finished.store(true, Ordering::SeqCst);
                },
            )
            .await
        });
        drop(stream);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            Err(StreamFailure::Disconnected)
        );
        assert!(cleaned.load(Ordering::SeqCst));
        assert!(other_cancellation.reason().is_none());
    }

    #[tokio::test]
    async fn oversized_frames_fail_without_entering_the_queue() {
        let (sender, mut stream, cancellation) = bounded_stream();
        assert!(!sender.send("x".repeat(MAX_FRAME_BYTES + 1)));
        sender.complete(vec!["stop".into()]);
        assert_eq!(cancellation.reason(), Some(StreamFailure::FrameTooLarge));
        assert_eq!(stream.next().await, Some(Err(StreamFailure::FrameTooLarge)));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn success_preserves_delta_order_and_terminal_markers() {
        let (sender, mut stream, _) = bounded_stream();
        assert!(sender.send("first".into()));
        assert!(sender.send("second".into()));
        sender.complete(vec!["stop".into(), "[DONE]".into()]);
        let mut frames = Vec::new();
        while let Some(frame) = stream.next().await {
            frames.push(frame.unwrap());
        }
        assert_eq!(frames, vec!["first", "second", "stop", "[DONE]"]);
    }

    #[test]
    fn unicode_chunks_are_bounded_and_lossless() {
        let text = "你好🌍".repeat(2048);
        let chunks = text_chunks(&text).collect::<Vec<_>>();
        assert!(chunks.iter().all(|chunk| chunk.len() <= 1024));
        assert_eq!(chunks.concat(), text);
    }
}
