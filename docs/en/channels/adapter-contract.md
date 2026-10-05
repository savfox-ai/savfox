# Channel Adapter Contract

This document defines the minimum contract for channel adapters under `crates/channels` and their gateway integration.

## Scope

A channel adapter is responsible for platform translation, not core agent behavior.

## Required responsibilities

Each adapter must define or support:
- inbound event normalization
- outbound message delivery
- identity mapping and stable external IDs
- auth/credential requirements
- retry and failure semantics
- idempotency or dedupe behavior where the platform can redeliver
- tracing/logging hooks sufficient for production debugging

## Gateway boundary

Gateway runtime owns:
- session lookup and creation
- agent routing policy
- long-lived service orchestration
- approval and execution policy

Adapters should not bypass that boundary.

## Configuration expectations

Each adapter should document:
- required secrets and tokens
- optional tuning flags
- webhook or polling mode expectations
- known platform limits

## Reliability rules

Adapters should make duplicate delivery and partial failure behavior explicit. If the platform can redeliver messages, the adapter must either dedupe or document exactly where dedupe happens.

## Acknowledgment and execution ownership

Define platform receipt, local durable inbox completion, coordinator admission,
and reply submission separately. "Dispatched" cannot describe all four outcomes,
and an in-memory queue cannot establish durable execution ownership. Validate the
trusted route before completing the source item. If retry ownership transfers to
another queue, persist the complete work and idempotency key there first. Temporary
failure retains source retry; terminal failure retains a named diagnostic. Do not
clear dedupe to automatically replay an executed instruction. Document crash
recovery limits where only an in-memory coordinator exists; do not promise
exactly-once model side effects.

Arkret routes retain the SDK independent `stream_ref`, full account, Realm, Strand,
and original request Event ID. Ordinary Realm/Topic and native Sidecar cannot share
inferred routes even when they have the same name or source Strand. Regressions
cover missing coordinates, unreadable durable files, source retry after rejected
admission, and real replies to the original conversation.

## Stability levels

Use one of these labels in docs and reviews:
- Stable
- Beta
- Experimental

New adapters should start as `Experimental` unless there is already operational evidence to promote them.

## Testing expectations

At minimum:
- unit coverage for parsing and normalization helpers
- contract-level tests for signature/auth verification when applicable
- gateway integration smoke coverage for adapter registration and basic dispatch
