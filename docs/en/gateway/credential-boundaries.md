# Gateway credential boundaries

## Remote model discovery

`models.list` and `models.test` may discover models at a caller-supplied
`base_url`. A custom destination receives only the `api_key` explicitly supplied
in that request. It does not receive saved OpenAI API keys, ChatGPT OAuth tokens,
or the ChatGPT account ID from the gateway.

Persistent provider and model configuration operations (`models.add`,
`models.update`, `models.delete`, `models.deleteAccount`, `models.setdefault`,
`models.import`, and `models.aliases.set`) require `operator_admin`. Ordinary
write authority cannot change a saved credential's destination by importing
over an existing account or changing the selected model. `models.list` remains
readable with read authority; `models.test` retains its write requirement.

Saved OpenAI credentials are used implicitly only for HTTPS requests to
`api.openai.com` on port 443, with the root or `/v1` path. URL credentials,
queries, fragments, alternate ports, and other paths disable this fallback.
For ChatGPT authentication, this trusted OpenAI discovery request is translated
to the administrator-configured ChatGPT Codex endpoint. Existing SSRF policy
and DNS pinning still apply to the resulting destination.

## WebChat session bootstrap

`POST /webchat/session` requires a valid gateway bearer token with `chat` scope
(the coarse `operator` role also includes it). Missing or invalid credentials
return `401`; a valid token without chat authority returns `403`.

The returned session token delegates agent execution for that WebChat session.
The `/webchat/send` and `/webchat/poll/{session_id}` endpoints continue to accept
that session token. Session creation does not grant a separate restricted public
agent: the agent still follows the gateway's configured execution policy.

An embedding application must authorize its own users and create sessions in a
trusted backend. Never publish a gateway bearer token in browser configuration.
Pass only the authorized user's session capability to the widget before loading
`/webchat/widget.js`:

```javascript
window._WEBCHAT_Config = {
  sessionId: session.session_id,
  sessionToken: session.token
};
```

Without a pre-created session, the widget displays a sign-in message and disables
input. Anonymous bootstrap is no longer available. A session token remains a
capability for the lifetime of the in-memory session; revoking the bootstrap
gateway token does not independently revoke already issued session capabilities.

## Provider account files

Gateway provider-file operations accept account IDs containing only ASCII
letters, digits, hyphens, and underscores, excluding Windows reserved device
names. All provider-file loads, saves, deletes, and import destinations use the
same validation. Paths, drive prefixes, UNC paths, dots, and alternate data
streams are rejected before file access. Custom account IDs outside this format
must be renamed before use through the gateway.

This protects remote account parameters. The administrator must still control
the local provider directory and its links; it is not a sandbox against a local
user who can replace files or directory links concurrently.
