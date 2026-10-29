const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const root = path.resolve(__dirname, '..');
const bash = process.env.SAVFOX_TEST_BASH || (process.platform === 'win32' ? 'C:/Program Files/Git/bin/bash.exe' : 'bash');
const source = fs.readFileSync(path.join(root, 'install.sh'), 'utf8').replace(/^\uFEFF/, '').replaceAll('\r', '');
const tempRoot = process.platform === 'win32' ? 'D:/BuildCache/savfox-quality-fixes/tmp' : os.tmpdir();
fs.mkdirSync(tempRoot, { recursive: true });
const sourceDir = fs.mkdtempSync(path.join(tempRoot, 'savfox-installer-source-'));
const sourcePath = path.join(sourceDir, 'install.sh');
fs.writeFileSync(sourcePath, source);
test.after(() => fs.rmSync(sourceDir, { recursive: true, force: true }));

function run(script, env = {}) {
  return spawnSync(bash, ['--noprofile', '--norc'], {
    input: `set -euo pipefail\nsource "$SAVFOX_TEST_INSTALLER"\n${script}\n`,
    encoding: 'utf8', cwd: root,
    env: { ...process.env, TMPDIR: tempRoot, SAVFOX_TEST_INSTALLER: sourcePath, ...env }, windowsHide: true,
  });
}

for (const [system, arch, target, ext] of [
  ['Linux', 'x86_64', 'x86_64-unknown-linux-gnu', 'tar.gz'],
  ['Linux', 'aarch64', 'aarch64-unknown-linux-gnu', 'tar.gz'],
  ['Darwin', 'x86_64', 'x86_64-apple-darwin', 'tar.gz'],
  ['Darwin', 'arm64', 'aarch64-apple-darwin', 'tar.gz'],
  ['MINGW64_NT', 'x86_64', 'x86_64-pc-windows-msvc', 'zip'],
]) {
  test(`installer selects release target ${target}`, () => {
    const result = run(`uname() { if [ "$1" = -s ]; then echo ${system}; else echo ${arch}; fi; }; detect_platform; get_download_url 0.3.1`);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), `https://github.com/savfox-ai/savfox/releases/download/v0.3.1/savfox-v0.3.1-${target}.${ext}`);
  });
}

test('latest follows the effective URL and keeps diagnostics out of the URL', () => {
  const result = run(`
    curl() {
      [ "\${@: -1}" = '%{url_effective}' ] || return 9
      echo https://github.com/savfox-ai/savfox/releases/tag/v0.3.1
    }
    TARGET=x86_64-unknown-linux-gnu; ARCHIVE_EXT=tar.gz
    get_download_url latest
  `);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), 'https://github.com/savfox-ai/savfox/releases/download/v0.3.1/savfox-v0.3.1-x86_64-unknown-linux-gnu.tar.gz');
  assert.match(result.stderr, /Latest version: v0.3.1/);
});

test('latest rejects an unresolved release URL and invalid versions', () => {
  for (const script of [
    'curl() { echo https://github.com/savfox-ai/savfox/releases/latest; }; get_download_url latest',
    'get_download_url ../main',
  ]) {
    const result = run(script);
    assert.notEqual(result.status, 0);
    assert.equal(result.stdout, '');
  }
});

test('unsupported Windows ARM64 fails before attempting a download', () => {
  const result = run('uname() { if [ "$1" = -s ]; then echo MINGW64_NT; else echo arm64; fi; }; detect_platform');
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /No release binary/);
});

for (const scenario of ['valid', 'mismatch', 'missing-checksum', 'missing-archive']) {
  test(`installation verifies the downloaded archive: ${scenario}`, () => {
    const dir = fs.mkdtempSync(path.join(tempRoot, 'savfox-install-'));
    try {
      const result = run(`
        fixture=$(cd "$FIXTURE_DIR" && pwd)
        mkdir -p "$fixture/package"
        printf '#!/bin/sh\\necho fixture-savfox\\n' > "$fixture/package/savfox"
        tar -czf "$fixture/release.tar.gz" -C "$fixture/package" savfox
        if command -v sha256sum >/dev/null; then sha256sum "$fixture/release.tar.gz"; else shasum -a 256 "$fixture/release.tar.gz"; fi > "$fixture/checksum"
        if [ "$SCENARIO" = mismatch ]; then printf '%064d archive\\n' 0 > "$fixture/checksum"; fi
        download() {
          case "$1" in
            *.sha256) [ "$SCENARIO" != missing-checksum ] && cp "$fixture/checksum" "$2" ;;
            *) [ "$SCENARIO" != missing-archive ] && cp "$fixture/release.tar.gz" "$2" ;;
          esac
        }
        INSTALL_DIR="$fixture/installed"; BINARY_NAME=savfox; ARCHIVE_EXT=tar.gz
        install https://example.test/release.tar.gz
      `, { FIXTURE_DIR: dir, SCENARIO: scenario });
      const installed = path.join(dir, 'installed', 'savfox');
      if (scenario === 'valid') {
        assert.equal(result.status, 0, result.stderr);
        assert.match(fs.readFileSync(installed, 'utf8'), /fixture-savfox/);
      } else {
        assert.notEqual(result.status, 0, result.stderr);
        assert.equal(fs.existsSync(installed), false);
        if (scenario === 'mismatch') assert.match(result.stderr, /SHA256 checksum mismatch/);
      }
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
}
