const fs = require('node:fs');
const path = require('node:path');

// Keep workflow inputs as data; never interpolate them into shell source.
const version = (process.env.RELEASE_VERSION ?? '').replace(/^v/, '');
if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) throw new Error('Invalid stable release version');
const manifest = fs.readFileSync(path.resolve(__dirname, '../../Cargo.toml'), 'utf8');
const section = manifest.match(/^\[workspace\.package\]\r?\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
const current = section?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (current !== version) throw new Error(`Requested ${version}, but this commit builds ${current}`);
console.log(`VERSION=${version}`);
