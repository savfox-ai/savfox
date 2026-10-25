#!/usr/bin/env node
// Build Glory WASM without a framework-specific CLI. All generated data stays on D: locally.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');
const root = path.resolve(__dirname, '..');
const args = process.argv.slice(2);
const has = name => args.some(arg => arg.toLowerCase() === name.toLowerCase());
const release = has('--release') || has('-Release');
const skipCopy = has('--skip-copy') || has('-SkipCopy');
const force = has('--force') || has('-Force');
const profile = release ? 'release' : 'debug';
const web = path.join(root, 'crates/gateway-glory');
const destination = path.join(root, 'crates/gateway-server/static');
const glory = path.resolve(root, '../../glory-rs/glory');
const sapid = path.resolve(root, '../../glory-rs/sapid');
const env = { ...process.env };
let cache;
if (process.platform === 'win32') {
  cache = 'D:/BuildCache/savfox-glory';
  env.CARGO_HOME = 'D:/BuildCache/cargo';
  env.CARGO_TARGET_DIR = path.join(cache, 'target');
} else if (fs.existsSync('/mnt/d')) {
  const mount = spawnSync('findmnt', ['-no', 'SOURCE,FSTYPE,OPTIONS', '-T', '/mnt/d'], { encoding: 'utf8' });
  if (mount.status !== 0 || !/(?:^|\s|=)D:(?:[\\/]|\s|$)/i.test(mount.stdout)) throw new Error('/mnt/d must be a verified D: drive mount');
  cache = '/mnt/d/BuildCache/savfox-glory';
  env.CARGO_HOME = path.join(cache, 'cargo');
  env.CARGO_TARGET_DIR = path.join(cache, 'target');
} else if (env.CI) {
  cache = path.join(root, 'target/web-build');
  env.CARGO_TARGET_DIR = path.join(root, 'target');
} else throw new Error('Local builds require D: or a verified /mnt/d mount; remote CI is also supported.');
fs.mkdirSync(path.join(cache, 'tmp'), { recursive: true });
env.TEMP = env.TMP = path.join(cache, 'tmp');
env.CARGO_BUILD_JOBS ||= '4';

function run(command, values, capture = false) {
  const result = spawnSync(command, values, { cwd: root, env, windowsHide: true, encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit', maxBuffer: 64 * 1024 * 1024 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} ${values.join(' ')} failed (${result.status})${capture ? '\n' + result.stderr : ''}`);
  return result.stdout;
}
function files(input) {
  if (fs.statSync(input).isFile()) return [input];
  return fs.readdirSync(input, { withFileTypes: true }).flatMap(entry => {
    if (['target', 'dist', '.git', 'node_modules'].includes(entry.name)) return [];
    return files(path.join(input, entry.name));
  });
}
function fingerprint(inputs, version) {
  const hash = crypto.createHash('sha256').update(profile).update(version);
  for (const file of inputs.flatMap(files).sort()) hash.update(file).update(fs.readFileSync(file));
  return hash.digest('hex');
}
const required = ['index.html', 'assets/main.css', 'assets/sapid.css', 'wasm/savfox_gateway_glory.js', 'wasm/savfox_gateway_glory_bg.wasm'];
const ready = directory => required.every(file => fs.existsSync(path.join(directory, file)));
const matches = (stamp, hash) => fs.existsSync(stamp) && fs.readFileSync(stamp, 'utf8').trim() === hash;

try {
  const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--format-version', '1'], true));
  if (path.resolve(metadata.target_directory) !== path.resolve(env.CARGO_TARGET_DIR)) throw new Error(`Unexpected Cargo target: ${metadata.target_directory}`);
  console.log(`==> Cargo target verified: ${metadata.target_directory}`);
  const bindgenIndex = args.findIndex(arg => /^(--bindgen|-Bindgen)$/i.test(arg));
  const localTool = path.join(cache, 'tools/bin', process.platform === 'win32' ? 'wasm-bindgen.exe' : 'wasm-bindgen');
  const bindgen = (bindgenIndex >= 0 ? args[bindgenIndex + 1] : undefined) || env.WASM_BINDGEN || (fs.existsSync(localTool) ? localTool : 'wasm-bindgen');
  const version = run(bindgen, ['--version'], true).trim();
  const inputs = [__filename, path.join(__dirname, 'build-web.ps1'), path.join(root, 'Cargo.toml'), path.join(root, 'Cargo.lock'), path.join(root, '.cargo/config.toml'),
    path.join(web, 'Cargo.toml'), path.join(web, 'src'), path.join(web, 'assets'), path.join(web, 'locales'), path.join(web, 'index.html'),
    path.join(root, 'crates/gateway-shared'), path.join(root, 'crates/utils/src'), path.join(root, 'crates/utils/Cargo.toml'),
    path.join(glory, 'Cargo.toml'), path.join(glory, 'crates'), path.join(sapid, 'Cargo.toml'), path.join(sapid, 'README.md'), path.join(sapid, 'src'), path.join(sapid, 'styles')];
  const output = path.join(cache, 'web', profile);
  const stamp = path.join(cache, `${profile}-frontend.stamp`);
  const before = fingerprint(inputs, version);
  if (force || !ready(output) || !matches(stamp, before)) {
    run('cargo', ['build', '-p', 'savfox-gateway-glory', '--lib', '--target', 'wasm32-unknown-unknown', ...(release ? ['--release'] : [])]);
    const expected = fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8').match(/^name = "wasm-bindgen"\r?\nversion = "([^"]+)"/m)?.[1];
    if (version !== `wasm-bindgen ${expected}`) throw new Error(`Install wasm-bindgen-cli ${expected}; selected ${version}. Use WASM_BINDGEN or --bindgen.`);
    const staging = path.join(cache, 'web', `${profile}-staging`);
    if (!path.resolve(staging).startsWith(path.resolve(cache) + path.sep)) throw new Error('Bundle must stay inside build cache');
    fs.rmSync(staging, { recursive: true, force: true });
    fs.mkdirSync(path.join(staging, 'wasm'), { recursive: true });
    run(bindgen, [path.join(env.CARGO_TARGET_DIR, 'wasm32-unknown-unknown', profile, 'savfox_gateway_glory.wasm'), '--target', 'web', '--out-name', 'savfox_gateway_glory', '--out-dir', path.join(staging, 'wasm')]);
    fs.cpSync(path.join(web, 'assets'), path.join(staging, 'assets'), { recursive: true });
    fs.copyFileSync(path.join(web, 'index.html'), path.join(staging, 'index.html'));
    fs.copyFileSync(path.join(sapid, 'styles/sapid.css'), path.join(staging, 'assets/sapid.css'));
    if (!ready(staging)) throw new Error('Incomplete WASM bundle');
    const backup = output + '-previous';
    for (const directory of [output, backup]) {
      if (!path.resolve(directory).startsWith(path.resolve(cache) + path.sep)) throw new Error('Generated bundle must stay inside build cache');
    }
    fs.rmSync(backup, { recursive: true, force: true });
    if (fs.existsSync(output)) fs.renameSync(output, backup);
    try { fs.renameSync(staging, output); }
    catch (error) { if (fs.existsSync(backup)) fs.renameSync(backup, output); throw error; }
    fs.rmSync(backup, { recursive: true, force: true });
    // Stamp the inputs observed before compilation. Concurrent edits trigger the next build.
    fs.writeFileSync(stamp, before);
    console.log(`==> Glory + Sapid frontend built: ${output}`);
  } else console.log('==> Frontend inputs unchanged; reusing WASM bundle');
  if (!skipCopy) {
    const syncStamp = path.join(cache, 'static.stamp');
    if (force || !ready(destination) || !matches(syncStamp, before)) {
      if (path.resolve(destination) !== path.join(root, 'crates/gateway-server/static')) throw new Error('Unexpected static destination');
      fs.rmSync(destination, { recursive: true, force: true });
      fs.cpSync(output, destination, { recursive: true });
      fs.writeFileSync(path.join(destination, '.gitkeep'), '');
      fs.writeFileSync(syncStamp, before);
    }
    console.log(`==> Gateway static bundle: ${destination}`);
  }
} catch (error) { console.error(error.message); process.exitCode = 1; }
