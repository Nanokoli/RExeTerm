'use strict';

const vscode = require('vscode');
const path = require('node:path');
const fs = require('node:fs/promises');
const { Model } = require('./model');
const { RexEViews, forwardLabel } = require('./view');
const { RexEFileSystem } = require('./fs');
const { RexETerminal } = require('./terminal');
const { namePattern, remotePath, joinRemote, pathName, isRoot, uriParts, fromUri, endpointHost, portNumber } = require('./paths');

const { window, workspace, commands, Uri, FileType } = vscode;
const settings = 'rexeterm';
const { randomUUID } = require('node:crypto');

function validate(input, check) {
    try { check(input); return undefined; } catch (error) { return error.message; }
}

function niceId(value) {
    if (!namePattern.test(value)) throw new Error('Use up to 100 letters, digits, dots, underscores, or hyphens.');
}

function toUri(executor, file) { return Uri.from(uriParts(executor.id, file, executor.system === 'windows')); }

function remoteParent(file, win) {
    const next = file.slice(0, file.lastIndexOf('/')) || '/';
    return remotePath(win && /^[a-z]:$/i.test(next) ? next + '/' : next, win);
}

async function activate(ctx) {
    const model = new Model(ctx);
    const activeView = new RexEViews(ctx, model);
    const terminals = new Map();
    const infoDocuments = new Map();
    const status = window.createStatusBarItem(vscode.StatusBarAlignment.Left, 35);
    status.command = 'rexeterm.chooseExecutorAction';

    model.onChange = () => {
        activeView.refresh();
        const id = model.selectedId;
        if (!id && !model.workspaceId) { status.hide(); return; }
        status.text = '$(remote) RexE: ' + model.label(id || model.workspaceId);
        status.tooltip = id ? 'RexE executor actions' : 'Select a RexE executor';
        status.show();
    };
    const fileSystem = new RexEFileSystem(() => model.client());
    ctx.subscriptions.push(status, fileSystem, workspace.registerFileSystemProvider('rexeterm', fileSystem, { isCaseSensitive: true }),
        workspace.registerTextDocumentContentProvider('rexeterm-info', {
            provideTextDocumentContent(uri) { return infoDocuments.get(uri.toString()) ?? ''; },
        }));

    const scopes = () => commands.executeCommand('setContext', 'rexeterm.workspace', Boolean(model.workspaceId));
    scopes();
    ctx.subscriptions.push(workspace.onDidChangeWorkspaceFolders(scopes), workspace.onDidChangeConfiguration(event => {
        if (event.affectsConfiguration(settings)) model.refresh();
    }));
    const refreshTimer = setInterval(() => { if (activeView.isVisible && window.state.focused) model.refresh(); }, 5000);
    refreshTimer.unref?.();
    ctx.subscriptions.push({ dispose: () => clearInterval(refreshTimer) });

    function guarded(id, action) {
        ctx.subscriptions.push(commands.registerCommand('rexeterm.' + id, async (...args) => {
            try { return await action(...args); }
            catch (error) { window.showErrorMessage('RexE: ' + error.message); return undefined; }
        }));
    }

    async function onlineExecutor(argument, { chooseExecutor = false, workspaceId = model.workspaceId } = {}) {
        const raw = argument?.args ?? {};
        const fromGivenUri = argument?.scheme === 'rexeterm' ? fromUri(argument).id : undefined;
        const specified = raw.executor?.id || raw.saved?.id || raw.row?.executor || fromGivenUri;
        const state = await model.load(true);
        if (state.error) throw new Error(state.error);
        if (!state.running) {
            if (!(await promptStart())) return undefined;
            if (!model.cached?.running) throw new Error('RexETerm did not start.');
            return onlineExecutor(argument, { chooseExecutor, workspaceId });
        }
        if (specified) {
            const executor = state.online.find(item => item.id === specified);
            if (executor) return executor;
            const entry = state.saved.find(item => item.id === specified);
            if (!entry) throw new Error(specified + ' is disconnected.');
            if ((await window.showInformationMessage(specified + ' is disconnected.', 'Connect')) !== 'Connect') return undefined;
            return connect(entry);
        }
        const current = state.online.find(item => item.id === (workspaceId || model.selectedId));
        if (current && workspaceId && (!chooseExecutor || state.online.length === 1)) return current;
        if (!state.online.length) {
            if (state.saved.length && (await window.showInformationMessage('No active RexE executors.', 'Connect an Executor')) === 'Connect an Executor') return connect();
            if (!state.saved.length) window.showInformationMessage('Add an executor in the RexE sidebar to open a remote folder.');
            return undefined;
        }
        return (await window.showQuickPick(state.online.map(executor => ({
            executor, label: (executor === current ? '$(debug-connected) ' : '$(remote) ') + model.label(executor.id),
            description: [executor.id, executor.system].join(' · '),
            detail: executor.device && executor.device !== executor.id ? executor.device : undefined,
        })), { title: 'Select a RexE Executor' }))?.executor;
    }

    async function promptStart() {
        if ((await window.showInformationMessage('Start the RexETerm service for this VS Code profile?', 'Start Service')) !== 'Start Service') return false;
        await start();
        return true;
    }

    async function start() {
        await window.withProgress({ location: vscode.ProgressLocation.Notification, title: 'Starting RexETerm' }, () => model.client().ensureService());
        await model.refresh();
    }

    async function connect(saved, preferredCredential) {
        const state = await model.load(true);
        if (state.error) throw new Error(state.error);
        if (!saved) {
            saved = (await window.showQuickPick(state.saved.map(item => ({ label: item.name, description: item.id, detail: item.url, item })),
                { title: 'Connect to an Executor' }))?.item;
            if (!saved) return undefined;
        }
        const existing = state.online.find(item => item.id === saved.id);
        if (existing) { await model.select(existing.id); return existing; }
        let credential = preferredCredential;
        const credentials = state.credentials.filter(value => value.available);
        if (!credential && credentials.length) {
            const answer = await window.showQuickPick([
                { label: 'Use the executor’s saved settings', value: '' },
                ...credentials.map(item => ({ label: '$(key) ' + item.name, value: item.name })),
            ], { title: 'Credential for ' + saved.name });
            if (!answer) return undefined;
            credential = answer.value;
        }
        const api = model.client();
        await window.withProgress({ location: vscode.ProgressLocation.Notification, title: 'Connecting to ' + saved.name }, async () => {
            await api.ensureService();
            await api.connect(saved.id, credential);
        });
        await model.select(saved.id);
        const fresh = await model.load(true);
        model.onChange();
        return fresh.online.find(item => item.id === saved.id);
    }

    async function browseFolder(executor, start) {
        const win = executor.system === 'windows';
        let folder = start;
        while (folder) {
            const result = await window.withProgress({ location: vscode.ProgressLocation.Window, title: 'Reading RexE folders' },
                () => model.client().listDirectory(executor.id, folder)).then(entries => ({ entries }), error => ({ error }));
            const content = [
                ...(!result.error ? [{ label: '$(folder-opened) Open This Folder', description: folder, type: 'open' }] : []),
                { label: '$(edit) Enter Absolute Path…', description: result.error?.message, type: 'path' },
                ...(!isRoot(folder, win) ? [{ label: '$(arrow-up) Parent Folder', type: 'back' }] : []),
                ...(result.entries ?? []).filter(([, dir]) => dir).map(([name]) => ({ label: '$(folder) ' + name, name, type: 'child' })),
            ];
            const pick = await window.showQuickPick(content, { title: model.label(executor.id) + ' — ' + folder, placeHolder: 'Choose a folder' });
            if (!pick) return undefined;
            if (pick.type === 'open') return folder;
            if (pick.type === 'child') { folder = joinRemote(folder, pick.name, win); continue; }
            if (pick.type === 'back') { folder = remoteParent(folder, win); continue; }
            const value = await window.showInputBox({ title: 'RexE Folder Path', value: folder, validateInput: input => validate(input, x => remotePath(x, win)) });
            if (value === undefined) continue;
            folder = remotePath(value, win);
        }
        return undefined;
    }

    async function openFolder(argument) {
        const executor = await onlineExecutor(argument, { chooseExecutor: true });
        if (!executor) return;
        const args = argument?.args ?? {};
        const location = argument?.scheme === 'rexeterm' ? fromUri(argument).path : undefined;
        const file = args.path || location || await browseFolder(executor, args.start || model.root(executor));
        if (!file) return;
        const uri = toUri(executor, file);
        if ((await workspace.fs.stat(uri)).type !== FileType.Directory) throw new Error(file + ' is not a folder.');
        const workspaceId = model.workspaceId;
        const forceNewWindow = Boolean(workspace.workspaceFile) || workspaceId !== executor.id;
        if (!workspaceId || !forceNewWindow) await model.select(executor.id);
        return commands.executeCommand('vscode.openFolder', uri, { forceNewWindow });
    }

    async function details(data, name = 'RexE Details') {
        const uri = Uri.from({ scheme: 'rexeterm-info', path: '/' + name + '.json', query: randomUUID() });
        infoDocuments.set(uri.toString(), JSON.stringify(data, null, 2));
        try {
            const document = await workspace.openTextDocument(uri);
            return await window.showTextDocument(document, { preview: true });
        } finally { infoDocuments.delete(uri.toString()); }
    }

    async function executorInfo(argument) {
        const exec = await onlineExecutor(argument);
        if (!exec) return;
        const meta = (await model.client().rpc(exec.id, '_executor_info')).metadata;
        return details({
            executor: exec.id, name: model.label(exec.id), device: meta?.device ?? exec.device, system: meta?.system ?? exec.system,
            root: model.root(exec), version: meta?.version ?? exec.version, capabilities: meta?.capabilities,
            channels: exec.channels, client: exec.clientSession?.username,
        }, exec.id + ' Executor Info');
    }

    async function selectAction(argument) {
        const executor = await onlineExecutor(argument);
        if (!executor) return;
        await model.select(executor.id);
        const items = [
            ['$(folder-opened) Open RexE Folder…', 'openFolder'],
            ['$(info) Executor Info', 'executorInfo'],
            ['$(terminal) New RexE Terminal', 'newTerminal'],
            ['$(plug) Forward a Port…', 'forwardPort'],
            ['$(debug-disconnect) Disconnect', 'disconnect'],
        ];
        const answer = await window.showQuickPick(items.map(([label, command]) => ({ label, command })), { title: model.label(executor.id) });
        if (answer) return commands.executeCommand('rexeterm.' + answer.command, { args: { executor } });
    }

    function terminalOptions(executor, cwd, task) {
        const id = task?.asyncID;
        const pty = new RexETerminal(model.client(), executor, cwd, id, () => model.refresh(), () => {
            if (id) terminals.delete(executor.id + ':' + id);
        });
        return { name: id ? (task.title && task.title !== '未命名' ? task.title : id) : 'RexE: ' + model.label(executor.id), pty, iconPath: new vscode.ThemeIcon('terminal') };
    }

    async function terminalTarget(argument) {
        const folder = argument?.scheme === 'rexeterm' ? argument : model.workspaceFolder();
        const cwd = folder ? fromUri(folder) : undefined;
        const executor = await onlineExecutor(argument, { workspaceId: cwd?.id });
        if (!executor) return undefined;
        return { executor, path: cwd?.id === executor.id ? cwd.path : (argument?.args?.path || model.root(executor)) };
    }

    async function newTerminal(argument) {
        const target = await terminalTarget(argument);
        if (!target) return;
        await model.select(target.executor.id);
        const terminal = window.createTerminal(terminalOptions(target.executor, target.path));
        terminal.show();
        return terminal;
    }

    if (window.registerTerminalProfileProvider) {
        ctx.subscriptions.push(window.registerTerminalProfileProvider('rexeterm.terminalProfile', {
            async provideTerminalProfile() {
                try {
                    const target = await terminalTarget();
                    if (target) return new vscode.TerminalProfile(terminalOptions(target.executor, target.path));
                } catch (error) { window.showErrorMessage('RexE: ' + error.message); }
                return undefined;
            },
        }));
    }

    async function transfer(mode, argument) {
        const executor = await onlineExecutor(argument);
        if (!executor) return;
        const windows = executor.system === 'windows';
        const folder = model.workspaceFolder();
        const scope = folder ? fromUri(folder) : undefined;
        const base = scope?.id === executor.id ? scope.path : model.root(executor);
        let value, local;
        if (mode === 'upload') {
            [local] = await window.showOpenDialog({ canSelectFiles: true, canSelectFolders: false, openLabel: 'Upload File', availableFileSystems: ['file'] }) ?? [];
            if (local?.scheme !== 'file') return;
            const entry = argument?.scheme === 'rexeterm' ? fromUri(argument).path : base;
            value = joinRemote(entry, path.basename(local.fsPath), windows);
        } else {
            value = argument?.scheme === 'rexeterm' ? fromUri(argument).path : base;
        }
        const prompted = mode === 'upload' || argument?.scheme !== 'rexeterm';
        const input = prompted ? await window.showInputBox({
            title: mode === 'upload' ? 'Remote Destination' : 'Remote File to Download', value,
            validateInput: value => validate(value, input => remotePath(input, windows)),
        }) : value;
        if (input === undefined) return;
        const target = remotePath(input, windows);
        if (mode === 'download') {
            local = await window.showSaveDialog({ defaultUri: Uri.file(path.join(require('node:os').homedir(), pathName(target))),
                saveLabel: 'Download File', availableFileSystems: ['file'] });
            if (local?.scheme !== 'file') return;
        }
        const api = model.client();
        const exists = mode === 'upload'
            ? (await api.stat(executor.id, target)).kind !== 'missing'
            : await fs.stat(local.fsPath).then(() => true, error => { if (error.code === 'ENOENT') return false; throw error; });
        if (exists && await window.showWarningMessage('Replace the existing file?', { modal: true, detail: mode === 'upload' ? target : local.fsPath }, 'Replace') !== 'Replace') return;
        await window.withProgress({ title: mode === 'upload' ? 'Uploading to RexE' : 'Downloading from RexE',
            location: vscode.ProgressLocation.Notification, cancellable: true }, async (_, token) => {
            const { id } = await api.transfer(executor.id, mode, target, local.fsPath, {
                overwrite: exists, title: (mode === 'upload' ? 'Upload ' : 'Download ') + pathName(target),
            });
            const cancel = () => api.api('transfers.cancel', { id }).catch(() => {});
            const listener = token.onCancellationRequested(cancel);
            if (token.isCancellationRequested) await cancel();
            model.refresh();
            try { await api.waitTransfer(id); }
            catch (error) { if (!token.isCancellationRequested) throw error; }
            finally { listener.dispose(); model.refresh(); }
            if (!token.isCancellationRequested && mode === 'upload') await commands.executeCommand('workbench.files.action.refreshFilesExplorer');
        });
    }

    async function forwardPort(argument) {
        const executor = await onlineExecutor(argument);
        if (!executor) return;
        const remote = await window.showInputBox({ title: 'Remote TCP Port', prompt: 'Port on ' + model.label(executor.id),
            validateInput: x => validate(x, portNumber) });
        if (remote === undefined) return;
        const host = await window.showInputBox({ title: 'Remote Destination Host', value: '127.0.0.1',
            validateInput: x => validate(x, endpointHost) });
        if (host === undefined) return;
        const local = await window.showInputBox({ title: 'Local TCP Port', value: remote, prompt: 'Enter 0 to choose an available port automatically',
            validateInput: x => validate(x, x => portNumber(x, true)) });
        if (local === undefined) return;
        const forward = await model.client().forward(executor.id, portNumber(local, true), endpointHost(host) + ':' + portNumber(remote), 'VS Code RexE');
        await model.refresh();
        const pick = await window.showInformationMessage('RexE forwarded ' + forwardLabel(forward), 'Show Ports', 'Copy Address');
        if (pick === 'Show Ports') return commands.executeCommand('rexeterm.ports.focus');
        if (pick === 'Copy Address') await vscode.env.clipboard.writeText(forwardLabel(forward));
    }

    function localAddress(row) {
        const address = forwardLabel(row);
        if (!/^(?:127\.0\.0\.1|\[::1\]|localhost):[0-9]+$/.test(address)) {
            throw new Error('This forward does not have a loopback address.');
        }
        return address;
    }

    guarded('refresh', () => model.refresh());
    guarded('startService', start);
    guarded('settings', () => commands.executeCommand('workbench.action.openSettings', '@ext:rexeterm.rexeterm-vscode-ext'));
    guarded('connect', item => connect(item?.args?.saved, item?.args?.credential?.name));
    guarded('chooseExecutorAction', selectAction);
    guarded('disconnect', async item => {
        const executor = await onlineExecutor(item);
        if (!executor || await window.showWarningMessage('Disconnect ' + executor.id + '?', { modal: true }, 'Disconnect') !== 'Disconnect') return;
        await model.client().api('hosts.disconnect', { host: executor.id });
        await model.refresh();
    });
    guarded('openFolder', openFolder);
    guarded('executorInfo', executorInfo);
    guarded('newTerminal', newTerminal);
    guarded('attachTerminal', item => {
        const { executor, task } = item.args;
        const key = executor.id + ':' + task.asyncID;
        let terminal = terminals.get(key);
        if (!terminal) {
            terminal = window.createTerminal(terminalOptions(executor, task.cwd, task));
            terminals.set(key, terminal);
        }
        terminal.show();
    });
    guarded('stopTerminal', async item => {
        const { executor, task } = item.args;
        await model.client().api('tasks.stop', { host: executor.id, id: task.asyncID });
        await model.refresh();
    });
    guarded('upload', item => transfer('upload', item));
    guarded('download', item => transfer('download', item));
    guarded('transferInfo', item => details(item.args.row));
    guarded('cancelTransfer', async item => {
        await model.client().api('transfers.cancel', { id: item.args.row.id });
        await model.refresh();
    });
    guarded('forwardPort', forwardPort);
    guarded('showPorts', () => commands.executeCommand('rexeterm.ports.focus'));
    guarded('forwardInfo', async item => {
        const { row } = item.args;
        const picked = await window.showQuickPick(['Copy Local Address', 'Open in Browser', 'Forward Details'], { title: forwardLabel(row) });
        if (picked === 'Copy Local Address') return vscode.env.clipboard.writeText(forwardLabel(row));
        if (picked === 'Open in Browser') return vscode.env.openExternal(Uri.parse('http://' + localAddress(row)));
        if (picked === 'Forward Details') return details(row);
    });
    guarded('stopForward', async item => {
        const { row } = item.args;
        if (await window.showWarningMessage('Stop forwarding ' + forwardLabel(row) + '?', { modal: true }, 'Stop Forward') !== 'Stop Forward') return;
        await model.client().api('forwards.stop', { id: row.forwardID, host: row.executor });
        await model.refresh();
    });
    guarded('addExecutor', async () => {
        const id = await window.showInputBox({ title: 'New RexE Executor ID', validateInput: x => validate(x, niceId) });
        if (!id) return;
        const address = await window.showInputBox({ title: 'Executor Endpoint', placeHolder: 'tls://host:45957',
            validateInput: x => /^(tls|tcp|wss?|unix):\/\/[^\s]+$/.test(x) && !/[@?#]/.test(x) ? undefined : 'Enter a RexE endpoint, such as tls://host:45957.' });
        if (!address) return;
        const root = await window.showInputBox({ title: 'Executor Start Folder', value: '/', validateInput: x => validate(x, remotePath) });
        if (!root) return;
        const api = model.client();
        await api.run(['-e', id, 'executor', 'host', 'add', address, '--root', remotePath(root)]);
        await model.refresh();
    });
    guarded('linkCredential', async () => {
        const [file] = await window.showOpenDialog({ filters: { 'RexETerm profile': ['json'] },
            openLabel: 'Link Credential Profile', availableFileSystems: ['file'] }) ?? [];
        if (file?.scheme !== 'file') return;
        const name = await window.showInputBox({ title: 'Credential Name', validateInput: x => validate(x, niceId) });
        if (!name) return;
        await model.client().run(['credential', 'link', name, file.fsPath]);
        await model.refresh();
    });

    if (model.workspaceId) model.refresh();
}

module.exports = { activate };
