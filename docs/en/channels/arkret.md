# Arkret Agent channel

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
its capability-floor completion, including both Event and Seal frontier operations.
Service scopes use exact versioned operation IDs, for example
`ak.self.events.read.scan.v1`; content actions such as `ak.event.read` stay unchanged.
Online chat does not request delayed-publication leases by default.

Editing a saved pairing preserves its exact scope array. Missing, old unversioned
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
