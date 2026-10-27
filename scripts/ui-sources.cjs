#!/usr/bin/env node
// Resolve UI assets from Cargo's locked Git packages, never sibling checkouts.
const fs = require('node:fs');
const path = require('node:path');
const repositories = {
  glory: 'https://github.com/glory-rs/glory.git',
  sapid: 'https://github.com/sapid-ui/sapid.git',
};

function lockedUiSources(lockfile) {
  const sources = {};
  for (const [name, repository] of Object.entries(repositories)) {
    const packages = lockfile.split(/^\[\[package\]\]\r?$/m)
      .filter(block => new RegExp(`^name = "${name}"\\r?$`, 'm').test(block));
    if (packages.length !== 1) throw new Error(`Cargo.lock must contain exactly one ${name} package`);
    const source = packages[0].match(/^source = "([^"]+)"\r?$/m)?.[1];
    const prefix = `git+${repository}?branch=main#`;
    if (!source?.startsWith(prefix) || !/^[0-9a-f]{40}$/.test(source.slice(prefix.length))) {
      throw new Error(`${name} must use ${repository} branch main with a locked commit`);
    }
    sources[name] = { source, revision: source.slice(prefix.length) };
  }
  return sources;
}

function resolvedUiSources(metadata, lockfile) {
  const locked = lockedUiSources(lockfile);
  const roots = {};
  for (const [name, expected] of Object.entries(locked)) {
    const packages = metadata.packages.filter(pkg => pkg.name === name);
    if (packages.length !== 1 || packages[0].source !== expected.source) {
      throw new Error(`Cargo resolved ${name} outside its locked Git main source; remove path/source overrides`);
    }
    roots[name] = path.dirname(packages[0].manifest_path);
  }
  return { ...roots, locked };
}

module.exports = { lockedUiSources, resolvedUiSources };

if (require.main === module) {
  const root = path.resolve(__dirname, '..');
  const sources = lockedUiSources(fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8'));
  for (const [name, value] of Object.entries(sources)) console.log(`${name} main: ${value.revision}`);
}
