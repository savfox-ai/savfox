const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { missingTargetedPackages, missingFeatureChecks, missingAuditCheck } = require('./check-ci-test-matrix.cjs');
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

test('default-feature commands do not cover optional Gateway features or all-feature Clippy', () => {
  const optionalPackages = ['savfox-gateway-server', 'savfox-gateway-shared', 'savfox-channels'].map(name => ({ name, features: { arkret: [] } }));
  const base = `jobs:
  clippy:
    steps:
      - run: cargo clippy --workspace --all-targets -- -D warnings
  test-targeted:
    steps:
      - run: cargo nextest run -p savfox-gateway-server -p savfox-gateway-shared -p savfox-channels
  coverage:
    steps:
      - run: cargo test -p savfox-gateway-server -p savfox-gateway-shared -p savfox-channels --all-features
`;
  assert.deepEqual(missingFeatureChecks(base, optionalPackages), [
    'workspace Clippy (--all-targets --all-features)',
    ...optionalPackages.map(pkg => `${pkg.name} (--all-features)`),
  ]);
  const fixed = base.replace('--all-targets --', '--all-targets --all-features --')
    .replace('run -p savfox-gateway-server', 'run --all-features -p savfox-gateway-server');
  for (const content of [fixed, fixed.replaceAll('\n', '\r\n')]) assert.deepEqual(missingFeatureChecks(content, optionalPackages), []);
  const partial = fixed.replace('run --all-features -p savfox-gateway-server -p savfox-gateway-shared -p savfox-channels', 'run --all-features -p savfox-gateway-server -p savfox-gateway-shared');
  assert.deepEqual(missingFeatureChecks(partial, optionalPackages), ['savfox-channels (--all-features)']);
});

test('advisory audit must fail on new warnings in its own job', () => {
  const base = `jobs:
  audit:
    steps:
      - run: cargo audit
  licenses:
    steps:
      - run: cargo audit --deny warnings
`;
  assert.deepEqual(missingAuditCheck(base), ['advisory audit (--deny warnings)']);
  for (const command of ['cargo audit --deny warnings', 'cargo audit --deny=warnings']) {
    const fixed = base.replace('run: cargo audit\n', `run: ${command}\n`);
    for (const content of [fixed, fixed.replaceAll('\n', '\r\n')]) assert.deepEqual(missingAuditCheck(content), []);
  }
});
