# Gateway Web Build and Release

The browser application uses Glory's semantic UI and Sapid widgets. It lives in
`crates/gateway-glory`; shared browser/backend models remain in
`crates/gateway-shared`. Authentication and the existing HTTP/WebSocket protocol
remain owned by the gateway.

## Dependencies and storage

The workspace depends on the `main` branches of
`https://github.com/glory-rs/glory.git` and `https://github.com/sapid-ui/sapid.git`.
`Cargo.lock` pins their commits for reproducible local, CI and Docker builds.
Sibling checkouts are not build inputs. Sapid currently declares Glory through a
sibling path; the workspace patches Sapid's Git source to the same Glory Git
dependency so Cargo resolves one copy of Glory without a local path fallback.
To adopt newer upstream commits, run `cargo update -p glory -p sapid`, review the
lockfile changes, then repeat native, WASM and browser verification. The
`.github/actions/ui-sources` action checks the lockfile and reports both revisions.

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
terminals. Node.js runs the frontend server on port 18080, watches application
sources and dependency manifests/lockfile, rebuilds WASM and reloads connected pages. It proxies
`/api`, `/health` and WebSocket `/ws` to port 18881. Set `SAVFOX_DEV_BACKEND` to
override the proxy target. `just web-serve` and `just gateway-skip-web` remain
available.

## Build boundary

`scripts/build-web.ps1` delegates to the Node.js engine in `scripts/build-web.cjs`.
It runs Cargo with `--locked` and wasm-bindgen, copies Sapid CSS from the exact
Git package selected by `cargo metadata` and copies application
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

Choice controls use themed Sapid listboxes so both the trigger and expanded
options follow the application palette. Browser acceptance opens these menus,
checks their contrast and viewport bounds, and verifies pointer/keyboard period
selection against the usage RPC parameters.

Boolean fields use styled Sapid Checkbox widgets, and enable/visibility toggles
use Sapid Switch widgets. Their internal native inputs preserve keyboard and
label activation; keep Sapid styling enabled instead of drawing replacement
controls in page CSS. Shared control sizing and palette belong in
`glory-bridge.css`; page styles should only arrange those controls.


Visual acceptance also runs against a real, already running gateway with
`node scripts/check-web-visual.cjs`. It reads the local gateway token without
printing it and only navigates/read-loads pages. Set `SAVFOX_VISUAL_BASE`,
`SAVFOX_VISUAL_TOKEN_FILE`, and `SAVFOX_VISUAL_REFERENCE` when needed. Screenshots
and an HTML comparison report are written to `D:/BuildCache/savfox-glory/visual-review`.
The audit covers all 18 navigation pages at desktop/mobile sizes in both themes, label
contrast, visible icons, form clipping, independent session scrolling, composer
bounds, and resizing without reloading. Compare the actual screenshots with the
pre-migration Dioxus reference as well as checking the assertions. The shell keeps
the original 56px header, 260px navigation, and 320px desktop session list; the
mobile editor must stop above the bottom navigation.

Docker resolves the same locked Git dependencies and copies Sapid CSS from its
Cargo package. No additional source contexts are required:

```powershell
docker buildx build -t savfox .
```

Before running this locally, verify the Docker builder actually stores data on
D: or a remote host. Docker Desktop's existing C: virtual disk does not satisfy
that rule. CI builds Docker on remote runners.


Use `SAVFOX_VISUAL_VIEWPORTS=1440x900,1120x900,800x560,390x560` with
`SAVFOX_VISUAL_ROUTES=sessions` to check intermediate conversation widths and
short windows. The audit checks editor controls for clipping/overlap and verifies
the search field before and after focus.
