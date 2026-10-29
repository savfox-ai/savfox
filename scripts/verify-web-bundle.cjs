const fs = require('node:fs');
const path = require('node:path');

const requiredFiles = [
  'index.html', 'assets/main.css', 'assets/sapid.css',
  'wasm/savfox_gateway_glory.js', 'wasm/savfox_gateway_glory_bg.wasm',
];

function verifyWebBundle(directory) {
  for (const file of requiredFiles) {
    const fullPath = path.join(directory, file);
    if (!fs.existsSync(fullPath) || !fs.statSync(fullPath).isFile() || fs.statSync(fullPath).size === 0) {
      throw new Error(`Incomplete gateway Web bundle: ${file}`);
    }
  }
  const html = fs.readFileSync(path.join(directory, 'index.html'), 'utf8');
  if (!html.includes('/wasm/savfox_gateway_glory.js')) throw new Error('Gateway index does not load the Web entrypoint');
  const wasm = fs.readFileSync(path.join(directory, 'wasm/savfox_gateway_glory_bg.wasm'));
  if (!wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))) {
    throw new Error('Gateway WebAssembly is not a version 1 WASM module');
  }
  // Verify all local assets referenced by the bootstrap HTML, including page CSS.
  for (const match of html.matchAll(/(?:href|src)=["'](\/(?:assets|wasm)\/[^"'?#]+)["']/g)) {
    const file = path.resolve(directory, '.' + match[1]);
    if (!file.startsWith(path.resolve(directory) + path.sep) || !fs.existsSync(file) || fs.statSync(file).size === 0) {
      throw new Error(`Missing bootstrap asset: ${match[1]}`);
    }
  }
}

if (require.main === module) {
  verifyWebBundle(path.resolve(process.argv[2] || path.join(__dirname, '../crates/gateway-server/static')));
  console.log('Gateway Web bundle verified');
}
module.exports = { requiredFiles, verifyWebBundle };
