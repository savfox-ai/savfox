//! Message queuing with FIFO and priority support for incoming gateway messages.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

/// Priority levels for messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MessagePriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Urgent = 3,
}

impl Default for MessagePriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// A queued message with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub priority: MessagePriority,
    pub payload: serde_json::Value,
    pub source: String,
    pub enqueued_at: u64,
}

/// Thread-safe message queue with priority support
#[derive(Debug)]
pub struct MessageQueue {
    urgent: Mutex<VecDeque<QueuedMessage>>,
    normal: Mutex<VecDeque<QueuedMessage>>,
    notify: Notify,
    max_size: usize,
}

impl MessageQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            urgent: Mutex::new(VecDeque::new()),
            normal: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            max_size,
        }
    }

    /// Enqueue a message. Urgent/High go to the priority queue, others to normal.
    pub async fn enqueue(&self, msg: QueuedMessage) -> Result<(), String> {
        match msg.priority {
            MessagePriority::Urgent | MessagePriority::High => {
                let mut q = self.urgent.lock().await;
                if q.len() >= self.max_size {
                    return Err("Priority queue full".to_string());
                }
                q.push_back(msg);
            }
            _ => {
                let mut q = self.normal.lock().await;
                if q.len() >= self.max_size {
                    return Err("Normal queue full".to_string());
                }
                q.push_back(msg);
            }
        }
        self.notify.notify_one();
        Ok(())
    }

    /// Dequeue the next message (priority first, then FIFO).
    pub async fn dequeue(&self) -> QueuedMessage {
        loop {
            // Check urgent queue first
            {
                let mut q = self.urgent.lock().await;
                if let Some(msg) = q.pop_front() {
                    return msg;
                }
            }
            // Then normal queue
            {
                let mut q = self.normal.lock().await;
                if let Some(msg) = q.pop_front() {
                    return msg;
                }
            }
            // Wait for notification
            self.notify.notified().await;
        }
    }

    /// Try to dequeue without blocking
    pub async fn try_dequeue(&self) -> Option<QueuedMessage> {
        if let Some(msg) = self.urgent.lock().await.pop_front() {
            return Some(msg);
        }
        self.normal.lock().await.pop_front()
    }

    /// Get queue depth
    pub async fn len(&self) -> usize {
        self.urgent.lock().await.len() + self.normal.lock().await.len()
    }

    /// Check if empty
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new(10_000)
    }
}
