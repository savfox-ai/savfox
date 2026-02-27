# savfox-cloud-requirements

Fetches organization-managed configuration requirements from the backend for Business and Enterprise ChatGPT customers. These cloud-hosted requirements (served as TOML) can enforce policies such as allowed approval modes, sandbox modes, MCP server configurations, and custom rules.

The crate provides `cloud_requirements_loader()`, which spawns a background task to fetch and parse the requirements with a 5-second timeout. If the fetch fails, times out, or the user is not on a Business/Enterprise plan, Savfox continues without cloud requirements. The loaded `ConfigRequirementsToml` is fed into the config loader pipeline alongside local configuration sources.

This crate depends on `savfox-backend-client` for HTTP communication and `savfox-core` for auth management and config types.
