# savfox-arg0

Implements the "arg0 dispatch" pattern for the Savfox CLI, allowing a single binary to behave as multiple distinct tools depending on how it is invoked. When the executable is launched with `argv[0]` set to `apply_patch` or `savfox-linux-sandbox`, it dispatches directly to the corresponding subsystem (`savfox-apply-patch` or `savfox-linux-sandbox`) without entering the normal CLI flow.

On startup, the crate creates a temporary directory containing symlinks (Unix) or batch scripts (Windows) that point back to the current executable under the alias names, then prepends this directory to `PATH`. This ensures that child processes spawned by the agent can invoke `apply_patch` as if it were a standalone binary.

The crate also handles loading environment variables from `~/.savfox/.env` (filtering out any `SAVFOX_`-prefixed variables for security) and provides `arg0_dispatch_or_else()` as a convenience wrapper that sets up a Tokio runtime and invokes the caller's async main function if no arg0 dispatch occurs.
