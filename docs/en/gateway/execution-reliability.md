# Channel execution reliability

The gateway accepts at most 128 channel tasks at once, including queued and running tasks. It runs at most 16 channel tasks across the process and at most two tasks for the same conversation. Each conversation inbox holds at most 32 waiting messages. Admission does not wait while holding the coordinator registry lock. A full budget rejects admission; durable sources retain rejected events for retry. Idle coordinators retire as soon as their running tasks and inbox are empty, atomically with admission. The coordinator registry also has a hard limit of 128 entries; completed conversations do not accumulate dormant actors.

Native agent invocation has a 120-second execution deadline. A correlated tool approval extends the deadline to 180 seconds; after a decision, the normal deadline resumes. A deadline, provider error, or event-stream failure produces an error even when text has already streamed. The incomplete text is not reported as a completed answer. Cleanup interrupts the exact submitted turn, so an older timeout cannot interrupt a later turn on a persistent session. Temporary and forked sessions are shut down and removed on both success and failure. Persistent sessions remain available for subsequent turns.

For channels that edit streamed messages, successful delivery means that the complete final revision has been acknowledged. The writer retries a failed final revision up to three times using the existing message. If all attempts fail, it requests the ordinary final-send retry path. HTTP and provider business errors from edit APIs propagate to this decision. A partial initial message alone is not considered completed delivery.

Channel response limits apply to the final message, including page markers, overlap context and repeated code fences. Overlap is reduced when necessary to leave useful space for new content and is omitted across code-fence boundaries. Very small custom limits omit page markers when numbering cannot fit alongside source content. Code splitting preserves blank lines and trailing newlines.

## Session persistence and resource lifecycle

Gateway session metadata is stored in `gateway-session-store.sqlite` under the Savfox home directory. The first successful open imports the legacy `gateway-session-store.json` snapshot transactionally and leaves that snapshot unchanged. Subsequent writes update one session row; UI updates are acknowledged only after the durable transaction succeeds. Bulk routing migration and pruning remain transactional. Deleted IDs have tombstones so a failed rollout-file deletion cannot restore a deleted session during cache refresh.

Back up the SQLite database with SQLite's online backup API, or stop the gateway before copying the database and any `-wal`/`-shm` files together. The legacy JSON is a migration source, not an up-to-date backup; downgrading to a version that reads only JSON requires an explicit export. Do not delete the SQLite database to reset the cache: it contains authoritative metadata.

Reset, new-session, deletion and periodic pruning retire the bound core session and clear pending approvals. Creation and retirement for the same logical session are serialized; unrelated sessions can initialize independently.

History reads preserve response/event fallback, message totals and source filtering while retaining only the requested tail and the current JSONL record in memory. Parsing runs on the blocking pool. This still scans the file twice and a single large record can consume substantial memory; indexed history pagination and record-size limits remain future work.

Routing migrations apply version-checked updates only to the rows present in their input snapshot, preserving sessions created while the migration was being computed. The explicit `SessionStore::replace_all` API instead treats IDs missing from its input but present in the cache at invocation time as deletion candidates; use it only when replacing the entire collection is intended.

Core session retirement and SSE cancellation allow up to 30 seconds for shutdown completion. This accommodates the Windows process-tree utility budget of 10 seconds and the user-shell cleanup grace period of 20 seconds. Periodic maintenance retires up to 16 unrelated sessions concurrently; lifecycle operations for the same logical session remain serialized.
