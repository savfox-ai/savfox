const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { requiredFiles, verifyWebBundle } = require('./verify-web-bundle.cjs');

function fixture(callback) {
  const parent = process.platform === 'win32' ? 'D:/BuildCache/savfox-quality-fixes/tmp' : os.tmpdir();
  fs.mkdirSync(parent, { recursive: true });
  const dir = fs.mkdtempSync(path.join(parent, 'savfox-web-bundle-'));
  try {
    for (const file of requiredFiles) {
      fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
      fs.writeFileSync(path.join(dir, file), file.endsWith('.wasm') ? Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]) : 'fixture');
    }
    fs.writeFileSync(path.join(dir, 'index.html'), '<script type="module">import init from "/wasm/savfox_gateway_glory.js";</script>');
    callback(dir);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
}

test('complete bundle passes release validation', () => fixture(dir => verifyWebBundle(dir)));
test('missing and empty release assets fail validation', () => {
  for (const file of requiredFiles) fixture(dir => {
    fs.writeFileSync(path.join(dir, file), '');
    assert.throws(() => verifyWebBundle(dir), /Incomplete/);
    fs.unlinkSync(path.join(dir, file));
    assert.throws(() => verifyWebBundle(dir), /Incomplete/);
  });
});
test('corrupt WASM and missing bootstrap CSS fail validation', () => {
  fixture(dir => {
    fs.writeFileSync(path.join(dir, 'wasm/savfox_gateway_glory_bg.wasm'), 'not wasm');
    assert.throws(() => verifyWebBundle(dir), /WebAssembly/);
  });
  fixture(dir => {
    fs.appendFileSync(path.join(dir, 'index.html'), '<link href="/assets/missing.css">');
    assert.throws(() => verifyWebBundle(dir), /Missing bootstrap asset/);
  });
});
