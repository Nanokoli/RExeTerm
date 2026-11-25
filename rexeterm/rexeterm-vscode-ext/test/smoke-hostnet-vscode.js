'use strict';

// Run with VS Code --extensionTestsPath and a .code-workspace containing the prepared RexE folders.
// REXE_MULTI_FIXTURES points to [{ id, folder, hostname, arch, windows }]; each folder
// contains same-name.txt with "HostNet fixture: <id>\n". Use disposable directories.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');
const vscode = require('vscode');

async function eventually(check, description) {
    const end = Date.now() + 25_000;
    let result;
    while (Date.now() < end) {
        try { if (await check()) return; } catch (error) { result = error; }
        await delay(300);
    }
    throw new Error(description, { cause: result });
}

exports.run = async () => {
    const { commands, Uri, window, workspace, FileType } = vscode;
    const extension = vscode.extensions.getExtension('rexeterm.rexeterm-vscode-ext');
    assert(extension, 'Install RexE Term before running this test.');
    await extension.activate();
    const { Model } = require(path.join(extension.extensionPath, 'src', 'model'));
    const { fromUri, uriParts, shLiteral } = require(path.join(extension.extensionPath, 'src', 'paths'));
    const fixtures = JSON.parse(await fs.readFile(process.env.REXE_MULTI_FIXTURES, 'utf8'));
    const destination = process.env.REXE_MULTI_REPORT;
    const storage = process.env.REXE_SMOKE_STORAGE;
    assert(storage, 'Set REXE_SMOKE_STORAGE to the test profile extension storage.');
    const model = new Model({ extensionPath: extension.extensionPath, globalStorageUri: Uri.file(storage), workspaceState: { get() {} } });
    const api = model.client();
    const report = { version: extension.packageJSON.version, simultaneousFolders: 0, simultaneousPTYs: 0, targets: [] };
    const read = async uri => Buffer.from(await workspace.fs.readFile(uri)).toString('utf8');
    const items = fixtures.map(target => {
        const root = Uri.from(uriParts(target.id, target.folder, target.windows));
        const result = { executor: target.id, system: target.windows ? 'windows' : 'linux', arch: target.arch, checks: [] };
        report.targets.push(result);
        return { ...target, root, file: Uri.joinPath(root, 'same-name.txt'), marker: Uri.joinPath(root, 'pty.txt'), result };
    });
    assert(items.length >= 2, 'Use more than one connected executor.');
    const errors = [];
    const visit = async (item, work) => {
        try { await work(item); } catch (error) {
            item.result.error = error.message;
            errors.push(new Error(item.id + ': ' + error.message, { cause: error }));
        }
    };
    const normalize = (cwd, windows) => windows ? cwd.replace(/\\/g, '/').toLowerCase() : cwd;
    try {
        assert.equal(workspace.workspaceFolders?.length, items.length);
        assert.equal(new Set(workspace.workspaceFolders.map(f => fromUri(f.uri).id)).size, items.length);
        report.simultaneousFolders = items.length;
        const state = await model.load(true);
        assert.ifError(state.error);
        for (const item of items) assert(state.online.some(row => row.id === item.id), item.id + ' is offline');

        // All roots use the same file name. Read across all roots concurrently, then use each
        // real editor to save its own marker. An executor mix-up would change another root's copy.
        await Promise.all(items.map(item => visit(item, async test => {
            const { root, file, result, id } = test;
            assert(workspace.getWorkspaceFolder(file)?.uri.toString() === root.toString(), 'VS Code did not register the correct workspace root');
            assert.equal((await workspace.fs.stat(root)).type, FileType.Directory);
            assert((await workspace.fs.readDirectory(root)).some(([name, type]) => name === 'same-name.txt' && type === FileType.File));
            assert.equal(await read(file), 'HostNet fixture: ' + id + '\n');
            result.checks.push('explorer and initial read');
        })));
        for (const test of items.filter(item => !item.result.error)) await visit(test, async item => {
            const doc = await workspace.openTextDocument(item.file);
            const editor = await window.showTextDocument(doc, { preview: false });
            assert.equal(doc.getText(), 'HostNet fixture: ' + item.id + '\n');
            assert(await editor.edit(builder => builder.insert(new vscode.Position(0, 0), 'Saved by RexE Term: ' + item.id + '\n')));
            assert(await doc.save(), 'VS Code failed to save');
            assert.equal(await read(item.file), 'Saved by RexE Term: ' + item.id + '\nHostNet fixture: ' + item.id + '\n');
            item.result.checks.push('editor save on the correct executor');

            // No executor argument: the extension must choose the host and cwd of the active editor.
            await eventually(() => window.activeTextEditor?.document.uri.toString() === item.file.toString(),
                'VS Code did not focus the requested editor');
            const terminal = await commands.executeCommand('rexeterm.newTerminal');
            assert(terminal, 'No terminal opened');
            if (terminal.name !== 'RexE: ' + model.label(item.id)) {
                terminal.dispose();
                assert.fail('Terminal opened for another executor: ' + terminal.name);
            }
            item.terminal = terminal;
            const command = item.windows
                ? "Set-Content -Encoding ascii -LiteralPath 'pty.txt' -Value ($env:COMPUTERNAME+'|"+item.id+"')"
                : "printf '%s|%s\\n' \"$(uname -n)\" " + shLiteral(item.id) + ' > pty.txt';
            item.terminal.sendText(command);
        });
        await Promise.all(items.filter(item => item.terminal).map(item => visit(item, async test => {
            const { id, result, marker, windows, folder } = test;
            await eventually(async () => (await read(marker)).trim() === test.hostname + '|' + id,
                'PTY did not write the expected host marker in its own folder');
            await eventually(async () => (await api.tasks(id)).active?.some(task =>
                typeof task.cwd === 'string' && normalize(task.cwd, windows) === normalize(folder, windows)
                && task.title === 'VS Code · ' + id), 'Active PTY was not found on the expected host and folder');
            result.checks.push('interactive PTY ran in the correct host and folder');
        })));
        report.simultaneousPTYs = items.filter(item => item.result.checks.length === 3).length;

        for (const item of items.filter(item => !item.result.error)) await visit(item, async test => {
            assert.equal(await read(test.file), 'Saved by RexE Term: ' + test.id + '\nHostNet fixture: ' + test.id + '\n');
            test.result.checks.push('final reread after all saves');
        });
        if (errors.length) throw new AggregateError(errors, errors.map(error => error.message).join('; '));
        report.passed = true;
        await commands.executeCommand('rexeterm.active.focus');
        if (process.env.REXE_MULTI_SCREEN) {
            await fs.writeFile(process.env.REXE_MULTI_SCREEN, 'ready\n');
            await eventually(() => fs.access(process.env.REXE_MULTI_SCREEN + '.done').then(() => true, () => false), 'No screenshot acknowledgement');
        }
    } catch (error) {
        report.passed = false;
        report.error = error.message;
        throw error;
    } finally {
        for (const item of items) item.terminal?.dispose();
        await fs.writeFile(destination, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
    }
};
