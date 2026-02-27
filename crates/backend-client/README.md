# savfox-backend-client

HTTP client for communicating with the Savfox/ChatGPT backend API. Provides the `Client` struct which wraps `reqwest` and handles authentication (bearer tokens), user-agent headers, and ChatGPT account ID headers.

The client supports two path styles: `SavfoxApi` (routes prefixed with `/api/savfox/`) and `ChatGptApi` (routes prefixed with `/wham/`), automatically selecting the correct style based on the base URL. It exposes methods for fetching rate limits and credit status (`get_rate_limits`), listing and inspecting cloud tasks (`list_tasks`, `get_task_details`, `list_sibling_turns`), creating new tasks (`create_task`), and retrieving managed configuration requirements (`get_config_requirements_file`).

Constructed from `SavfoxAuth` credentials via `Client::from_auth()`, this crate serves as the low-level HTTP transport layer used by higher-level crates such as `savfox-cloud-requirements` and `savfox-cloud-tasks-client`.
