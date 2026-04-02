use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollInput {
    pub question: String,
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_selections: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_hours: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedPollInput {
    pub question: String,
    pub options: Vec<String>,
    pub max_selections: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_hours: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResult {
    pub poll_id: String,
    pub question: String,
    pub options: Vec<PollOption>,
    pub total_votes: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub ends_at: Option<chrono::DateTime<chrono::Utc>>,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollOption {
    pub text: String,
    pub votes: usize,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePoll {
    pub poll: NormalizedPollInput,
    pub votes: HashMap<String, Vec<String>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub ends_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct Polls {
    polls: HashMap<String, ActivePoll>,
    max_options: usize,
    default_duration_hours: u32,
    max_duration_hours: u32,
}

impl Polls {
    #[must_use]
    pub fn new() -> Self {
        Self {
            polls: HashMap::new(),
            max_options: 10,
            default_duration_hours: 24,
            max_duration_hours: 168,
        }
    }

    pub fn normalize(&self, input: PollInput) -> Result<NormalizedPollInput, String> {
        let question = input.question.trim().to_owned();
        if question.is_empty() {
            return Err("Poll question is required".to_owned());
        }

        let options: Vec<String> = input
            .options
            .iter()
            .map(|o| o.trim().to_owned())
            .filter(|o| !o.is_empty())
            .collect();

        if options.len() < 2 {
            return Err("Poll requires at least 2 options".to_owned());
        }

        if options.len() > self.max_options {
            return Err(format!(
                "Poll supports at most {} options",
                self.max_options
            ));
        }

        let max_selections = input.max_selections.unwrap_or(1);
        if max_selections < 1 {
            return Err("max_selections must be at least 1".to_owned());
        }
        if max_selections > options.len() {
            return Err("max_selections cannot exceed option count".to_owned());
        }

        let duration_hours = input
            .duration_hours
            .map(|d| d.clamp(1, self.max_duration_hours));

        Ok(NormalizedPollInput {
            question,
            options,
            max_selections,
            duration_hours,
        })
    }

    pub fn create(&mut self, input: PollInput) -> Result<String, String> {
        let normalized = self.normalize(input)?;
        let poll_id = uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();

        let ends_at = normalized
            .duration_hours
            .map(|hours| chrono::Utc::now() + chrono::Duration::hours(hours as i64));

        self.polls.insert(
            poll_id.clone(),
            ActivePoll {
                poll: normalized,
                votes: HashMap::new(),
                created_at: chrono::Utc::now(),
                ends_at,
            },
        );

        Ok(poll_id)
    }

    pub fn vote(
        &mut self,
        poll_id: &str,
        voter_id: &str,
        option_indices: Vec<usize>,
    ) -> Result<(), String> {
        let poll = self
            .polls
            .get_mut(poll_id)
            .ok_or_else(|| format!("Poll not found: {poll_id}"))?;

        if let Some(ends_at) = poll.ends_at
            && chrono::Utc::now() > ends_at
        {
            return Err("Poll has ended".to_owned());
        }

        if poll.votes.contains_key(voter_id) {
            return Err("Already voted".to_owned());
        }

        if option_indices.is_empty() || option_indices.len() > poll.poll.max_selections {
            return Err(format!(
                "Must select 1-{} options",
                poll.poll.max_selections
            ));
        }

        for &idx in &option_indices {
            if idx >= poll.poll.options.len() {
                return Err(format!("Invalid option index: {idx}"));
            }
        }

        poll.votes.insert(
            voter_id.to_owned(),
            option_indices.iter().map(|&i| i.to_string()).collect(),
        );
        Ok(())
    }

    #[must_use]
    pub fn get_result(&self, poll_id: &str) -> Option<PollResult> {
        let poll = self.polls.get(poll_id)?;

        let is_active = match poll.ends_at {
            Some(ends_at) => chrono::Utc::now() < ends_at,
            None => true,
        };

        let total_votes = poll.votes.len();
        let mut option_counts = vec![0usize; poll.poll.options.len()];

        for votes in poll.votes.values() {
            for vote in votes {
                if let Ok(idx) = vote.parse::<usize>()
                    && idx < option_counts.len()
                {
                    option_counts[idx] += 1;
                }
            }
        }

        let options: Vec<PollOption> = poll
            .poll
            .options
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let votes = option_counts[i];
                let percentage = if total_votes > 0 {
                    (votes as f64 / total_votes as f64) * 100.0
                } else {
                    0.0
                };
                PollOption {
                    text: text.clone(),
                    votes,
                    percentage,
                }
            })
            .collect();

        Some(PollResult {
            poll_id: poll_id.to_owned(),
            question: poll.poll.question.clone(),
            options,
            total_votes,
            created_at: poll.created_at,
            ends_at: poll.ends_at,
            is_active,
        })
    }

    pub fn close(&mut self, poll_id: &str) -> Option<PollResult> {
        let poll = self.polls.get_mut(poll_id)?;
        poll.ends_at = Some(chrono::Utc::now());
        self.get_result(poll_id)
    }

    pub fn delete(&mut self, poll_id: &str) -> bool {
        self.polls.remove(poll_id).is_some()
    }

    #[must_use]
    pub fn list_active(&self) -> Vec<&ActivePoll> {
        self.polls
            .values()
            .filter(|p| match p.ends_at {
                Some(ends_at) => chrono::Utc::now() < ends_at,
                None => true,
            })
            .collect()
    }

    pub fn prune_expired(&mut self) -> usize {
        let now = chrono::Utc::now();
        let before = self.polls.len();
        self.polls.retain(|_, p| match p.ends_at {
            Some(ends_at) => now < ends_at,
            None => true,
        });
        before - self.polls.len()
    }
}

impl Default for Polls {
    fn default() -> Self {
        Self::new()
    }
}
