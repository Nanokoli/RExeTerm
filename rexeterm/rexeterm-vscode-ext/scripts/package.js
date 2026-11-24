'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const [target, source, ...extra] = process.argv.slice(2);
const supported = ['linux-arm64', 'linux-x64', 'win32-x64'];
if (!supported.includes(target) || !source || extra.length) {
    console.error('Usage: npm run package -- <' + supported.join('|') + '> <backend-binary>');
    process.exit(2);
}
const project = path.resolve(__dirname, '..');
const { name, version } = require('../package.json');
const bin = path.join(project, 'bin');
const executable = path.join(bin, 'rexeterm-vscode-backend' + (target.startsWith('win32-') ? '.exe' : ''));
const output = path.join(project, 'dist', name + '-' + version + '-' + target + '.vsix');
fs.mkdirSync(bin, { recursive: true });
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.copyFileSync(path.resolve(source), executable, fs.constants.COPYFILE_EXCL);
try {
    fs.chmodSync(executable, 0o755);
    const vsce = path.join(project, 'node_modules', '@vscode', 'vsce', 'vsce');
    execFileSync(process.execPath, [vsce, 'package', '--no-dependencies', '--skip-license', '--target', target, '--out', output],
        { cwd: project, stdio: 'inherit' });
} finally {
    fs.unlinkSync(executable);
}
