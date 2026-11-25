'use strict';

// VS Code --extensionTestsPath: use a private test profile with prepared RexE configs.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const vscode = require('vscode');

exports.run = async () => {
    const read = file => fs.readFile(file, 'utf8').then(JSON.parse);
    const scenarios = await read(process.env.REXE_CONFIG_SMOKE_TARGETS);
    const storage = process.env.REXE_SMOKE_STORAGE;
    assert(storage, 'Set the extension storage path for the test profile');
    const extension = vscode.extensions.getExtension('rexeterm.rexeterm-vscode-ext');
    await extension.activate();
    const { Model } = require(path.join(extension.extensionPath, 'src', 'model'));
    const { uriParts } = require(path.join(extension.extensionPath, 'src', 'paths'));
    const model = new Model({ extensionPath: extension.extensionPath, globalStorageUri: vscode.Uri.file(storage), workspaceState: { get() {} } });
    let state = await model.load(true);
    assert.ifError(state.error);
    await vscode.commands.executeCommand('rexeterm.configs.focus');
    const result = [];
    for (const expected of scenarios) {
        const config = state.configs.find(item => item.id === expected.id);
        assert.deepEqual(config?.route, expected.route);
        const executor = await vscode.commands.executeCommand('rexeterm.connect', { args: { config } });
        assert.equal(executor?.id, expected.id);
        state = await model.load(true);
        assert.ifError(state.error);
        assert.equal(model.root(executor), expected.startPath);
        const uri = vscode.Uri.from(uriParts(executor.id, expected.startPath, executor.system === 'windows'));
        const stat = await vscode.workspace.fs.stat(uri);
        const entries = await vscode.workspace.fs.readDirectory(uri);
        assert.equal(stat.type, vscode.FileType.Directory);
        assert(entries.length > 0);
        result.push({ ...expected, entries: entries.length });
    }
    assert.deepEqual(state.online.map(executor => executor.id).sort(), scenarios.map(item => item.id).sort());
    await fs.writeFile(process.env.REXE_CONFIG_SMOKE_REPORT, JSON.stringify({
        version: extension.packageJSON.version, verified:true, targets:result,
    }, null, 2) + '\n');
};
