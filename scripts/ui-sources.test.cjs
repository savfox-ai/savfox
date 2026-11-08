const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { lockedUiSources, resolvedUiSources } = require('./ui-sources.cjs');

const registry = 'registry+https://github.com/rust-lang/crates.io-index';
const lockfile = `version = 4
[[package]]
name = "glory"
version = "0.7.0"
source = "${registry}"
checksum = "${'a'.repeat(64)}"
[[package]]
name = "sapid"
version = "0.7.1"
source = "${registry}"
checksum = "${'b'.repeat(64)}"
`;
const metadata = () => ({ packages: [
  { name: 'glory', version: '0.7.0', source: registry, manifest_path: path.resolve('cargo/registry/src/glory-0.7.0/Cargo.toml') },
  { name: 'sapid', version: '0.7.1', source: registry, manifest_path: path.resolve('cargo/registry/src/sapid-0.7.1/Cargo.toml') },
] });

test('locked crates.io packages resolve their own assets with LF or CRLF locks', () => {
  for (const content of [lockfile, lockfile.replaceAll('\n', '\r\n')]) {
    const sources = resolvedUiSources(metadata(), content);
    assert.equal(sources.glory, path.resolve('cargo/registry/src/glory-0.7.0'));
    assert.equal(sources.sapid, path.resolve('cargo/registry/src/sapid-0.7.1'));
    assert.equal(sources.locked.sapid.version, '0.7.1');
    assert.equal(sources.locked.sapid.checksum, 'b'.repeat(64));
  }
});

test('local path overrides, Git and other registries are rejected', () => {
  for (const source of [null, `git+https://example.com/glory.git#${'a'.repeat(40)}`, 'registry+https://example.com/index']) {
    const resolved = metadata();
    resolved.packages[0].source = source;
    assert.throws(() => resolvedUiSources(resolved, lockfile), /outside its locked crates.io version/);
  }
});

test('different or duplicated resolved package versions are rejected', () => {
  const resolved = metadata();
  resolved.packages[0].version = '0.7.1';
  assert.throws(() => resolvedUiSources(resolved, lockfile), /outside its locked crates.io version/);
  resolved.packages.push(metadata().packages[0]);
  assert.throws(() => resolvedUiSources(resolved, lockfile), /outside its locked crates.io version/);
});

test('missing, duplicated, wrong-version or non-crates.io UI sources are rejected', () => {
  assert.throws(() => lockedUiSources(lockfile.replace('name = "sapid"', 'name = "other"')), /exactly one sapid/);
  assert.throws(() => lockedUiSources(lockfile + '\n[[package]]\nname = "glory"\n'), /exactly one glory/);
  assert.throws(() => lockedUiSources(lockfile.replace('version = "0.7.0"', 'version = "0.7.1"')), /crates.io version 0.7.0/);
  assert.throws(() => lockedUiSources(lockfile.replace(registry, 'git+https://example.com/glory.git')), /crates.io version/);
  assert.throws(() => lockedUiSources(lockfile.replace(registry, 'registry+https://example.com/index')), /crates.io version/);
});

test('missing or malformed registry checksums are rejected', () => {
  for (const replacement of ['', 'checksum = "short"', `checksum = "${'z'.repeat(64)}"`]) {
    assert.throws(() => lockedUiSources(lockfile.replace(`checksum = "${'a'.repeat(64)}"`, replacement)), /locked checksum/);
  }
});


