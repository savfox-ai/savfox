# Execution cancellation and session recovery

`savfox exec` exits with status `130` when Ctrl+C cancels execution, including before initial submission or after core becomes idle. It waits for primary and sub-agent session cleanup, including their running subprocesses. With `--json`, cancellation produces one `turn.failed` terminal with `error.message` set to `Turn interrupted`; if a terminal was already delivered before the signal, it is preserved without appending another. A sub-agent being interrupted does not end the primary session.

`savfox exec resume <SESSION_ID_OR_NAME> <PROMPT>` resumes an existing recorded session. A missing ID or name is an error. `resume --last <PROMPT>` fails when no session matches the current directory and provider filters; `--all` disables the directory filter. History lookup failures are propagated. These failures do not create a new session. To begin a new conversation, use `savfox exec <PROMPT>`.

The app-server `turn/interrupt` request uses `sessionId` and `turnId`. It cancels only that active turn. The `{}` response acknowledges cancellation cleanup, and `turn/completed` reports `status: "interrupted"`. If the turn is already complete, absent, or replaced, the server returns an invalid-request error (`-32600`) promptly and preserves the current turn. Clients must handle this normal completion/cancellation race.
