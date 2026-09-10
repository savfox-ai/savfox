# Terminal Agent Next Tasks

This checklist tracks the next production slice for using external Codex and
Claude terminal agents through the top-level `terminal` branch.

## Goals

- Give external CLIs an explicit, self-contained turn context.
- Preserve Savfox session continuity while allowing vendor CLIs to keep their
  own state when configured to do so.
- Support image attachments without pushing large base64 payloads through shell
  arguments.
- Stream observable terminal output while the one-shot process is still running.
- Keep the implementation portable; do not make tmux a cross-platform runtime
  dependency.

## Checklist

- [x] Save this follow-up task list in the repository.
- [x] Add a terminal input package that includes session id, previous turns,
  current request, and attachment metadata.
- [x] Persist image attachments for terminal turns under the terminal session
  directory and expose stable file paths in the input package.
- [x] Add template variables for structured terminal input:
  `{{conversation_context}}`, `{{attachment_manifest}}`, and
  `{{terminal_input_json}}`.
- [x] Pass chat image attachments from the Sessions UI to terminal agents
  instead of rejecting them client-side.
- [x] Persist terminal user message attachments in the Savfox rollout.
- [x] Stream stdout/stderr chunks from one-shot terminal agents before the
  process exits.
- [x] Keep parsed final output and terminal timeline behavior backward
  stable.
- [x] Document the new terminal agent context and attachment contract.
- [x] Add targeted tests for context packaging, attachment persistence, and
  streaming events.
- [x] Connect native Windows ConPTY / Unix PTY to managed terminal RPC and
  managed chat, with reusable processes and explicit turn completion.
- [x] Add direct input, interruption, manual completion, output recovery, and
  close controls to Sessions.
- [x] Verify live chat output, concurrent socket input, and browser reattachment
  against a real Windows terminal.

## Deferred

- Vendor-specific Codex and Claude profile parsers beyond plain text/JSONL/
  sentinel.
- Approval bridging for vendor CLI tool prompts.
- Enforced read-only native sandbox for terminal agents.
