# Gateway Web Build and Release

The browser application uses Glory's semantic UI and Sapid widgets. It lives in
`crates/gateway-glory`; shared browser/backend models remain in
`crates/gateway-shared`. Authentication and the existing HTTP/WebSocket protocol
remain owned by the gateway.

## Dependencies and storage

The workspace consumes sibling checkouts at `../../glory-rs/glory` and
`../../glory-rs/sapid`. In the Windows development layout these are
`D:/Works/glory-rs/glory` and `D:/Works/glory-rs/sapid`. CI restores pinned source
revisions with `.github/actions/ui-sources`.

Install the Rust `wasm32-unknown-unknown` target and a `wasm-bindgen-cli` version
matching the `wasm-bindgen` entry in `Cargo.lock`. Set `WASM_BINDGEN` or pass
`-Bindgen <path>` to select a CLI. The build validates the version before bundling.

Windows compilation, Cargo downloads, temporary files, stamps and frontend
bundles use `D:/BuildCache`. WSL uses a verified `/mnt/d` mount. The script checks
`cargo metadata --no-deps --format-version 1` against `CARGO_TARGET_DIR` before
compilation. Remote CI uses runner storage. It does not start Docker.

## Local workflows

```powershell
just web-build                 # debug WASM bundle and gateway static sync
just web-build-release         # release bundle and static sync
just gateway                   # build frontend, then run gateway
just gateway-release
```

For development, run `just gateway-frontend` and `just gateway-backend` in separate
terminals. Node.js runs the frontend server on port 18080, watches application and
sibling framework sources, rebuilds WASM and reloads connected pages. It proxies
`/api`, `/health` and WebSocket `/ws` to port 18881. Set `SAVFOX_DEV_BACKEND` to
override the proxy target. `just web-serve` and `just gateway-skip-web` remain
available.

## Build boundary

`scripts/build-web.ps1` delegates to the Node.js engine in `scripts/build-web.cjs`.
It runs Cargo and wasm-bindgen, copies Sapid CSS and application
assets, and publishes a complete bundle to `crates/gateway-server/static`. The
intermediate bundle is in `D:/BuildCache/savfox-glory/web/<profile>`. `-SkipCopy`
builds only that bundle; `-Force` rebuilds and resyncs it. Content fingerprints
include the application, locales, shared types, workspace lockfile, Glory, Sapid,
stylesheet and build tooling. A failed build never replaces the gateway bundle.

Generated static files are ignored by Git. Do not edit them by hand. The gateway
embeds them at compilation and serves `index.html` for application routes. The
bootstrap HTML uses absolute URLs so refreshing nested routes works. Unhashed
assets are served with `no-cache`.

For shell-independent builds, run `node scripts/build-web.cjs`; its options are
`--release`, `--skip-copy`, `--force` and `--bindgen <path>`.

## Verification and deployment

Run `cargo test -p savfox-gateway-glory -p savfox-gateway-shared`, a WASM build, and the gateway's ignored
`built_frontend_assets_are_served_with_spa_fallback` test after generating the
bundle. Browser acceptance must cover authentication, navigation, RPC requests,
chat streaming and reconnect behavior.

`node scripts/check-web.cjs` runs the built bundle through Chromium against
deterministic gateway fixtures. Set `SAVFOX_PLAYWRIGHT` to a Playwright installation
when it is not installed in Glory's sibling browser test directory. It covers
authentication, deep links, session streaming, approval/question events, provider
login cancellation, channel edits, record refresh and mobile navigation. CI runs
this independently of native tests. `node scripts/check-web-proxy.cjs` verifies the
actual development server's HTTP/WebSocket proxy and static bundle paths.

Docker builds require named source contexts:

```powershell
docker buildx build --build-context glory=D:/Works/glory-rs/glory --build-context sapid=D:/Works/glory-rs/sapid -t savfox .
```

Before running this locally, verify the Docker builder actually stores data on
D: or a remote host. Docker Desktop's existing C: virtual disk does not satisfy
that rule. CI builds Docker on remote runners and supplies the same source contexts.
