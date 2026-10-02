# Arkret Agent channel

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
The runtime requires the actual session grant to match its requested operation set;
a narrower grant cannot run the full configured listener and an over-grant is rejected.
The cached last successful session scope is diagnostic history, not current key or
provision authority, and never authorizes adding permissions to a saved Agent.

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

### Applet outbound identity

Applet mode requires `bot_account_id` as the complete Arkret `AccountId` object
(`principal_id` and `station_id`) retained from accepted provisioning. The retired
`botActorId` / `bot_actor_id` string is rejected; a service DID or destination URL
cannot supply an omitted account Station.

Configure the resolvable `service_did`, matching `serviceId`, `trust_domain`, and
an explicit service `verification_method`. `keyRef` selects that service signing
key. Outbound bridge configuration also requires the existing `namespaces` and
`managed_actor_authoring` object with `principal_endpoint` and
`key_encryption_key_hex`; use the actual installed identity-custody settings.
The bridge preserves the Bot Account as `actor_id` and signs as the Applet service
in `executed_by`. Ordinary sends use retained local authority context.
