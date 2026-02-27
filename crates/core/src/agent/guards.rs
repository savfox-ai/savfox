use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use savfox_protocol::SessionId;
use savfox_protocol::protocol::{SessionSource, SubAgentSource};

use crate::error::{SavfoxError, Result};

/// This structure is used to add some limits on the multi-agent capabilities for Savfox. In
/// the current implementation, it limits:
/// * Total number of sub-agents (i.e. sessions) per user session
///
/// This structure is shared by all agents in the same user session (because the `AgentControl`
/// is).
#[derive(Default)]
pub(crate) struct Guards {
    sessions_set: Mutex<HashSet<SessionId>>,
    total_count: AtomicUsize,
}

/// Initial agent is depth 0.
pub(crate) const MAX_THREAD_SPAWN_DEPTH: i32 = 1;

fn session_depth(session_source: &SessionSource) -> i32 {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::SessionSpawn { depth, .. }) => *depth,
        SessionSource::SubAgent(_) => 0,
        _ => 0,
    }
}

pub(crate) fn next_session_spawn_depth(session_source: &SessionSource) -> i32 {
    session_depth(session_source).saturating_add(1)
}

pub(crate) fn exceeds_session_spawn_depth_limit(depth: i32) -> bool {
    depth > MAX_THREAD_SPAWN_DEPTH
}

impl Guards {
    pub(crate) fn reserve_spawn_slot(
        self: &Arc<Self>,
        max_sessions: Option<usize>,
    ) -> Result<SpawnReservation> {
        if let Some(max_sessions) = max_sessions {
            if !self.try_increment_spawned(max_sessions) {
                return Err(SavfoxError::AgentLimitReached { max_sessions });
            }
        } else {
            self.total_count.fetch_add(1, Ordering::AcqRel);
        }
        Ok(SpawnReservation {
            state: Arc::clone(self),
            active: true,
        })
    }

    pub(crate) fn release_spawned_session(&self, session_id: SessionId) {
        let removed = {
            let mut sessions = self
                .sessions_set
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions.remove(&session_id)
        };
        if removed {
            self.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn register_spawned_session(&self, session_id: SessionId) {
        let mut sessions = self
            .sessions_set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.insert(session_id);
    }

    fn try_increment_spawned(&self, max_sessions: usize) -> bool {
        let mut current = self.total_count.load(Ordering::Acquire);
        loop {
            if current >= max_sessions {
                return false;
            }
            match self.total_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => current = updated,
            }
        }
    }
}

pub(crate) struct SpawnReservation {
    state: Arc<Guards>,
    active: bool,
}

impl SpawnReservation {
    pub(crate) fn commit(mut self, session_id: SessionId) {
        self.state.register_spawned_session(session_id);
        self.active = false;
    }
}

impl Drop for SpawnReservation {
    fn drop(&mut self) {
        if self.active {
            self.state.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn session_depth_defaults_to_zero_for_root_sources() {
        assert_eq!(session_depth(&SessionSource::Cli), 0);
    }

    #[test]
    fn session_spawn_depth_increments_and_enforces_limit() {
        let session_source = SessionSource::SubAgent(SubAgentSource::SessionSpawn {
            parent_session_id: SessionId::new(),
            depth: 1,
        });
        let child_depth = next_session_spawn_depth(&session_source);
        assert_eq!(child_depth, 2);
        assert!(exceeds_session_spawn_depth_limit(child_depth));
    }

    #[test]
    fn non_session_spawn_subagents_default_to_depth_zero() {
        let session_source = SessionSource::SubAgent(SubAgentSource::Review);
        assert_eq!(session_depth(&session_source), 0);
        assert_eq!(next_session_spawn_depth(&session_source), 1);
        assert!(!exceeds_session_spawn_depth_limit(1));
    }

    #[test]
    fn reservation_drop_releases_slot() {
        let guards = Arc::new(Guards::default());
        let reservation = guards.reserve_spawn_slot(Some(1)).expect("reserve slot");
        drop(reservation);

        let reservation = guards.reserve_spawn_slot(Some(1)).expect("slot released");
        drop(reservation);
    }

    #[test]
    fn commit_holds_slot_until_release() {
        let guards = Arc::new(Guards::default());
        let reservation = guards.reserve_spawn_slot(Some(1)).expect("reserve slot");
        let session_id = SessionId::new();
        reservation.commit(session_id);

        let err = match guards.reserve_spawn_slot(Some(1)) {
            Ok(_) => panic!("limit should be enforced"),
            Err(err) => err,
        };
        let SavfoxError::AgentLimitReached { max_sessions } = err else {
            panic!("expected SavfoxError::AgentLimitReached");
        };
        assert_eq!(max_sessions, 1);

        guards.release_spawned_session(session_id);
        let reservation = guards
            .reserve_spawn_slot(Some(1))
            .expect("slot released after session removal");
        drop(reservation);
    }

    #[test]
    fn release_ignores_unknown_session_id() {
        let guards = Arc::new(Guards::default());
        let reservation = guards.reserve_spawn_slot(Some(1)).expect("reserve slot");
        let session_id = SessionId::new();
        reservation.commit(session_id);

        guards.release_spawned_session(SessionId::new());

        let err = match guards.reserve_spawn_slot(Some(1)) {
            Ok(_) => panic!("limit should still be enforced"),
            Err(err) => err,
        };
        let SavfoxError::AgentLimitReached { max_sessions } = err else {
            panic!("expected SavfoxError::AgentLimitReached");
        };
        assert_eq!(max_sessions, 1);

        guards.release_spawned_session(session_id);
        let reservation = guards
            .reserve_spawn_slot(Some(1))
            .expect("slot released after real session removal");
        drop(reservation);
    }

    #[test]
    fn release_is_idempotent_for_registered_sessions() {
        let guards = Arc::new(Guards::default());
        let reservation = guards.reserve_spawn_slot(Some(1)).expect("reserve slot");
        let first_id = SessionId::new();
        reservation.commit(first_id);

        guards.release_spawned_session(first_id);

        let reservation = guards.reserve_spawn_slot(Some(1)).expect("slot reused");
        let second_id = SessionId::new();
        reservation.commit(second_id);

        guards.release_spawned_session(first_id);

        let err = match guards.reserve_spawn_slot(Some(1)) {
            Ok(_) => panic!("limit should still be enforced"),
            Err(err) => err,
        };
        let SavfoxError::AgentLimitReached { max_sessions } = err else {
            panic!("expected SavfoxError::AgentLimitReached");
        };
        assert_eq!(max_sessions, 1);

        guards.release_spawned_session(second_id);
        let reservation = guards
            .reserve_spawn_slot(Some(1))
            .expect("slot released after second session removal");
        drop(reservation);
    }
}
