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
