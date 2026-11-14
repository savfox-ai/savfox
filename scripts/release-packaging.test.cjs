const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const { packageNpm } = require('../nodejs/scripts/package-npm.cjs');
const repo = path.resolve(__dirname, '..');

function temporary(t) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'savfox-release-'));
  t.after(() => {
    if (path.dirname(base) !== path.resolve(os.tmpdir())) throw new Error('Invalid fixture cleanup path');
    fs.rmSync(base, { recursive: true, force: true });
  });
  return base;
}

for (const [bump, version] of [['patch', '0.3.2'], ['minor', '0.4.0'], ['major', '1.0.0']]) {
  test(`release ${bump} keeps local requirements and locked packages consistent`, t => {
    const root = temporary(t);
    fs.writeFileSync(path.join(root, 'Cargo.toml'), '[workspace]\nmembers=["a","b"]\nresolver="3"\n[workspace.package]\nversion="0.3.1"\nedition="2024"\n[workspace.dependencies]\na = { path="a", version="0.3.1" }\n');
    for (const name of ['a', 'b']) {
      fs.mkdirSync(path.join(root, name, 'src'), { recursive: true });
      fs.writeFileSync(path.join(root, name, 'src/lib.rs'), 'pub fn fixture() {}\n');
      fs.writeFileSync(path.join(root, name, 'Cargo.toml'), `[package]\nname="${name}"\nversion.workspace=true\nedition.workspace=true\n${name === 'b' ? '[dependencies]\na.workspace=true\n' : ''}`);
    }
    const lock = spawnSync('cargo', ['generate-lockfile', '--offline'], { cwd: root, encoding: 'utf8' });
    assert.equal(lock.status, 0, lock.stderr);
    const result = spawnSync('python', [path.join(repo, 'scripts/prepare-release.py'), bump, '--root', root], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), version);
    const checked = spawnSync('cargo', ['check', '--locked', '--offline'], { cwd: root, encoding: 'utf8' });
    assert.equal(checked.status, 0, checked.stderr);
    const manifest = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
    assert.ok(manifest.includes(`version = "${version}"`));
  });
}

function npmFixture(t) {
  const base = temporary(t);
  const root = path.join(base, 'nodejs');
  fs.mkdirSync(path.join(root, 'bin'), { recursive: true });
  for (const file of ['platforms.json', 'package.json', 'bin/savfox.js']) fs.copyFileSync(path.join(repo, 'nodejs', file), path.join(root, file));
  const platforms = JSON.parse(fs.readFileSync(path.join(root, 'platforms.json'), 'utf8'));
  for (const p of platforms) {
    const dir = path.join(base, 'dist', p.target, 'savfox');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, p.os === 'win32' ? 'savfox.exe' : 'savfox'), 'fixture binary');
  }
  return { root, base, platforms };
}

test('npm main package has exact platform dependencies and no bundled binaries', t => {
  const { root, platforms } = npmFixture(t);
  const output = packageNpm('v0.3.2', root);
  const main = JSON.parse(fs.readFileSync(path.join(output, 'savfox/package.json'), 'utf8'));
  assert.equal(main.version, '0.3.2');
  assert.deepEqual(main.optionalDependencies, Object.fromEntries(platforms.map(p => [`@savfox/savfox-${p.platform}`, '0.3.2'])));
  assert.equal(fs.existsSync(path.join(output, 'savfox/vendor')), false);
  assert.equal(fs.existsSync(path.join(output, 'savfox-win32-arm64')), false);
  for (const p of platforms) {
    const binary = path.join(output, `savfox-${p.platform}`, 'vendor', p.target, 'savfox', p.os === 'win32' ? 'savfox.exe' : 'savfox');
    assert.equal(fs.readFileSync(binary, 'utf8'), 'fixture binary');
  }
});

test('missing artifact stops packaging before replacing previous packages', t => {
  const { root, base, platforms } = npmFixture(t);
  const binary = path.join(base, 'dist', platforms[0].target, 'savfox/savfox');
  fs.unlinkSync(binary);
  fs.mkdirSync(path.join(base, 'npm-packages'));
  fs.writeFileSync(path.join(base, 'npm-packages/sentinel'), 'preserve');
  assert.throws(() => packageNpm('0.3.2', root));
  assert.equal(fs.readFileSync(path.join(base, 'npm-packages/sentinel'), 'utf8'), 'preserve');
});

test('untrusted release version is rejected before packaging', t => {
  const { root } = npmFixture(t);
  assert.throws(() => packageNpm('0.3.2; echo injected', root), /valid release version/);
});

test('failed release resolution restores manifest and lock bytes including CRLF', t => {
  const root = temporary(t);
  const original = '[workspace]\r\nmembers=["a"]\r\n[workspace.package]\r\nversion="0.3.1"\r\n[workspace.dependencies]\r\na={path="a",version="0.3.1"}\r\n';
  fs.writeFileSync(path.join(root, 'Cargo.toml'), original);
  fs.mkdirSync(path.join(root, 'a/src'), { recursive: true });
  fs.writeFileSync(path.join(root, 'a/src/lib.rs'), '');
  fs.writeFileSync(path.join(root, 'a/Cargo.toml'), '[package]\nname="a"\nversion="0.3.1"\nedition="2024"\n');
  const generated = spawnSync('cargo', ['generate-lockfile', '--offline'], { cwd: root, encoding: 'utf8' });
  assert.equal(generated.status, 0, generated.stderr);
  const lock = fs.readFileSync(path.join(root, 'Cargo.lock'));
  const result = spawnSync('python', [path.join(repo, 'scripts/prepare-release.py'), 'minor', '--root', root], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.equal(fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8'), original);
  assert.deepEqual(fs.readFileSync(path.join(root, 'Cargo.lock')), lock);
});

test('binary builder uses the shared matrix and workspace from an unrelated directory', t => {
  const base = temporary(t);
  const root = path.join(base, 'project');
  const target = path.join(base, 'normal-target');
  for (const dir of ['nodejs/scripts', 'scripts']) fs.mkdirSync(path.join(root, dir), { recursive: true });
  const source = fs.readFileSync(path.join(repo, 'nodejs/scripts/build-binaries.sh'), 'utf8');
  assert.equal(/\bmapfile\b/.test(source), false, 'must support macOS system Bash 3.2');
  fs.writeFileSync(path.join(root, 'nodejs/scripts/build-binaries.sh'), source.replaceAll('\r', ''));
  fs.copyFileSync(path.join(repo, 'nodejs/platforms.json'), path.join(root, 'nodejs/platforms.json'));
  fs.writeFileSync(path.join(root, 'scripts/cargo-target.cjs'), `exports.cargoTargetDirectory = () => ${JSON.stringify(target)};`);
  const mock = path.join(base, 'cargo.cjs');
  fs.writeFileSync(mock, `const fs=require('node:fs'),path=require('node:path'); const args=process.argv.slice(2); const target=args[args.indexOf('--target')+1]; const directory=path.join(process.env.SAVFOX_REVIEW_TARGET,target,'release');fs.mkdirSync(directory,{recursive:true});fs.writeFileSync(path.join(directory,target.includes('windows')?'savfox.exe':'savfox'),'fixture');fs.appendFileSync(process.env.SAVFOX_REVIEW_LOG,JSON.stringify({target,cwd:process.cwd()})+'\\n');`);
  const log = path.join(base, 'builds.jsonl');
  const bash = process.env.SAVFOX_TEST_BASH || (process.platform === 'win32' ? 'C:/Program Files/Git/bin/bash.exe' : 'bash');
  const script = path.join(root, 'nodejs/scripts/build-binaries.sh').replaceAll('\\', '/');
  const result = spawnSync(bash, ['--noprofile', '--norc', '-c', `cargo() { node "$SAVFOX_REVIEW_MOCK" "$@"; }; rustup() { node -e 'for(const p of require(process.argv[1])) console.log(p.target+" (installed)")' "$SAVFOX_REVIEW_MATRIX"; }; OSTYPE=darwin23; source "${script}"`], {
    cwd: base, encoding: 'utf8', env: { ...process.env, SAVFOX_REVIEW_TARGET: target, SAVFOX_REVIEW_LOG: log, SAVFOX_REVIEW_MOCK: mock, SAVFOX_REVIEW_MATRIX: path.join(root, 'nodejs/platforms.json') },
  });
  assert.equal(result.status, 0, result.stderr);
  const builds = fs.readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(builds.map(build => build.target), require('../nodejs/platforms.json').filter(p => p.os !== 'win32').map(p => p.target));
  assert.ok(builds.every(build => path.resolve(build.cwd) === path.resolve(root)));
});

for (const propagated of [true, false]) {
  test(`npm publication gates the main package on registry propagation: ${propagated}`, t => {
    const base = temporary(t);
    fs.mkdirSync(path.join(base, 'nodejs'));
    fs.copyFileSync(path.join(repo, 'nodejs/platforms.json'), path.join(base, 'nodejs/platforms.json'));
    const mock = path.join(base, 'npm.cjs');
    fs.writeFileSync(mock, `const fs=require('node:fs'); const args=process.argv.slice(2);fs.appendFileSync(process.env.SAVFOX_REVIEW_LOG,JSON.stringify(args)+'\\n');if(args[0]==='view'){if(process.env.SAVFOX_REVIEW_PROPAGATED==='true') console.log(process.env.RELEASE_VERSION);else process.exitCode=1;}`);
    const workflow = fs.readFileSync(path.join(repo, '.github/workflows/publish-npm.yml'), 'utf8');
    const block = workflow.match(/      - name: Publish to npm\r?\n        run: \|\r?\n([\s\S]*?)        env:/)?.[1];
    assert.ok(block);
    const script = block.split('\n').map(line => line.replace(/^          /, '')).join('\n');
    const log = path.join(base, 'commands.jsonl');
    const bash = process.env.SAVFOX_TEST_BASH || (process.platform === 'win32' ? 'C:/Program Files/Git/bin/bash.exe' : 'bash');
    const result = spawnSync(bash, ['--noprofile', '--norc'], {
      cwd: base, encoding: 'utf8',
      input: `set -euo pipefail\nnpm() { node "$SAVFOX_REVIEW_MOCK" "$@"; }; sleep() { return 0; };\n${script}`,
      env: { ...process.env, RELEASE_VERSION: '0.3.2', SAVFOX_REVIEW_LOG: log, SAVFOX_REVIEW_MOCK: mock, SAVFOX_REVIEW_PROPAGATED: String(propagated) },
    });
    const commands = fs.readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line));
    const mainIndex = commands.findIndex(args => args[0] === 'publish' && args.at(-1) === './npm-packages/savfox');
    if (propagated) {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(mainIndex, commands.length - 1);
      assert.equal(commands.filter(args => args[0] === 'view').length, require('../nodejs/platforms.json').length);
    } else {
      assert.notEqual(result.status, 0);
      assert.equal(mainIndex, -1);
    }
  });
}
