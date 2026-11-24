'use strict';

const vscode = require('vscode');
const path = require('node:path');
const { RexE } = require('./rexe');
const { fromUri, remotePath } = require('./paths');

class Model {
    constructor(ctx) {
        this.ctx = ctx;
        this.selectedId = ctx.workspaceState.get('activeExecutor');
        this.cached = undefined;
    }

    client() {
        const settings = vscode.workspace.getConfiguration('rexeterm');
        const storage = this.ctx.globalStorageUri.fsPath;
        const directory = settings.inspect('clientDirectory')?.globalValue?.trim() || path.join(storage, 'client');
        const local = settings.inspect('localExecutor')?.globalValue === true;
        if (!path.isAbsolute(directory)) throw new Error('RexE Client Directory must be an absolute local path.');
        const key = [directory, local].join('\0');
        if (key !== this.key) {
            this.key = key;
            this.rexe = RexE.forExtension(this.ctx.extensionPath, storage, directory, local);
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
        const [saved, creds, live] = await Promise.allSettled([api.saved(), api.credentials(), api.online()]);
        const state = {
            saved: saved.value?.items ?? [], invalid: saved.value?.failures ?? [],
            credentials: creds.value ?? [],
            online: live.value?.executors ?? [], defaultId: live.value?.defaultId,
            error: live.reason?.message || saved.reason?.message || creds.reason?.message,
        };
        const existing = id => state.online.some(item => item.id === id);
        this.selectedId = [this.selectedId, this.workspaceId, state.defaultId, state.online[0]?.id].find(id => id && existing(id));
        return state;
    }

    async load(fresh = false) {
        if (!fresh && this.cached) return this.cached;
        if (this.loading) return this.loading;
        this.loading = this.snapshot()
            .catch(error => ({ saved: [], invalid: [], credentials: [], online: [], error: error.message }))
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
