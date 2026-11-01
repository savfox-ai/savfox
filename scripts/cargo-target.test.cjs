const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { cargoTargetDirectory } = require('./cargo-target.cjs');

function fixture(action) {
  const parent = path.resolve(__dirname, '../artifacts/cargo-target-tests');
  fs.mkdirSync(parent, { recursive: true });
  const root = fs.mkdtempSync(path.join(parent, 'cargo target-'));
  try {
    fs.mkdirSync(path.join(root, 'src'));
    fs.mkdirSync(path.join(root, '.cargo'));
    fs.writeFileSync(path.join(root, 'src/lib.rs'), '');
    fs.writeFileSync(path.join(root, 'Cargo.toml'), '[package]\nname="target-fixture"\nversion="0.1.0"\nedition="2024"\n[workspace]\n');
    fs.writeFileSync(path.join(root, 'Cargo.lock'), 'version=4\n[[package]]\nname="target-fixture"\nversion="0.1.0"\n');
    fs.writeFileSync(path.join(root, '.cargo/config.toml'), '[build]\ntarget-dir="configured-output"\n');
    const env = { ...process.env };
    delete env.CARGO_TARGET_DIR;
    action(root, env);
  } finally {
    assert.equal(path.dirname(path.resolve(root)), parent);
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test('frontend build and server resolve Cargo workspace configuration', () => fixture((root, env) => {
  assert.equal(cargoTargetDirectory(root, env), path.join(root, 'configured-output'));
}));

test('explicit Cargo target overrides configuration without changing the caller environment', () => fixture((root, env) => {
  env.CARGO_TARGET_DIR = path.join(root, 'caller output');
  const before = { ...env };
  assert.equal(cargoTargetDirectory(root, env), env.CARGO_TARGET_DIR);
  assert.deepEqual(env, before);
}));

test('failed Cargo inspection stops path selection', () => fixture((root, env) => {
  fs.writeFileSync(path.join(root, 'Cargo.toml'), 'invalid manifest');
  assert.throws(() => cargoTargetDirectory(root, env));
}));
