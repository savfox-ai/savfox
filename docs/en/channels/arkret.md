# Arkret Agent channel

Savfox's Arkret Agent runtime handles authorized message subscription and replies.
It does not send encrypted presence heartbeats over Arkret Signal. The current v1
wire has a distinct authenticated Agent sender branch, but its shape alone does
not authorize a runtime to send. Savfox has not yet closed the end-to-end checks
for the current accepted Agent key, lifecycle and controller authority, the exact
MLS leaf, or recipient eligibility. An Agent key must not impersonate an ordinary
device through a synthetic device ID.

Pairing therefore does not request `ak.self.signal.command.send.v1`. A connected
Savfox listener indicates local runtime connectivity, not a remotely published
Agent online-presence claim. Agent presence can be enabled only after the protocol
authority evidence and complete sender and recipient checks are implemented and
verified end to end.

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
