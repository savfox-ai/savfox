# savfox-cloud-tasks

Implements the `savfox cloud` CLI subcommand for managing cloud-hosted coding tasks. This crate provides a terminal UI (built on `ratatui` and `crossterm`) for listing, inspecting, creating, and applying cloud tasks, as well as a non-interactive CLI interface.

The crate handles backend authentication via ChatGPT login, supports both a live HTTP backend and a mock backend (controlled by `SAVFOX_CLOUD_TASKS_MODE`), and includes functionality for applying task diffs to the local repository. It depends on `savfox-cloud-tasks-client` for the backend API abstraction and `savfox-tui` for shared TUI components.
