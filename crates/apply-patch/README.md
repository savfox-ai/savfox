# savfox-apply-patch

A library and standalone binary for parsing and applying structured patch files to the local filesystem. The patch format uses `*** Begin Patch` / `*** End Patch` delimiters with support for adding, deleting, updating, and moving files. Update hunks use context-based matching (similar to unified diffs) with fuzzy matching for Unicode punctuation normalization.

The crate exposes `apply_patch()` for direct patch application, `parse_patch()` for parsing patch text into `Hunk` structures, and `unified_diff_from_chunks()` for computing standard unified diffs from parsed chunks. It also provides `maybe_parse_apply_patch_verified()` which validates shell command invocations to determine if they correspond to `apply_patch` calls and pre-computes the resulting file changes without writing to disk.

The `apply_patch` binary is deployed via the arg0 dispatch mechanism (see `savfox-arg0`), allowing it to be invoked as a standalone CLI tool without requiring a separate executable installation. The crate includes `APPLY_PATCH_TOOL_INSTRUCTIONS` -- a bundled instruction document describing how LLM agents should use the tool.
