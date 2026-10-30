# Git Dependency Policy

Savfox intentionally carries a small set of git dependencies and git patches. That flexibility must stay governed.

## Why git dependencies exist here

Current use cases include:
- tracking upstream fixes not yet released on crates.io
- keeping protocol- or transport-specific forks aligned with Savfox behavior
- pinning exact revisions for compatibility-sensitive integrations

Examples in the workspace include eventsource-related forks and platform/integration dependencies pinned to upstream commits or branches. OpenTelemetry uses crates.io releases; its previous Git patches have been removed.

## Required metadata for each git dependency

Each git dependency should have a recorded answer for:
1. why crates.io is insufficient right now
2. whether the dependency is a short-term bridge or a long-term fork
3. what upstream release, commit, or issue would let us remove it
4. who owns upgrades or breakage response

## Allowed categories

- exact commit pin for reproducibility
- branch tracking only when there is a clear operational reason
- `[patch.crates-io]` use when the workspace must force a coherent transitive version

## Preferred policy

Prefer, in order:
1. crates.io release
2. exact git revision
3. tracked branch
4. floating upstream main branch only when unavoidable

## Upgrade workflow

When changing a git dependency:
- document the reason in the PR
- note blast radius in the test slice
- prefer a commit pin after validating a branch update
- keep removal opportunities visible in maintenance review

## Dependency upgrade constraints (2026-10-30)

The lockfile uses rmcp 3.8.0, OpenTelemetry 0.33.1 crates and
tracing-opentelemetry 0.34.0 from crates.io. The previous six OpenTelemetry
Git overrides are removed. Upgrades retain these dependency constraints:

| Retained versions | Constraint |
| --- | --- |
| starlark 0.13.0, allocative 0.3.4, allocative_derive 0.3.3 | The compatible Starlark/allocative pair remains pinned. Starlark 0.14.2 requires blake3 exactly 1.8.2, conflicting with Arkret's requirement of at least 1.8.7. Allocative 0.3.4 pins its derive crate exactly. |
| time 0.3.47, time-core 0.1.8, time-macros 0.2.27 | rama-crypto 0.4.0 pins time exactly; time then pins its companion crates. |
| generic-array 0.14.7 | crypto-common 0.1.7 pins this version exactly. Updating it alone fails resolution. |
| pest/pest_derive/pest_generator/pest_meta 2.9.2 | The 2.9.5 family adds pest_stack_guard 1.0.0, which pins psm 0.1.24; the lockfile keeps psm 0.1.32. Updating pest alone fails resolution. |

These versions are not yanked or blocked by the workspace's Rust version.
Updating generic-array or pest by downgrading crypto-common or psm exchanges
one older dependency for another. This upgrade does not claim that every
transitive dependency is on its newest published release or major version.

## Matrix appservice security fork

`matrix-bot-sdk` 0.2.4 still depends on Salvo 0.89, which retains vulnerable
zstd 0.13 packages. Savfox uses Salvo 1.0.1 and pins the maintenance fork
`https://github.com/arkret-org/matrix-bot-sdk` at
`d079403dd100af26d2fe7c5ea83dffa7c547cc5a` (`arkret/salvo1-security`).
The fork imports only the public crates.io 0.2.4 source archive and preserves
the Apache-2.0 license; it contains no private upstream Git history.
All appservice/client functionality remains enabled.

Savfox integration maintainers own upgrades and compatibility regressions.
Return to crates.io when an upstream release supports the maintained Salvo
version without the affected compression dependency chain, after running
Matrix appservice route/client tests and gateway integration checks.
