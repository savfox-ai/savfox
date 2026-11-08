#!/usr/bin/env node
// Resolve UI assets from Cargo's locked crates.io packages, never sibling checkouts.
const fs = require('node:fs');
const path = require('node:path');
const registry = 'registry+https://github.com/rust-lang/crates.io-index';
const versions = { glory: '0.7.0', sapid: '0.7.1' };

function lockedUiSources(lockfile) {
  const sources = {};
  for (const [name, expectedVersion] of Object.entries(versions)) {
    const packages = lockfile.split(/^\[\[package\]\]\r?$/m)
      .filter(block => new RegExp(`^name = "${name}"\\r?$`, 'm').test(block));
    if (packages.length !== 1) throw new Error(`Cargo.lock must contain exactly one ${name} package`);
    const source = packages[0].match(/^source = "([^"]+)"\r?$/m)?.[1];
    const version = packages[0].match(/^version = "([^"]+)"\r?$/m)?.[1];
    const checksum = packages[0].match(/^checksum = "([^"]+)"\r?$/m)?.[1];
    if (source !== registry || version !== expectedVersion || !/^[0-9a-f]{64}$/.test(checksum || '')) {
      throw new Error(`${name} must use crates.io version ${expectedVersion} with a locked checksum`);
    }
    sources[name] = { source, version, checksum };
  }
  return sources;
}

function resolvedUiSources(metadata, lockfile) {
  const locked = lockedUiSources(lockfile);
  const roots = {};
  for (const [name, expected] of Object.entries(locked)) {
    const packages = metadata.packages.filter(pkg => pkg.name === name);
    if (packages.length !== 1 || packages[0].source !== expected.source || packages[0].version !== expected.version) {
      throw new Error(`Cargo resolved ${name} outside its locked crates.io version; remove path/source overrides`);
    }
    roots[name] = path.dirname(packages[0].manifest_path);
  }
  return { ...roots, locked };
}


module.exports = { lockedUiSources, resolvedUiSources };

if (require.main === module) {
  const root = path.resolve(__dirname, '..');
  const sources = lockedUiSources(fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8'));
  for (const [name, value] of Object.entries(sources)) console.log(`${name} crates.io: ${value.version}`);
}
