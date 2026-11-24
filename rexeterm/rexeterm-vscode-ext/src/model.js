'use strict';

const vscode = require('vscode');
const os = require('node:os');
const path = require('node:path');
const fs = require('node:fs');
const { createHash } = require('node:crypto');
const { RexE } = require('./rexe');
const { fromUri, remotePath } = require('./paths');

function managedRegistry(directory) {
    const conventional = path.join(directory, '.rexeterm', 'runtime');
    if (process.platform === 'win32' || Buffer.byteLength(conventional) <= 84) return conventional;
    const uid = process.getuid();
    const base = '/tmp/rexe-vsc-' + uid;
    fs.mkdirSync(base, { recursive: true, mode: 0o700 });
    const info = fs.lstatSync(base);
    if (!info.isDirectory() || info.uid !== uid || (info.mode & 0o077) !== 0) {
        throw new Error('RexE needs a private registry directory. Set an absolute path in RexE › Registry Directory.');
    }
    return path.join(base, createHash('sha256').update(directory).digest('hex').slice(0, 16));
}

class Model {
    constructor(ctx) {
        this.ctx = ctx;
        this.selectedId = ctx.workspaceState.get('activeExecutor');
        this.cached = undefined;
    }

    client() {
        const settings = vscode.workspace.getConfiguration('rexeterm');
        const configured = key => settings.inspect(key)?.globalValue?.trim();
        const directory = configured('clientDirectory') || path.join(this.ctx.globalStorageUri.fsPath, 'client');
        const cli = configured('cliPath') || (process.platform === 'win32' ? 'rexeterm-cli.exe' : 'rexeterm-cli');
        const registry = configured('registryDirectory') || managedRegistry(directory);
        if (!path.isAbsolute(directory) || !path.isAbsolute(registry)) throw new Error('RexE directories must be absolute local paths.');
        if (!path.isAbsolute(cli) && /[\\/]/.test(cli)) throw new Error('Use an absolute path or command name for the RexETerm CLI.');
        const key = [directory, cli, registry].join('\0');
        if (key !== this.key) {
            this.key = key;
            this.rexe = new RexE({ cli, directory, registry, scratch: path.join(this.ctx.globalStorageUri.fsPath, 'scratch') });
        }
        return this.rexe;
    }

    workspaceFolder() {
        const editorUri = vscode.window.activeTextEditor?.document.uri;
        if (editorUri?.scheme === 'rexeterm') {
            let match = vscode.workspace.getWorkspaceFolder(editorUri);
            const drive = editorUri.query === 'win' && /^\/([a-z]):\//i.exec(editorUri.path)?.[1];
            if (!match && drive) {
                // VS Code can lower-case the drive in an opened document but keep the
                // original case on the workspace root. The virtual provider is case sensitive.
                const alternate = drive === drive.toLowerCase() ? drive.toUpperCase() : drive.toLowerCase();
                match = vscode.workspace.getWorkspaceFolder(editorUri.with({ path: '/' + alternate + editorUri.path.slice(2) }));
            }
            return match?.uri;
        }
        return vscode.workspace.workspaceFolders?.find(folder => folder.uri.scheme === 'rexeterm')?.uri;
    }

    get workspaceId() {
        const folder = this.workspaceFolder();
        if (!folder) return undefined;
        try { return fromUri(folder).id; } catch { return undefined; }
    }

    async select(id) {
        this.selectedId = id;
        await this.ctx.workspaceState.update('activeExecutor', id);
        this.onChange?.();
    }

    label(id) { return this.cached?.saved.find(item => item.id === id)?.name || id; }

    root(executor) {
        const windows = executor.system === 'windows';
        const saved = this.cached?.saved.find(item => item.id === executor.id);
        try { return remotePath(saved?.root || executor.root || (windows ? 'C:/' : '/'), windows); }
        catch { return windows ? 'C:/' : '/'; }
    }

    async snapshot() {
        const api = this.client();
        const [saved, creds, info] = await Promise.allSettled([api.saved(), api.credentials(), api.serviceInfo()]);
        const state = {
            saved: saved.value?.items ?? [], invalid: saved.value?.failures ?? [],
            credentials: creds.value ?? [], running: Boolean(info.value?.running), online: [], defaultId: undefined,
            error: info.reason?.message || saved.reason?.message || creds.reason?.message,
        };
        if (state.running) {
            try {
                const live = await api.online();
                state.online = live.executors;
                state.defaultId = live.defaultId;
            } catch (error) { state.error = error.message; }
        }
        const existing = id => state.online.some(item => item.id === id);
        this.selectedId = [this.selectedId, this.workspaceId, state.defaultId, state.online[0]?.id].find(id => id && existing(id));
        return state;
    }

    async load(fresh = false) {
        if (!fresh && this.cached) return this.cached;
        if (this.loading) return this.loading;
        this.loading = this.snapshot()
            .catch(error => ({ saved: [], invalid: [], credentials: [], online: [], running: false, error: error.message }))
            .then(result => { this.cached = result; return result; })
            .finally(() => { this.loading = undefined; });
        return this.loading;
    }

    async refresh() {
        if (this.refreshing) return this.refreshing;
        this.refreshing = this.load(true).then(() => this.onChange?.()).finally(() => { this.refreshing = undefined; });
        return this.refreshing;
    }
}

module.exports = { Model };
