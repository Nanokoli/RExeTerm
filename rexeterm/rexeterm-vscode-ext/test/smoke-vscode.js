'use strict';

// Run this through VS Code --extensionTestsPath with a RexE fixture opened via --folder-uri.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');
const vscode = require('vscode');

async function eventually(check, timeout = 18_000) {
    const end = Date.now() + timeout;
    let reason;
    do {
        try {
            const value = await check();
            if (value) return value;
        } catch (error) { reason = error; }
        await delay(250);
    } while (Date.now() < end);
    throw reason || new Error('Timed out waiting for the RexE smoke test.');
}

exports.run = async () => {
    const { workspace, window, commands, FileType, Uri } = vscode;
    const installed = vscode.extensions.getExtension('rexeterm.rexeterm-vscode-ext');
    assert(installed, 'The RexETerm extension is not installed.');
    await installed.activate();
    const { RexE } = require(path.join(installed.extensionPath, 'src', 'rexe'));
    const { fromUri, shLiteral, psLiteral } = require(path.join(installed.extensionPath, 'src', 'paths'));
    const root = workspace.workspaceFolders?.[0]?.uri;
    assert.equal(root?.scheme, 'rexeterm');
    const settings = workspace.getConfiguration('rexeterm');
    const scratch = await fs.mkdtemp(path.join(os.tmpdir(), 'rexe-smoke-'));
    const api = new RexE({
        cli: settings.get('cliPath'), directory: settings.get('clientDirectory'), registry: settings.get('registryDirectory'), scratch,
    });
    const target = fromUri(root);
    const testFolder = Uri.joinPath(root, 'rexe-smoke ' + Date.now());
    const file = Uri.joinPath(testFolder, "资料 ' #%.txt"), renamed = Uri.joinPath(testFolder, 'renamed.txt');
    const marker = Uri.joinPath(testFolder, 'pty-marker.txt');
    const payload = 'RexETerm file round trip ✓\n';
    const report = { executor: target.id, system: process.platform, folder: target.path, checks: [] };
    let terminal, server, forward;
    try {
        assert(await api.serviceInfo().then(value => value.running), 'CLI service not running');
        assert.equal((await workspace.fs.stat(root)).type, FileType.Directory);
        await commands.executeCommand('rexeterm.active.focus');
        await workspace.fs.createDirectory(testFolder);
        assert((await workspace.fs.readDirectory(root)).some(([name, type]) => name === testFolder.path.split('/').at(-1) && type === FileType.Directory));
        await workspace.fs.writeFile(file, Buffer.from(payload));
        assert.equal(Buffer.from(await workspace.fs.readFile(file)).toString(), payload);
        const doc = await workspace.openTextDocument(file);
        const editor = await window.showTextDocument(doc);
        assert.equal(doc.getText(), payload);
        await editor.edit(builder => builder.insert(new vscode.Position(0, 0), 'Saved in VS Code\n'));
        assert(await doc.save());
        assert.equal(Buffer.from(await workspace.fs.readFile(file)).toString(), 'Saved in VS Code\n' + payload);
        await commands.executeCommand('workbench.action.closeActiveEditor');
        await workspace.fs.rename(file, renamed, { overwrite: false });
        await assert.rejects(() => workspace.fs.stat(file), /entrynotfound|not exist|not found/i);
        assert.equal((await workspace.fs.stat(renamed)).type, FileType.File);
        report.checks.push('native virtual Explorer, create, Unicode file, editor save, rename');

        terminal = await commands.executeCommand('rexeterm.newTerminal');
        assert(terminal, 'No RexE terminal was created');
        const markerPath = fromUri(marker).path;
        const command = target.windows
            ? 'Set-Content -LiteralPath ' + psLiteral(markerPath) + ' -Value RexE-PTY-OK -Encoding ascii'
            : 'printf %s RexE-PTY-OK > ' + shLiteral(markerPath);
        terminal.sendText(command);
        await eventually(async () => (await workspace.fs.stat(marker)).size > 0);
        assert.equal(Buffer.from(await workspace.fs.readFile(marker)).toString().trim(), 'RexE-PTY-OK');
        const active = (await api.tasks(target.id)).active || [];
        assert(active.some(task => task.cwd.replace(/\\/g, '/') === target.path && task.title.includes('VS Code')));
        report.checks.push('interactive PTY and the workspace cwd');

        if (target.id === 'local') {
            server = net.createServer(socket => socket.end('RexE forwarded TCP'));
            await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
            forward = await api.forward(target.id, 0, '127.0.0.1:' + server.address().port, 'RexE extension smoke');
            const listener = Number(forward.listen.split(':').at(-1));
            const value = await new Promise((resolve, reject) => {
                const connection = net.createConnection({ host:'127.0.0.1', port:listener });
                let output = '';
                connection.on('data', data => output += data);
                connection.once('end', () => resolve(output));
                connection.once('error', reject);
                connection.setTimeout(5_000, () => connection.destroy(new Error('Forward did not answer')));
            });
            assert.equal(value, 'RexE forwarded TCP');
            assert((await api.forwards(target.id)).some(item => item.forwardID === forward.forwardID));
            await commands.executeCommand('rexeterm.ports.focus');
            report.checks.push('loopback TCP forwarding and RexE Ports view');
        }

        const outbound = path.join(scratch, 'outbound.txt'), inbound = path.join(scratch, 'inbound.txt');
        const transferred = Uri.joinPath(testFolder, 'transfer.txt');
        await fs.writeFile(outbound, 'RexE explicit transfer\n');
        const remote = fromUri(transferred).path;
        const upload = await api.transfer(target.id, 'upload', remote, outbound, { title:'RexE test upload' });
        await api.waitTransfer(upload.id);
        const download = await api.transfer(target.id, 'download', remote, inbound, { title:'RexE test download' });
        await api.waitTransfer(download.id);
        assert.deepEqual(await fs.readFile(outbound), await fs.readFile(inbound));
        assert((await api.transfers()).some(item => item.id === download.id && item.status === 'done'));
        report.checks.push('explicit two way file transfer and task list');

        await workspace.fs.delete(testFolder, { recursive: true, useTrash: false });
        await assert.rejects(() => workspace.fs.stat(testFolder), /entrynotfound|not exist|not found/i);
        report.checks.push('recursive removal of temporary fixture');
        report.passed = true;
        console.log('REXE_SMOKE_PASSED', JSON.stringify(report));
    } catch (error) {
        report.error = error.stack;
        console.error('REXE_SMOKE_FAILED', JSON.stringify(report));
        throw error;
    } finally {
        if (forward) await api.api('forwards.stop', { host:target.id, id:forward.forwardID }).catch(() => {});
        terminal?.dispose();
        server?.close();
        await workspace.fs.delete(testFolder, { recursive: true, useTrash: false }).then(() => {}, () => {});
        await fs.rm(scratch, { recursive:true, force:true });
        if (process.env.REXE_SMOKE_REPORT) await fs.writeFile(process.env.REXE_SMOKE_REPORT, JSON.stringify(report, null, 2));
    }
};
