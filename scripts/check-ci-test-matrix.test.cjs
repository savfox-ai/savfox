const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { missingTargetedPackages } = require('./check-ci-test-matrix.cjs');
const root = path.resolve('matrix-fixture');
const packages = ['common', 'core'].map(name => ({ name: `savfox-${name}`, manifest_path: path.join(root, 'crates', name, 'Cargo.toml') }));
const workflow = `jobs:
  changes:
    excluded: '!crates/{common,core}/**'
  test-targeted:
    steps:
      - run: cargo build -p savfox-common
      - run: cargo nextest run -p savfox-core
  coverage:
    steps:
      - run: cargo test -p savfox-common
`;
test('dependency compilation and a later coverage job do not count as targeted tests', () => {
  assert.deepEqual(missingTargetedPackages(workflow, packages, root), ['savfox-common']);
});
test('targeted common tests close the exclusion gap for LF and CRLF workflows', () => {
  const fixed = workflow.replace('run -p savfox-core', 'run -p savfox-core -p savfox-common');
  for (const content of [fixed, fixed.replaceAll('\n', '\r\n')]) assert.deepEqual(missingTargetedPackages(content, packages, root), []);
});
test('unknown excluded packages fail the coverage mapping check', () => {
  assert.throws(() => missingTargetedPackages(workflow.replace('{common,core}', '{common,core,new-crate}'), packages, root), /no workspace package/);
});

test('common feature-gated tests must be explicitly enabled in their test command', () => {
  const featurePackages = packages.map(pkg => pkg.name === 'savfox-common' ? { ...pkg, features: { elapsed: [] } } : pkg);
  const fixed = workflow.replace('run -p savfox-core', 'run -p savfox-core -p savfox-common');
  assert.deepEqual(missingTargetedPackages(fixed, featurePackages, root), ['savfox-common (--all-features)']);
  assert.deepEqual(missingTargetedPackages(fixed.replace('run -p savfox-core -p savfox-common', 'run -p savfox-core -p savfox-common --all-features'), featurePackages, root), []);
});
