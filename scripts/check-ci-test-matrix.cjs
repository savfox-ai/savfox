const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

function missingTargetedPackages(workflow, packages, root) {
  const excluded = workflow.match(/!crates\/\{([^}]+)\}\/\*\*/)?.[1].split(',');
  if (!excluded) throw new Error('Workspace test exclusion list was not found');
  const targeted = workflow.match(/^  test-targeted:[\s\S]*?(?=^  [a-z][\w-]*:|(?![\s\S]))/m)?.[0];
  if (!targeted) throw new Error('Targeted test job was not found');
  const testCommands = [...targeted.matchAll(/\bcargo\s+(?:nextest\s+run|test)\b[^\r\n]*(?:\\\r?\n[^\r\n]*)*/g)].map(match => match[0]).join('\n');
  const tested = new Set([...testCommands.matchAll(/\s-p\s+([\w-]+)/g)].map(match => match[1]));
  const names = new Map(packages.map(pkg => [path.resolve(pkg.manifest_path), pkg.name]));
  return excluded.flatMap(directory => {
    const name = names.get(path.resolve(root, 'crates', directory, 'Cargo.toml'));
    if (!name) throw new Error(`Excluded crate ${directory} has no workspace package`);
    if (!tested.has(name)) return [name];
    const pkg = packages.find(pkg => pkg.name === name);
    if (name === 'savfox-common' && pkg.features?.elapsed && !testCommands.split('\n').some(command => /\s-p\s+savfox-common(?:\s|$)/.test(command) && command.includes('--all-features'))) {
      return ['savfox-common (--all-features)'];
    }
    return [];
  });
}

function missingFeatureChecks(workflow, packages) {
  const clippy = workflow.match(/^  clippy:[\s\S]*?(?=^  [a-z][\w-]*:|(?![\s\S]))/m)?.[0] ?? '';
  const clippyCommands = [...clippy.matchAll(/\bcargo\s+clippy\b[^\r\n]*/g)].map(match => match[0]);
  const missing = clippyCommands.some(command => ['--workspace', '--all-targets', '--all-features'].every(flag => command.includes(flag)))
    ? [] : ['workspace Clippy (--all-targets --all-features)'];
  const targeted = workflow.match(/^  test-targeted:[\s\S]*?(?=^  [a-z][\w-]*:|(?![\s\S]))/m)?.[0] ?? '';
  const testCommands = [...targeted.matchAll(/\bcargo\s+(?:nextest\s+run|test)\b[^\r\n]*/g)].map(match => match[0]);
  for (const name of ['savfox-gateway-server', 'savfox-gateway-shared', 'savfox-channels']) {
    const pkg = packages.find(pkg => pkg.name === name);
    if (pkg?.features?.arkret && !testCommands.some(command => new RegExp(`\\s-p\\s+${name}(?:\\s|$)`).test(command) && command.includes('--all-features'))) {
      missing.push(`${name} (--all-features)`);
    }
  }
  return missing;
}

function missingAuditCheck(workflow) {
  const audit = workflow.match(/^  audit:[\s\S]*?(?=^  [a-z][\w-]*:|(?![\s\S]))/m)?.[0] ?? '';
  const commands = [...audit.matchAll(/\bcargo\s+audit\b[^\r\n]*/g)].map(match => match[0]);
  return commands.some(command => /--deny(?:=|\s+)warnings(?:\s|$)/.test(command))
    ? [] : ['advisory audit (--deny warnings)'];
}

if (require.main === module) {
  const root = path.resolve(__dirname, '..');
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8' }));
  const workflow = fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const missing = [...missingTargetedPackages(workflow, metadata.packages, root), ...missingFeatureChecks(workflow, metadata.packages), ...missingAuditCheck(workflow)];
  if (missing.length) throw new Error(`CI regression coverage missing: ${missing.join(', ')}`);
  console.log('Targeted package tests, optional Gateway features, all-feature Clippy, and strict advisory audit are covered');
}
module.exports = { missingTargetedPackages, missingFeatureChecks, missingAuditCheck };
