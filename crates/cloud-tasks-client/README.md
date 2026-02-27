# savfox-cloud-tasks-client

Defines the `CloudBackend` trait and associated types for interacting with the cloud tasks API. This crate provides the API abstraction layer used by `savfox-cloud-tasks`, separating backend communication logic from the CLI/TUI presentation.

Key types include `TaskSummary`, `TaskStatus`, `TurnAttempt`, `DiffSummary`, and `ApplyOutcome`. Two backend implementations are available behind feature flags: `online` (the real HTTP client backed by `savfox-backend-client`) and `mock` (an in-memory mock for development and testing). The apply engine for diffs is delegated to the shared `savfox-git` crate.
