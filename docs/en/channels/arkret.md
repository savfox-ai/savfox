# Arkret Agent channel

Resetting the Station database creates a new Station identity when its identity
state is removed. This differs from logging in again against the same Station.
Savfox's local channel directory survives that reset: an old saved pairing does
not authorize the new Station, even at the same URL. Runtime discovery checks
the saved Station audience before loading a runtime key or submitting an Agent
proof and reports `station_identity_changed` on mismatch. Configure a fresh
pairing for the current Station; this diagnostic does not revoke or erase the
previous pairing's key material.

New channels default to `interactive_chat`; explicit `task_delivery` settings
retain checkpoint delivery. A fresh owned-Agent Direct Conversation waits for
the accepted participant binding before enabling ordinary chat. A provisional
resolver result with durable peer MLS admission permits only founding binding
completion, not provisional application messages (v1 contact-and-direct-
conversation §7.2). The runtime must still consume its Welcome before binding;
it must not wait for the first chat message to join. Validate fresh pairing and
the first send on isolated empty databases, without changing the delivery mode
in the test or repairing the conversation manually.

On Windows, Arkret runtime seeds and crypto-state wrapping keys use the shared
SDK's CurrentUser DPAPI-protected encrypted vault. They do not consume Windows
Credential Manager entries. The `keyring` key reference remains an opaque
platform-protected location; the key never enters channel JSON. Earlier Windows
Credential Manager entries are not imported: pair again to authorize a fresh
runtime key. macOS and Linux continue to use their native keyrings.

The comparison code contains eight decimal digits, displayed in two groups of four with leading zeros preserved. Compare the Inkson and Savfox codes before approval. Permission purposes and authorization duration stay visible; authorization references, runtime-key identifiers, and full error chains are under expandable technical details.

After a status-query failure, **Check approval** resumes polling the same request. The Station determines expiry. Stopping the local wait does not revoke an approved request or discard its runtime key, and a consumed pairing link need not be submitted again.

Approval submission and status polling validate a pending pairing candidate without
requiring `authorizedEventRef`. That reference is returned after Inkson approves
the runtime key; it is still mandatory before the saved channel can run. Pairing
errors include the underlying validation reason.

Pairing resolution, runtime key approval submission, and approval status polling
send the exact versioned `Arkret-Operation` header required by the Station.
If pairing reports `operation_selector_required`, rebuild and restart the gateway
with this support before retrying.

In Agent mode, paste the Inkson pairing link and click **Start pairing**. Controller
Account ID and Runtime key DID URL are internal values obtained automatically from
the resolver's `runtime_identity` object. Savfox then generates the local key and
requests Inkson approval. A resolver returning only the older six-field bootstrap
must be upgraded; Savfox reports the missing identity instead of requiring manual
identity entry or leaving the button disabled. Existing saved bindings remain usable.

Savfox's Arkret Agent runtime handles authorized message subscription, replies,
and encrypted presence heartbeats. Agent Signals omit the device id. The digest
of the current runtime's raw Ed25519 key identifies the sequence endpoint, and
that runtime key also signs the Signal proof. Each Realm's MLS nonce and payload
sequence are persisted before HTTP submit, so an uncertain request may skip a
value but cannot reuse a nonce or roll the sequence back.

The pairing bootstrap carries the Agent's stable `ak:did_core:*` identity in
`agent_id`. The runtime-key DID URL is a separate, complete
`did:<method>:...#<fragment>` value supplied by the Agent/controller identity flow.
Savfox verifies through the shared DID-method adapter that the DID controller
projects to the bootstrap `agent_id`; it never constructs a verification method
as `agent_id#device_id` or treats a local/session device identifier as the Agent
MLS actor.

After approval, the Agent MLS endpoint is exactly the tuple
`(agent_id, verificationMethod, authorizedEventRef)`. Session grants and every
KeyPackage, claim, Welcome, receipt, and consume operation must retain that
binding. A different Agent subject, runtime key, or authorization Event is
rejected instead of falling back to a human-device identity.

New pairings request `ak.self.signal.command.send.v1`. The Station still verifies
Agent classification, lifecycle, its sole controller, the current runtime and
session authorization, and the exact MLS leaf. Recipients accept and project
presence only under the verified current raw-key digest. No synthetic device id
is created for an Agent key.

New pairing-request candidates use the shared Arkret SDK operation registry and
its capability-floor completion, including standard committed-event stream subscription, scanning, and secure messaging.
Service scopes use exact versioned operation IDs, for example
`ak.self.committed_event.read.scan.v1`; content actions such as `ak.event.read` stay unchanged.
Online chat does not request delayed-publication leases by default.

Editing delivery settings preserves the saved pairing, including redacted comparison codes and runtime-key references restored by the server. Use `interactive_chat` for chat replies and `task_delivery` for task delivery. Editing a saved pairing preserves its exact scope array. Missing, old unversioned
or `query` aliases are rejected, not upgraded. A new candidate is only a request:
the Station must still check immutable provision, current key and session ceilings.
The actual session grant must retain every configured service operation and remain
within the requested scope. The authority may remove content actions when no resource
grant applies; accepted native Sidecar consumption checks the refreshed committed-event
read operation together with the exact Agent AccountId and audience. Resource grants
and action policy still independently authorize writes and replies. Missing service
operations and widened grants are rejected.
The cached last successful session scope is diagnostic history, not current key or
provision authority, and never authorizes adding permissions to a saved Agent.

A saved delivery-mode change applies to subsequent inbound messages in existing
conversations. Each mode uses a separate local execution session, sharing only
verified remote conversation history. Switching to chat does not publish or resume
the private task rollout; switching back resumes the original task session.

Use the service-reported recovery: provision a new Agent for a deficient immutable
provision scope; reauthorize the key within that ceiling for a key-scope deficiency;
refresh the session within both ceilings for a session-scope deficiency. These scope
checks do not imply that the separate Agent identity/runtime migration is complete.
An invalid saved binding cannot be reported as successfully disconnected. If its
old identity or scope prevents safe revocation, Savfox retains local state and
requires controller-side recovery. Only a confirmed unbind clears the saved scope
and allows the same empty channel slot to form a new pairing candidate.

Retiring a previous pairing's KeyPackage pool reads only its inventory and verifies
the owning Agent. Obsolete message IDs in that file do not prevent the new runtime
from starting. Remote revocation must be acknowledged before the local retirement
marker is written; unrelated state and other Agents' keys are preserved.

Owned-Agent direct conversations require a verified accepted MLS Welcome before
the runtime can decrypt messages or publish encrypted presence. Savfox verifies
the governance closure and the controller/Agent leaf attribution, persists and
reads back the joined state, then signs the recipient durable receipt and submits KeyPackage consume. Direct Conversation binding depends on that consumption; confirming a durable Welcome must never wait for the binding. The
accepted governance binding selects the content encryption scheme; exporter AEAD
replies reserve and persist their counter before submission, including across
runtime restarts.

Standard MLS content authenticates the complete sender ActorId credential, including its Station, rather than a device/runtime verification-method selector. Signed RealmGenesis purpose identifies a Direct Conversation, so ordinary messages trigger without a mention. Decryption and unbound Direct Conversation triggers stay in the durable inbox; only a verified accepted stream read can recover previously deferred content. Replies carry the exact accepted participant binding before encryption and signing. A fresh verified Welcome advances the same group after endpoint replacement without resetting its epoch or reviving an expired claim.

A pending recipient delivery does not block processing later deliveries. Cumulative ACK and the durable queue cursor remain before the first incomplete delivery. Welcome allocation removes the matching single-use KeyPackage from available private inventory before claim admission, so expired allocations still trigger replenishment. Replacing the initial pool entry preserves prior records; repairing a missing public record requires a verified claim and its exact private bundle already held locally.

## Reply modes, acknowledgment, and recovery

`interactive_chat` sends the model's reply body to the original remote conversation.
`task_delivery` retains complete execution output in a private local session and
publishes public checkpoints through the task-delivery path. Missing ordinary reply
text does not establish that task execution failed. This is a Savfox product setting,
not an Arkret permission, MLS state, or additional protocol profile. Switching modes
does not widen authority or replay already acknowledged historical messages.

An ordinary message crosses authenticated independent-stream delivery, the durable
inbox, trusted route preflight, coordinator admission, model execution, and reply
submission. Preflight requires the saved config, account, Realm, Strand, SDK
`stream_ref`, original request Event ID, and sender. Display names or the parent
Realm cannot supply private-stream coordinates. Missing metadata or an unreadable
binding store must reject coordinator admission while the inbox can still retry;
the worker uses the same validation. In-memory coordinator admission is neither
model completion nor a second durable execution queue. This path does not promise
exactly-once model execution or replies after a process crash. Inspect the model
session and accepted outbound reply instead of treating inbound/dispatched counts
as proof that the complete chain succeeded.

The local inbox completion checkpoint and Arkret's cumulative to-device ACK are
separate boundaries. The latter follows v1 `client-sync` section 10.1 and must not
advance beyond an incompletely persisted Welcome/DeviceMessage. Do not combine
them into one receipt or cursor.

The gateway WebSocket first completes typed `connectChallenge`/`connect`
authentication, then accepts bare requests with a root `jsonrpc` discriminator.
No `type` wrapper is needed. Field order cannot change routing even when `params`
is large or contains non-ASCII text and `jsonrpc` is last. A nested key or quoted
text cannot select the RPC route. The 1 MiB frame limit, typed parameter validation,
and authorization checks remain in effect.

| Symptom | Diagnosis and recovery |
| --- | --- |
| Re-paired channel shows an old session failure or an old connection | Runtime health belongs to the exact account in the saved pairing. Retired accounts cannot make the current channel healthy or unhealthy. A successful connection test only checks reachability; verify the current listener and accepted reply separately. |
| Inkson says Encryption state moved while sending | A concurrent MLS transition can refuse the old encryption context. Once verified current state is ready, send the restored plaintext draft as a freshly authored message; do not replay its refused ciphertext or reset the group. An unknown submission outcome instead remains pending for exact-byte recovery. |
| Route error `missing field streamRef`, before model execution | Preserve the unreadable file and diagnostics. Do not invent missing coordinates. An operator may archive explicitly retired development data and let new authenticated inbound events establish routes. Do not clear dedupe to replay old instructions. |
| Missing keyring entry at startup; the channel is not listening | Check the process's Windows user, protected-store namespace, and exact `keyRef`. An approval record does not contain the private key; restarting or resaving config cannot regenerate it. |
| The original runtime private key is actually lost | Use Replace runtime for the same Agent in Inkson, generate a raw key never used for that Agent, and complete controller approval. Preserve the scope ceiling, identity, and lifecycle. A newly provisioned namesake is not the original Agent. |
| Reply sent, but the receiver stays at Verifying sender identity | Inspect exact committed Agent signer resolution and MLS leaf authorization. One reply must initiate resolution and revalidate its pending row without a later account frame or reload. Never show unverified plaintext first. |
| Public Agent runs the model, but reply submission returns `capability_denied` for `ak.message.create` | Public mode, global Reply as agent, and participation are separate gates; none creates a Realm Capability Grant. In that Realm's Inkson Settings → Security & MLS → Manage permissions, an authorized issuer must explicitly grant `ak.message.create` to the exact Agent account. Preserve other permission ceilings. After a definitive refusal, use a new request to verify the reply; do not replay the refused ciphertext. |

Public group replies use validated SDK mention nodes with the complete
`subject_account_id`, including its Station. Select the Agent in Inkson's mention
picker; a literal `@me/aa` or local Savfox alias does not address it. Encrypted
messages retain these targets after MLS decryption and use the same mention
trigger as plaintext. Current public mode and existing participation/authority
checks still apply, and replies stay in the request's shared discussion.
An exact present mode row in the authorized Realm snapshot supplies the mode
without adding the separate exact-current service operation to an accepted
runtime scope. Its selector, complete account, Realm stream, revision and head
must agree. A missing snapshot row cannot establish the never-written default.
Temporary failures reading the exact mode leave the inbound event pending;
they must not acknowledge it as an intentionally ignored request. A later mode
change does not replay already acknowledged messages.

An equivalent replacement retains the original Direct group and binding and uses
ordinary Remove/Add/Welcome to converge the runtime endpoint. It does not replace
other human devices, recover lost private keys or MLS private state, or guarantee
recovery of data protected by a lost wrapping key. Preserve identity, history,
wrapped crypto, and recovery material. Clearing storage, resetting epochs, or
repeating Genesis cannot repair pending state. Use unbind cleanup only after the
controller explicitly authorizes unbinding and the remote operation is confirmed.

### Applet management and execution identity

A Service-only installation requires an HTTPS `baseUrl` bound by the accepted
registration epoch. The URL supplies discovery, authoring and signed completion
management APIs. It never subscribes to group Events or Signals. The management
transaction receiver verifies RFC9421 Station signatures and no longer requires
an Applet bearer session. Outbound management requests carry SDK Service HTTP
signatures; their native Event producer proofs remain independent.

An installation does not create a default Bot. A runtime that executes as a Bot
must configure an independently accepted `bot_account_id`, a complete Arkret
`AccountId` with `principal_id` and `station_id`. A Service may manage multiple
independent execution Accounts; an execution configuration selects one exact
Account. An absent Bot permits Service management only. Synthetic `botActorId`
strings and Service-derived Ghost DIDs are rejected or removed. Unimplemented
Ghost mapping queries return no accepted identity.

Configure a resolvable `service_did`, its matching `serviceId`, `trust_domain`,
`verification_method` and actual private `keyRef`. Managed identity custody uses
`managed_actor_authoring` with `principal_endpoint` and
`key_encryption_key_hex`. Creation proves provenance; it does not grant business
permissions. Actual group execution needs current Service parents and terminal
managed-Account children, independent native membership and accepted Devices. A child
whose subject is a Bot or Ghost must be consumed by that actual managed Account;
a Service proof cannot act as the Account through `executed_by`.

Applet-mode Device custody/possession, native queue/Welcome/ACK/MLS/Blob readers,
resolution rotation installation and continuous scope refresh remain unwired.
Legacy management transaction group dispatch is closed. Account-mode Agent native
MLS handling is unchanged; these management updates do not establish a working
Applet group execution runtime.

Fresh managed identity authoring custody is not connected in Savfox Applet mode; the standard author endpoint returns unavailable. Completion requires exact locally frozen creation material, so it cannot bootstrap a new Account by accepting foreign bundles. Bridges supplies the production authoring implementation.

Personal Agent replies always retain their accepted Account and native delivery binding. Applet namespace ownership never redirects those replies through another Bot.
