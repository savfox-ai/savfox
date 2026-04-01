# savfox-core

This crate implements the business logic for Savfox. It is designed to be used by the various Savfox UIs written in Rust.

## Dependencies

Note that `savfox-core` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.savfox` read-only.

### Linux

Expects the binary containing `savfox-core` to run the equivalent of `savfox sandbox linux` (legacy alias: `savfox debug landlock`) when `arg0` is `savfox-linux-sandbox`. See the `savfox-arg0` crate for details.

### All Platforms

Expects the binary containing `savfox-core` to simulate the virtual `apply_patch` CLI when `arg1` is `--savfox-run-as-apply-patch`. See the `savfox-arg0` crate for details.
