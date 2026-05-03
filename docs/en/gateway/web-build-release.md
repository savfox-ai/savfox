# Gateway Web Build and Release

This document defines the ownership boundary between the Dioxus frontend and the gateway static bundle.

## Source of truth

- source code lives in `crates/gateway-dioxus`
- shared browser/backend data models live in `crates/gateway-shared`
- deployable static assets are copied into `crates/gateway-server/static`
- `scripts/build-web.ps1` is the canonical build-and-sync entrypoint

## Local workflows

### One-shot build

- `just web-build`
- `just web-build-release`

### Full gateway dev loop

- `just gateway`
- `just gateway-release`

### Split frontend/backend dev loop

- terminal 1: `just gateway-frontend`
- terminal 2: `just gateway-backend`

Compatibility aliases still exist:
- `just web-serve`
- `just gateway-skip-web`

## Script responsibilities

`build-web.ps1` is responsible for:
- tracking relevant frontend inputs
- hashing inputs to a fingerprint
- skipping unnecessary rebuilds
- syncing the build output to the Dioxus `out_dir`
- syncing the same output to `crates/gateway-server/static`

## Release rule

Do not treat `crates/gateway-server/static` as hand-edited source. It is a build artifact destination owned by the sync script.

## CI expectation

CI should validate the frontend independently from native gateway tests. A frontend-only change should still prove the Dioxus build works.

## Failure modes to watch

- frontend source changed but static bundle was not rebuilt
- shared serde models changed but wasm/native compatibility was not revalidated
- local dev used compatibility aliases without realizing the preferred split loop changed
