'use strict';

const vscode = require('vscode');
const { rows } = require('./rexe');
const { pathName, joinRemote } = require('./paths');

const Fold = vscode.TreeItemCollapsibleState;
const icon = id => new vscode.ThemeIcon(id);

class Node extends vscode.TreeItem {
    constructor(label, type, { id, description, tooltip, glyph, fold = Fold.None, args, command } = {}) {
        super(label, fold);
        this.contextValue = type;
        this.id = id;
        this.description = description;
        this.tooltip = tooltip;
        this.iconPath = glyph && icon(glyph);
        this.args = args;
        if (command) this.command = { command: 'rexeterm.' + command, title: label, arguments: [this] };
    }
}

const empty = (label, glyph = 'info') => new Node(label, 'message', { glyph });
const group = (kind, label, glyph, executor) => new Node(label, kind, {
    id: kind + ':' + executor.id, glyph, fold: Fold.Collapsed, args: { executor },
});
const action = (label, glyph, command, args) => new Node(label, 'action', { glyph, command, args });
const forwardLabel = row => String(row.listen || '').replace(/^tcp:\/\//, '');
const activeTransfer = row => ['running', 'pending', 'waiting_upload'].includes(row.status);
const fileTransfer = row => !['VS Code: Read file', 'VS Code: Save file'].includes(row.title);

function byteLabel(value) {
    if (!Number.isFinite(value) || value < 1) return '0 B';
    const i = Math.min(3, Math.floor(Math.log(value) / Math.log(1024)));
    return (value / 1024 ** i).toLocaleString(undefined, { maximumFractionDigits: i ? 1 : 0 }) + [' B', ' KiB', ' MiB', ' GiB'][i];
}

function cleanLabel(value) { return typeof value === 'string' ? value.replace(/[\x00-\x1f]/g, ' ') : ''; }

class RexEViews {
    constructor(ctx, model) {
        const active = new vscode.EventEmitter();
        const ports = new vscode.EventEmitter();
        const saved = new vscode.EventEmitter();
        const credentials = new vscode.EventEmitter();
        this.model = model;
        this.events = { active, ports, saved, credentials };
        this.views = {};
        for (const [name, emitter] of Object.entries(this.events)) {
            const tree = vscode.window.createTreeView('rexeterm.' + name, {
                treeDataProvider: {
                    onDidChangeTreeData: emitter.event,
                    getTreeItem: item => item,
                    getChildren: item => this.children(name, item).catch(error => [empty(error.message, 'warning')]),
                },
                showCollapseAll: name === 'active',
            });
            this.views[name] = tree;
            ctx.subscriptions.push(tree, emitter, tree.onDidChangeVisibility(event => {
                if (event.visible) this.model.refresh();
            }));
        }
    }

    refresh() { for (const e of Object.values(this.events)) e.fire(undefined); }

    get isVisible() { return Object.values(this.views).some(view => view.visible); }

    async children(view, item) {
        const m = await this.model.load();
        if (!item) {
            if (view === 'credentials') {
                return m.credentials.length ? m.credentials.map(credential => new Node(credential.name, 'credential', {
                    id: 'credential:' + credential.name, glyph: credential.available ? 'key' : 'warning',
                    description: credential.available ? undefined : 'Missing profile', args: { credential },
                })) : [empty('No credential links')];
            }
            if (view === 'saved') {
                const nodes = m.saved.map(saved => {
                    const online = m.online.some(item => item.id === saved.id);
                    return new Node(saved.name, online ? 'savedOnline' : 'savedOffline', {
                        id: 'saved:' + saved.id, glyph: 'server', description: online ? 'Online' : saved.id === saved.name ? undefined : saved.id,
                        tooltip: [saved.root, saved.url].filter(Boolean).join('\n'), args: { saved }, command: online ? 'chooseExecutorAction' : 'connect',
                    });
                });
                for (const bad of m.invalid) nodes.push(empty('Cannot read ' + bad, 'warning'));
                return nodes.length ? nodes : [empty('No saved executors')];
            }
            if (view === 'ports') {
                if (m.error) return [empty(m.error, 'warning')];
                const forwards = await this.model.client().forwards();
                return forwards.length ? this.showForwards(forwards) : [empty('No forwarded ports', 'plug')];
            }
            if (m.error) return [empty(m.error, 'warning')];
            if (!m.online.length) return [empty('No active executors')];
            return m.online.map(executor => {
                const selected = executor.id === this.model.selectedId;
                return new Node(this.model.label(executor.id), 'online', {
                    id: 'online:' + executor.id, description: [selected ? 'Selected' : undefined, executor.system].filter(Boolean).join(' · '),
                    glyph: selected ? 'debug-connected' : 'remote', fold: Fold.Collapsed, command: 'chooseExecutorAction',
                    args: { executor }, tooltip: [executor.id, executor.device, executor.url].filter(Boolean).join('\n'),
                });
            });
        }

        const { executor } = item.args;
        const api = this.model.client();
        if (item.contextValue === 'online') return [
            action('Open RexE Folder…', 'folder-opened', 'openFolder', { executor }),
            action('Executor Info', 'info', 'executorInfo', { executor }),
            group('folders', 'Folders', 'folder', executor),
            group('pty', 'PTY', 'terminal', executor),
            group('transfers', 'File Transfers', 'arrow-swap', executor),
            group('forwards', 'Port Forwards', 'plug', executor),
        ];
        if (item.contextValue === 'folders' || item.contextValue === 'folder') {
            const windows = executor.system === 'windows';
            const base = item.args.path ?? this.model.root(executor);
            const list = await api.listDirectory(executor.id, base);
            const folders = list.filter(([, directory]) => directory).map(([name]) => {
                const dir = joinRemote(base, name, windows);
                return new Node(name, 'folder', {
                    id: 'folder:' + executor.id + ':' + dir, glyph: 'folder', fold: Fold.Collapsed,
                    tooltip: dir, args: { executor, path: dir }, command: 'openFolder',
                });
            });
            return [action('Browse ' + base + '…', 'folder-opened', 'openFolder', { executor, start: base }), ...folders];
        }
        if (item.contextValue === 'pty') {
            const { active, completed } = await api.tasks(executor.id);
            const tasks = [...rows(active), ...rows(completed).slice().reverse()].filter(task =>
                typeof task.asyncID === 'string' && !task.asyncID.startsWith('stdio-')).slice(0, 30);
            return [action('New RexE Terminal', 'add', 'newTerminal', { executor }), ...tasks.map(task => {
                const running = task.state === 'running';
                const label = cleanLabel(task.title && task.title !== '未命名' ? task.title : task.asyncID);
                return new Node(label, running ? 'ptyRunning' : 'ptyStopped', {
                    id: 'pty:' + executor.id + ':' + task.asyncID, description: running ? 'Running' : 'Exited',
                    glyph: 'terminal', command: 'attachTerminal', args: { executor, task },
                    tooltip: [task.asyncID, task.cwd, task.state].filter(Boolean).join('\n'),
                });
            })];
        }
        if (item.contextValue === 'transfers') {
            const list = (await api.transfers()).filter(row => row.host === executor.id && fileTransfer(row)).reverse().slice(0, 35);
            return [
                action('Upload File…', 'cloud-upload', 'upload', { executor }),
                action('Download File…', 'cloud-download', 'download', { executor }),
                ...list.map(row => new Node(pathName(row.remotePath || row.id), activeTransfer(row) ? 'transferRunning' : 'transferDone', {
                    id: 'transfer:' + row.id, glyph: row.mode === 'upload' ? 'cloud-upload' : 'cloud-download',
                    description: row.status + ' · ' + byteLabel(row.bytes) + (row.total > row.bytes ? ' / ' + byteLabel(row.total) : ''),
                    tooltip: [row.title, row.remotePath, row.error].filter(Boolean).join('\n'), args: { executor, row }, command: 'transferInfo',
                })),
            ];
        }
        if (item.contextValue === 'forwards') {
            const list = await api.forwards(executor.id);
            return [action('Forward a Port…', 'add', 'forwardPort', { executor }), ...this.showForwards(list)];
        }
        return [];
    }

    showForwards(list) {
        return list.map(row => new Node(forwardLabel(row) + ' → ' + (row.target?.address || '?'), 'forward', {
            id: 'forward:' + row.forwardID, glyph: row.running === false || row.lastError ? 'warning' : 'plug',
            description: row.executor + (row.activeConnections > 0 ? ' · ' + row.activeConnections + ' active' : ''),
            command: 'forwardInfo', args: { row },
            tooltip: [row.title, row.forwardID, row.lastError].filter(Boolean).join('\n'),
        }));
    }
}

module.exports = { RexEViews, forwardLabel, activeTransfer };
