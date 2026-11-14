const fs = require('node:fs');
const path = require('node:path');

function packageNpm(inputVersion, root = path.resolve(__dirname, '..')) {
  const version = (inputVersion ?? '').replace(/^v/, '');
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(version)) {
    throw new Error('A valid release version is required');
  }
  const platforms = JSON.parse(fs.readFileSync(path.join(root, 'platforms.json'), 'utf8'));
  const dist = path.resolve(root, '../dist');
  const output = path.resolve(root, '../npm-packages');
  for (const entry of platforms) {
    const binary = path.join(dist, entry.target, 'savfox', entry.os === 'win32' ? 'savfox.exe' : 'savfox');
    const stat = fs.lstatSync(binary);
    if (!stat.isFile() || stat.size === 0) throw new Error(`Missing or empty binary: ${binary}`);
  }
  if (fs.existsSync(output) && fs.lstatSync(output).isSymbolicLink()) throw new Error('Package output must not be a symbolic link');
  if (path.dirname(output) !== path.dirname(path.resolve(root))) throw new Error('Package output escaped the project');
  fs.rmSync(output, { recursive: true, force: true });
  const mainDir = path.join(output, 'savfox');
  fs.mkdirSync(path.join(mainDir, 'bin'), { recursive: true });
  const main = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  main.version = version;
  main.files = ['bin', 'platforms.json'];
  main.optionalDependencies = Object.fromEntries(platforms.map(entry => [`@savfox/savfox-${entry.platform}`, version]));
  fs.writeFileSync(path.join(mainDir, 'package.json'), JSON.stringify(main, null, 2) + '\n');
  fs.copyFileSync(path.join(root, 'bin/savfox.js'), path.join(mainDir, 'bin/savfox.js'));
  fs.chmodSync(path.join(mainDir, 'bin/savfox.js'), 0o755);
  fs.copyFileSync(path.join(root, 'platforms.json'), path.join(mainDir, 'platforms.json'));
  for (const entry of platforms) {
    const directory = path.join(output, `savfox-${entry.platform}`);
    fs.mkdirSync(path.join(directory, 'vendor'), { recursive: true });
    fs.cpSync(path.join(dist, entry.target), path.join(directory, 'vendor', entry.target), { recursive: true });
    const binary = path.join(directory, 'vendor', entry.target, 'savfox', entry.os === 'win32' ? 'savfox.exe' : 'savfox');
    if (entry.os !== 'win32') fs.chmodSync(binary, 0o755);
    fs.writeFileSync(path.join(directory, 'package.json'), JSON.stringify({
      name: `@savfox/savfox-${entry.platform}`, version, license: main.license,
      os: [entry.os], cpu: [entry.cpu], files: ['vendor'],
      repository: main.repository, description: `Savfox CLI binary for ${entry.platform}`,
    }, null, 2) + '\n');
  }
  return output;
}

if (require.main === module) console.log(`Packages ready: ${packageNpm(process.argv[2])}`);
module.exports = { packageNpm };
