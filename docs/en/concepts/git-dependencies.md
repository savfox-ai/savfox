# Git Dependency Policy

Savfox intentionally carries a small set of git dependencies and git patches. That flexibility must stay governed.

## Why git dependencies exist here

Current use cases include:
- tracking upstream fixes not yet released on crates.io
- keeping protocol- or transport-specific forks aligned with Savfox behavior
- pinning exact revisions for compatibility-sensitive integrations

Examples in the workspace include OpenTelemetry git patches, eventsource-related forks, and a few platform/integration dependencies pinned to upstream commits or branches.

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

## Matrix appservice security fork

`matrix-bot-sdk` 0.2.4 still depends on Salvo 0.89, which retains vulnerable
zstd 0.13 packages. Savfox uses Salvo 1.0.1 and pins the maintenance fork
`https://github.com/arkret-org/matrix-bot-sdk` at
`bc5c32e935134e2ff2363616fab1eb1ba3f090be` (`arkret/salvo1-security`).
The fork imports only the public crates.io 0.2.4 source archive and preserves
the Apache-2.0 license; it contains no private upstream Git history.
All appservice/client functionality remains enabled.

Savfox integration maintainers own upgrades and compatibility regressions.
Return to crates.io when an upstream release supports the maintained Salvo
version without the affected compression dependency chain, after running
Matrix appservice route/client tests and gateway integration checks.
