# Gateway access and download limits

Gateway scopes and session ownership are separate checks. A token with read or
write scope does not gain access to a Claude session owned by a different
credential. REST session previews filter inaccessible sessions before applying
the requested limit. Session patch, reset, and delete endpoints return `403`
for another credential's private session, matching history and WebSocket RPC.
Sessions without a private Claude ownership record retain their existing shared
operator access policy.

`POST /api/sessions/{session_id}` accepts `label`, `pinned`, and `archived`.
Pinned and archived state defaults to `false` for older metadata and persists
across gateway restarts. Both REST previews and `sessions.list` expose the two
flags. A missing session returns `404`; invalid field types return `400`.
If the metadata write fails, patch returns `500` and leaves the cached values
unchanged. A successful patch confirms the metadata was durably replaced.

Browser profile creation/replacement, deletion, and default selection require
`operator_admin`, as does `browser.download`, which can select a local output
directory. Profiles can select executables, extensions, process arguments,
proxies, and profile directories, so administrators must treat them as trusted
process configuration. Ordinary browsing can continue to use configured
profiles with its existing read/write scopes. The coarse `operator` role
includes administrative access.

Direct `browser.request` calls pin validated DNS addresses for every HTTP method,
including requests with custom headers or bodies. These guarded clients bypass
environment HTTP proxies so a proxy cannot independently re-resolve the target.
Redirects are disabled for custom requests; plain GET redirects are validated
and pinned at each hop.

Outbound response bodies are read incrementally with a byte limit. Generic
guarded downloads default to 5 MiB; media downloads use the largest configured
media allowance while reading, then enforce the detected MIME type's allowance
before storing the file. Each default media allowance is 5 MiB. Browser requests
have a 1 MiB response limit for both browser-context and direct transports.
An oversized response fails rather than returning a partially read success.
The limit also applies when `Content-Length` is missing or inaccurate. A
browser-context size rejection does not retry the request through the direct
transport, avoiding repeated POST side effects. Successful browser text output
still has its separate 100,000-character display limit.
