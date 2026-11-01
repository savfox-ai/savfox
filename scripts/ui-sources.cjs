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


// Same opt-in name as Glory's official project CLI. Defaults remain owned by
// each caller, including the existing remote-CI output layout.
function configuredBuildRoot(env = process.env, options = {}) {
  if (!Object.hasOwn(env, 'GLORY_BUILD_ROOT')) return null;
  const raw = env.GLORY_BUILD_ROOT;
  if (typeof raw !== 'string' || !raw || raw.trim() !== raw || raw.replaceAll('\\', '/').split('/').includes('..')) {
    throw new Error('GLORY_BUILD_ROOT must be a nonempty absolute approved path without parent traversal');
  }
  const platform = options.platform || process.platform;
  const io = options.fs || fs;
  const spawn = options.spawnSync || require('node:child_process').spawnSync;
  const paths = platform === 'win32' ? path.win32 : path.posix;
  const portable = value => value.replaceAll('\\', '/').replace(/^\/\/\?\//, '').toLowerCase();
  const approved = value => platform === 'win32'
    ? portable(value).startsWith('d:/buildcache/')
    : portable(value).startsWith('/mnt/d/buildcache/');
  if (!paths.isAbsolute(raw) || !approved(raw)) throw new Error('GLORY_BUILD_ROOT must stay under D:/BuildCache or verified /mnt/d/BuildCache');
  if (platform !== 'win32') {
    const mount = spawn('findmnt', ['-no', 'SOURCE,FSTYPE,OPTIONS', '-T', '/mnt/d'], { encoding: 'utf8' });
    if (mount.error || mount.status !== 0 || !/(?:^|\s|=)D:(?:[\\/]|\s|$)/i.test(mount.stdout || '')) {
      throw new Error('GLORY_BUILD_ROOT requires a verified Windows D: mount');
    }
  }
  const selected = paths.normalize(raw);
  let ancestor = selected;
  while (!io.existsSync(ancestor)) {
    const parent = paths.dirname(ancestor);
    if (parent === ancestor) throw new Error('GLORY_BUILD_ROOT has no existing ancestor');
    ancestor = parent;
  }
  const resolved = io.realpathSync(ancestor);
  // Validate the destination before creating any directory; a D:-named
  // junction whose real target is C: (or another unapproved root) is refused.
  const realRoot = paths.resolve(resolved, paths.relative(ancestor, selected));
  if (!approved(realRoot)) throw new Error('GLORY_BUILD_ROOT resolves outside the approved D: cache root');
  return platform === 'win32' ? realRoot.replace(/^\\\\\?\\/, '') : realRoot;
}

module.exports = { lockedUiSources, resolvedUiSources, configuredBuildRoot };

if (require.main === module) {
  const root = path.resolve(__dirname, '..');
  const sources = lockedUiSources(fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8'));
  for (const [name, value] of Object.entries(sources)) console.log(`${name} main: ${value.revision}`);
}
