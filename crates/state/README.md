# savfox-state

SQLite-backed persistent state for Savfox thread and rollout metadata. This crate extracts metadata from JSONL rollout files and mirrors it into a local SQLite database (`state.sqlite`), enabling fast queries over session history without rescanning the filesystem.

The primary entrypoint is `StateRuntime`, which owns the database connection pool and provides methods for thread CRUD operations: listing threads with pagination and filtering (by source, model provider, archive status), looking up threads by ID, upserting thread metadata, and managing archive/unarchive transitions. It also supports incremental rollout application -- given a stream of `RolloutItem` values, it updates the corresponding thread record in place. Dynamic tool definitions attached to threads are persisted in a separate table.

The crate also provides a structured logging subsystem. Log entries (with timestamp, level, target, message, and optional thread association) can be inserted and queried through `StateRuntime`, supporting filtered queries by level, time range, module path, and thread ID. Metrics constants are exported for telemetry integration covering database initialization, backfill, and error tracking.
