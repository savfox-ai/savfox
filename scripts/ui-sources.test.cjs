const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { lockedUiSources, resolvedUiSources } = require('./ui-sources.cjs');

const glorySource = `git+https://github.com/glory-rs/glory.git?branch=main#${'a'.repeat(40)}`;
const sapidSource = `git+https://github.com/sapid-ui/sapid.git?branch=main#${'b'.repeat(40)}`;
const lockfile = `version = 4
[[package]]
name = "glory"
version = "0.3.1"
source = "${glorySource}"
[[package]]
name = "sapid"
version = "0.1.0"
source = "${sapidSource}"
`;
const metadata = () => ({ packages: [
  { name: 'glory', source: glorySource, manifest_path: path.resolve('cargo/git/glory/crates/glory/Cargo.toml') },
  { name: 'sapid', source: sapidSource, manifest_path: path.resolve('cargo/git/sapid/Cargo.toml') },
] });

test('locked Git main packages resolve their own assets with LF or CRLF locks', () => {
  for (const content of [lockfile, lockfile.replaceAll('\n', '\r\n')]) {
    const sources = resolvedUiSources(metadata(), content);
    assert.equal(sources.glory, path.resolve('cargo/git/glory/crates/glory'));
    assert.equal(sources.sapid, path.resolve('cargo/git/sapid'));
    assert.equal(sources.locked.sapid.revision, 'b'.repeat(40));
  }
});

test('local path overrides and different resolved commits are rejected', () => {
  for (const source of [null, glorySource.replace('a'.repeat(40), 'c'.repeat(40))]) {
    const resolved = metadata();
    resolved.packages[0].source = source;
    assert.throws(() => resolvedUiSources(resolved, lockfile), /outside its locked Git main source/);
  }
});

test('missing, duplicated, unpinned or non-main UI sources are rejected', () => {
  assert.throws(() => lockedUiSources(lockfile.replace('name = "sapid"', 'name = "other"')), /exactly one sapid/);
  assert.throws(() => lockedUiSources(lockfile + '\n[[package]]\nname = "glory"\n'), /exactly one glory/);
  assert.throws(() => lockedUiSources(lockfile.replace('?branch=main#', '?branch=development#')), /branch main/);
  assert.throws(() => lockedUiSources(lockfile.replace(glorySource, glorySource.slice(0, -40))), /locked commit/);
});


const { configuredBuildRoot } = require('./ui-sources.cjs');
const windowsIO = { existsSync: value => value === 'D:\\BuildCache', realpathSync: value => value };
const windowsOptions = { platform: 'win32', fs: windowsIO };
test('missing opt-in keeps existing Windows, WSL and remote-CI cache defaults untouched', () => {
  for (const platform of ['win32', 'linux', 'darwin']) {
    assert.equal(configuredBuildRoot({}, { platform, fs: { existsSync() { throw Error('unexpected filesystem access'); } } }), null);
    assert.equal(configuredBuildRoot({ CI: 'true' }, { platform }), null);
  }
});
test('official GLORY_BUILD_ROOT opt-in gives separate task roots without file creation', () => {
  assert.equal(configuredBuildRoot({ GLORY_BUILD_ROOT: 'D:/BuildCache/task-a/web' }, windowsOptions), 'D:\\BuildCache\\task-a\\web');
  assert.equal(configuredBuildRoot({ GLORY_BUILD_ROOT: 'D:/BuildCache/task-b/web' }, windowsOptions), 'D:\\BuildCache\\task-b\\web');
});
test('unsafe, relative, empty and traversing override paths fail closed', () => {
  for (const raw of ['', ' ', 'relative/task', 'D:relative', 'C:/BuildCache/task', '//host/share/task', 'D:/BuildCache-other/task', 'D:/BuildCache', 'D:/BuildCache/task/../other']) {
    assert.throws(() => configuredBuildRoot({ GLORY_BUILD_ROOT: raw }, windowsOptions));
  }
});
test('existing junction ancestors cannot redirect an approved name to C or outside BuildCache', () => {
  for (const target of ['C:\\Users\\outside', 'D:\\outside']) {
    const io = { existsSync: value => value === 'D:\\BuildCache\\alias', realpathSync: () => target };
    assert.throws(() => configuredBuildRoot({ GLORY_BUILD_ROOT: 'D:/BuildCache/alias/task' }, { platform: 'win32', fs: io }), /outside/);
  }
  const io = { existsSync: value => value === 'D:\\BuildCache\\alias', realpathSync: () => 'D:\\BuildCache\\physical' };
  assert.equal(configuredBuildRoot({ GLORY_BUILD_ROOT: 'D:/BuildCache/alias/task' }, { platform: 'win32', fs: io }), 'D:\\BuildCache\\physical\\task');
});
test('WSL opt-in requires the original explicit D mount proof', () => {
  const fs = { existsSync: value => value === '/mnt/d/BuildCache', realpathSync: value => value };
  const selected = { GLORY_BUILD_ROOT: '/mnt/d/BuildCache/task/web' };
  assert.equal(configuredBuildRoot(selected, { platform: 'linux', fs, spawnSync: () => ({ status: 0, stdout: 'D:\\ 9p rw' }) }), '/mnt/d/BuildCache/task/web');
  for (const response of [{ status: 1, stdout: '' }, { status: 0, stdout: '/dev/sda ext4 rw' }, { status: 0, stdout: 'C:\\ 9p rw' }]) {
    assert.throws(() => configuredBuildRoot(selected, { platform: 'linux', fs, spawnSync: () => response }), /verified Windows D/);
  }
});
