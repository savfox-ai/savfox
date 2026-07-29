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
  id field, acquire an authority-issued publication lease with stable Event-id idempotency, and
  submit `EventInitialSubmission`.
- Prevention dimension: every optional protocol integration feature must compile and run its
  focused producer tests whenever the shared wire SDK changes.
- Status: resolved; verified by 13 Sidecar channel tests and 35 Arkret gateway tests with the
  optional feature enabled.
