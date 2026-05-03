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
