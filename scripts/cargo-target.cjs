const { spawnSync } = require('node:child_process');
const path = require('node:path');

function cargoTargetDirectory(root, env = process.env) {
  const result = spawnSync('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1'], {
    cwd: root, env, encoding: 'utf8', windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || 'Cargo metadata failed');
  const target = JSON.parse(result.stdout).target_directory;
  if (typeof target !== 'string' || !path.isAbsolute(target)) throw new Error('Cargo returned no absolute target directory');
  return path.resolve(target);
}

module.exports = { cargoTargetDirectory };
