# Regression Review

## 2026-07-29 — Arkret runtime feature drifted behind the current publication kernel

- Surface: `savfox-channels` and `savfox-gateway-server` Arkret signer, Event authoring, outbound
  submission, applet metadata, sync timeline, and queue receipt paths.
- Regression: the optional `arkret` feature still referenced the removed Move signer, the old
  Realm-only operation builder argument, a deleted message-id builder, and bare Event submission.
  The gateway also referenced retired service/applet fields and omitted ordered-log conflict and
  ingress-receipt fields. Default-feature tests did not compile this integration, so the runtime
  producer silently fell behind the shared SDK.
- Detection: Sidecar producer completion-gate verification with
  `cargo test -p savfox-channels --features arkret sidecar`.
- Resolution: migrate to `Ed25519PayloadSigner` and signed `ScopeRef`, remove the retired message
  id field, and submit through a host-owned `ArkretInitialSubmissionProvider` that returns an
  issuer-produced `EventInitialSubmission`. The personal-agent session remains unable to mint its
  own authorization lease.
- Prevention dimension: every optional protocol integration feature must compile and run its
  focused producer tests whenever the shared wire SDK changes.
- Status: resolved; the post-merge focused verification results are recorded in the task handoff.

## 2026-07-30 — Agent unbind confused local KeyPackage ids with canonical refs

- Surface: Arkret Agent replacement and explicit runtime unbind.
- Regression: the crypto store returned its client-generated `keypackage_id` row keys to the
  Principal Server revoke endpoint, whose signed target space is the canonical
  `keypackage_ref`. The server correctly rejected every target. The unbind path then treated
  session, signing, transport, and partial-revoke failures as warnings and erased the local signing
  material and binding anyway.
- Detection: the real two-Savfox replacement gate returned
  `KeyPackage signature target is missing` after the first pairing.
- Resolution: the store now enumerates canonical refs, the revoke caller verifies a complete
  failure-free acknowledgement for the exact requested set, and unbind propagates every
  pre-revocation error before local state or the persisted binding can be cleared.
- Prevention dimension: use distinct types for local storage ids and protocol refs; destructive
  teardown must cross its remote revocation barrier before committing local deletion.
- Status: resolved; covered by the wire-ref regression, Arkret-feature gateway compile, and the
  live Agent replacement gate.

## 2026-08-01 — Agent session rotation omitted the required runtime-key proof

- Surface: long-lived personal-Agent account subscription and MLS KeyPackage pool maintenance.
- Regression: the session provider stored `proof=None` and copied that fallback on refresh. Once
  the short Agent grant reached its refresh window, Garth stopped before the HTTP exchange with
  `session refresh proof is required`; the Agent no longer replenished its one-time KeyPackages,
  and repeated Direct Conversation attempts exhausted the pool.
- Correction: each rotation now mints a fresh, 60-second `agent_key_proof` bound to the prior grant
  hash, Agent principal, stable runtime `device_id`, audience, verification method, one-time
  challenge, and canonical timestamps, then signs it with the authorized runtime key.
- Prevention dimension: the focused test verifies the proof kind, fresh challenges, exact
  bindings, and the Ed25519 signature over the canonical refresh transcript.

## 2026-08-01 — Agent runtime identity key was reused as the session DPoP key

- Surface: initial Agent session-grant exchange, protected requests, and short-lived grant refresh.
- Regression: the transport used the long-lived authorized Agent runtime key for DPoP and left the
  expected DPoP thumbprint unset. This violated protocol key separation and made refresh fail after
  the first grant window with `session refresh DPoP key thumbprint is required`.
- Correction: each runtime now creates a distinct ephemeral session DPoP key, captures its initial
  proof thumbprint, and pins that thumbprint across kickoff, protected requests, and refresh. The
  authorized runtime key is used only for the fresh Agent identity proof.
- Prevention dimension: focused tests assert that the runtime and DPoP public keys differ and that
  refresh proof signing remains bound to the authorized runtime key.
