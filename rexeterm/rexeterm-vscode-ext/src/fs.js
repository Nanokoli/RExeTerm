'use strict';

const vscode = require('vscode');
const { fromUri, isRoot } = require('./paths');

const { FileType, FileChangeType: Change, FileSystemError: FSError } = vscode;

function asError(error, uri) {
    if (error instanceof FSError) return error;
    const message = error.message || String(error);
    if (/access is denied|permission denied|not authorized/i.test(message)) return FSError.NoPermissions(message);
    if (/not found|not exist|cannot find|no such file/i.test(message) && !/executor|cli|transfer/i.test(message.split('\n')[0])) {
        return FSError.FileNotFound(uri);
    }
    return FSError.Unavailable(message);
}

function typeOf(kind) {
    if (kind === 'directory') return FileType.Directory;
    if (kind === 'file') return FileType.File;
    return FileType.Unknown;
}

function parent(uri) {
    let path = uri.path.slice(0, uri.path.replace(/\/$/, '').lastIndexOf('/')) || '/';
    if (uri.query === 'win' && /^(?:\/[a-z]:|\/~unc\/[^/]+\/[^/]+)$/i.test(path)) path += '/';
    return uri.with({ path });
}

class RexEFileSystem {
    constructor(client, changed) {
        this.client = client;
        this.changed = changed;
        this.emitter = new vscode.EventEmitter();
        this.onDidChangeFile = this.emitter.event;
        this.watches = new Map();
        this.stamps = new Map();
        this.pollCursor = 0;
        this.poller = setInterval(() => this.poll().catch(() => {}), 5000);
        this.poller.unref?.();
    }

    dispose() {
        clearInterval(this.poller);
        this.emitter.dispose();
        this.watches.clear();
        this.stamps.clear();
    }

    watch(uri) {
        const key = uri.toString();
        const record = this.watches.get(key) ?? { uri, count: 0 };
        record.count++;
        this.watches.set(key, record);
        return { dispose: () => {
            if (--record.count === 0) {
                this.watches.delete(key);
                this.stamps.delete(key);
            }
        } };
    }

    async poll() {
        if (this.polling || !vscode.window.state.focused) return;
        const candidates = new Map([...this.watches].map(([key, record]) => [key, record.uri]));
        for (const doc of vscode.workspace.textDocuments) {
            if (doc.uri.scheme === 'rexeterm') candidates.set(doc.uri.toString(), doc.uri);
        }
        for (const key of this.stamps.keys()) if (!candidates.has(key)) this.stamps.delete(key);
        const all = [...candidates.values()];
        if (!all.length) return;
        const count = Math.min(all.length, 8);
        const sample = Array.from({ length: count }, (_, i) => all[(this.pollCursor + i) % all.length]);
        this.pollCursor = (this.pollCursor + count) % all.length;
        this.polling = true;
        try {
            const events = [];
            await Promise.all(sample.map(async uri => {
                const { id, path } = fromUri(uri);
                const key = uri.toString();
                const data = await this.client().stat(id, path).catch(() => undefined);
                if (!data) return;
                const stamp = data.kind === 'missing' ? null : [data.fileKey, data.mtimeMs, data.size].join(':');
                const last = this.stamps.get(key);
                this.stamps.set(key, stamp);
                if (last === undefined || last === stamp) return;
                events.push({ uri, type: stamp === null ? Change.Deleted : last === null ? Change.Created : Change.Changed });
            }));
            if (events.length) this.emit(events);
        } finally { this.polling = false; }
    }

    emit(events) {
        this.emitter.fire(events);
        this.changed?.();
    }

    async at(uri, fn) {
        try { return await fn(fromUri(uri), this.client()); }
        catch (error) { throw asError(error, uri); }
    }

    stat(uri) {
        return this.at(uri, async ({ id, path }, api) => {
            const data = await api.stat(id, path);
            if (data.kind === 'missing') throw FSError.FileNotFound(uri);
            return { type: typeOf(data.kind), size: data.size ?? 0, ctime: 0, mtime: data.mtimeMs ?? 0 };
        });
    }

    readDirectory(uri) {
        return this.at(uri, async ({ id, path }, api) => {
            const entries = await api.listDirectory(id, path);
            return entries.map(([name, dir]) => [name, dir ? FileType.Directory : FileType.File]);
        });
    }

    readFile(uri) {
        return this.at(uri, ({ id, path }, api) => api.readFile(id, path));
    }

    writeFile(uri, contents, options) {
        return this.at(uri, async ({ id, path }, api) => {
            const data = await api.stat(id, path);
            const present = data.kind !== 'missing';
            if (data.kind === 'directory') throw FSError.FileIsADirectory(uri);
            if (present && !options.overwrite) throw FSError.FileExists(uri);
            if (!present && !options.create) throw FSError.FileNotFound(uri);
            await api.writeFile(id, path, contents, present);
            this.emit([{ uri, type: present ? Change.Changed : Change.Created }, { uri: parent(uri), type: Change.Changed }]);
        });
    }

    createDirectory(uri) {
        return this.at(uri, async ({ id, path, windows }, api) => {
            const data = await api.stat(id, path);
            if (data.kind === 'directory') return;
            if (data.kind !== 'missing') throw FSError.FileExists(uri);
            await api.fileOperation(id, windows, 'mkdir', path);
            this.emit([{ uri, type: Change.Created }, { uri: parent(uri), type: Change.Changed }]);
        });
    }

    delete(uri, options) {
        return this.at(uri, async ({ id, path, windows }, api) => {
            if (isRoot(path, windows)) throw FSError.NoPermissions('A filesystem root cannot be deleted.');
            const data = await api.stat(id, path);
            if (data.kind === 'missing') throw FSError.FileNotFound(uri);
            await api.fileOperation(id, windows, 'delete', path, options);
            this.emit([{ uri, type: Change.Deleted }, { uri: parent(uri), type: Change.Changed }]);
        });
    }

    rename(oldUri, newUri, options) {
        return this.at(oldUri, async ({ id, path, windows }, api) => {
            const to = fromUri(newUri);
            if (id !== to.id || windows !== to.windows) throw FSError.NoPermissions('Move files within the same RexE executor.');
            if (path === to.path) return;
            const [data, destination] = await Promise.all([api.stat(id, path), api.stat(id, to.path)]);
            if (data.kind === 'missing') throw FSError.FileNotFound(oldUri);
            const caseChange = windows && path.toLowerCase() === to.path.toLowerCase();
            if (!caseChange && destination.kind !== 'missing' && !options.overwrite) throw FSError.FileExists(newUri);
            await api.fileOperation(id, windows, 'rename', path, { destination: to.path, overwrite: options.overwrite });
            this.emit([{ uri: oldUri, type: Change.Deleted }, { uri: newUri, type: Change.Created },
                { uri: parent(oldUri), type: Change.Changed }, { uri: parent(newUri), type: Change.Changed }]);
        });
    }

    async copy(from, to, options) {
        // Keep remote file copies within the same executor and use the supported file channels.
        const fromPath = fromUri(from), toPath = fromUri(to);
        if (fromPath.id !== toPath.id) throw FSError.NoPermissions('Use file transfer to copy between executors.');
        if ((await this.stat(from)).type !== FileType.File) throw FSError.NoPermissions('Copy currently supports individual files.');
        await this.writeFile(to, await this.readFile(from), { create: true, overwrite: options.overwrite });
    }
}

module.exports = { RexEFileSystem };
