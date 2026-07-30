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
