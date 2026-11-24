'use strict';

const { execFile } = require('node:child_process');
const fs = require('node:fs/promises');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');
const { psLiteral, shLiteral, isRoot } = require('./paths');

const rows = value => Array.isArray(value) ? value : [];
const string = value => typeof value === 'string' ? value : undefined;
const publicUrl = value => string(value) && !/[@?#\r\n]/.test(value) ? value : undefined;
const editorFileLimit = 256 * 1024 * 1024;

function checkEditorFileSize(size) {
    if (size > editorFileLimit) throw new Error('RexE editor files must be 256 MiB or smaller.');
}

class Queue {
    constructor(max) { this.max = max; this.running = 0; this.pending = []; }
    async run(action) {
        if (this.running < this.max) this.running++;
        else await new Promise(resolve => this.pending.push(resolve));
        try { return await action(); }
        finally {
            const next = this.pending.shift();
            if (next) next();
            else this.running--;
        }
    }
}

class RexE {
    static forExtension(extension, storage, directory, local) {
        const binary = 'rexeterm-vscode-backend' + (process.platform === 'win32' ? '.exe' : '');
        return new RexE({
            backend: path.join(extension, 'bin', binary),
            directory: directory || path.join(storage, 'client'),
            registry: path.join(storage, 'runtime'),
            scratch: path.join(storage, 'scratch'),
            local,
        });
    }

    constructor({ backend, directory, registry, scratch, local }) {
        this.backend = backend;
        this.directory = directory;
        this.registry = registry;
        this.scratch = scratch;
        this.local = local;
        this.queue = new Queue(8);
        this.ready = undefined;
    }

    async workspace() {
        this.ready ??= Promise.all([
            fs.mkdir(this.directory, { recursive: true, mode: 0o700 }),
            fs.mkdir(this.scratch, { recursive: true, mode: 0o700 }),
        ]).catch(error => { this.ready = undefined; throw error; });
        await this.ready;
    }

    async api(operation, args = {}, timeout = 15_000) {
        await this.workspace();
        const argv = ['--registry-dir', this.registry, ...(this.local ? ['--local'] : [])];
        return this.queue.run(() => new Promise((resolve, reject) => {
            const proc = execFile(this.backend, argv, {
                cwd: this.directory, windowsHide: true, timeout, maxBuffer: 8 * 1024 * 1024,
            }, (error, stdout, stderr) => {
                if (error?.code === 'ENOENT' || error?.code === 'EACCES') {
                    return reject(new Error('RexE backend could not start. Reinstall the RexE Term VSIX for this platform.'));
                }
                if (error?.killed) return reject(new Error('RexE did not answer before the timeout.'));
                if (error) return reject(new Error(stderr.trim().slice(0, 1000) || 'RexE backend failed (' + error.code + ').'));
                try { resolve(JSON.parse(stdout)); }
                catch { reject(new Error('RexE backend returned an invalid response.')); }
            });
            // Exit status and stderr report failures even if the child closes its pipe immediately.
            proc.stdin?.on('error', () => {});
            proc.stdin?.end(JSON.stringify({ operation, args }));
        }));
    }

    async temporary(action) {
        await this.workspace();
        const dir = await fs.mkdtemp(path.join(this.scratch, 'rx-'));
        try { return await action(dir); }
        finally { await fs.rm(dir, { recursive: true, force: true, maxRetries: 2 }).catch(() => {}); }
    }

    rpc(id, tool, params = {}, directory, terminal, timeout) {
        return this.api('rpc', { executor: id, tool, params, directory, terminal }, timeout);
    }

    async online() {
        const value = (await this.api('list_executor', {}, 25_000)).metadata ?? {};
        return { executors: rows(value.executors), defaultId: string(value.default) };
    }

    async saved() {
        const dir = path.join(this.directory, '.rexeterm', 'config-dir', 'executors');
        const entries = await this.entries(dir);
        const failures = [];
        const candidates = await Promise.all(entries.map(async entry => {
            const filename = entry.isDirectory()
                ? path.join(dir, entry.name, 'executor.json')
                : (entry.name.endsWith('.json') ? path.join(dir, entry.name) : undefined);
            if (!filename) return undefined;
            try {
                const data = JSON.parse(await fs.readFile(filename, 'utf8'));
                if (!string(data.id)) throw new Error('Missing executor ID');
                return { id: data.id, name: string(data.name) || data.id, root: string(data.root), url: publicUrl(data.url),
                    hasProfile: typeof data.connectionProfile === 'string', preferred: entry.isDirectory() };
            } catch (error) {
                if (error.code !== 'ENOENT') failures.push(entry.name);
                return undefined;
            }
        }));
        const result = new Map();
        for (const entry of candidates.filter(Boolean)) {
            if (entry.preferred || !result.has(entry.id)) result.set(entry.id, entry);
        }
        return { items: [...result.values()].sort((a, b) => a.name.localeCompare(b.name)), failures };
    }

    async credentials() {
        const dir = path.join(this.directory, '.rexeterm', 'config-dir', 'credentials');
        const entries = await this.entries(dir);
        return Promise.all(entries.filter(entry => entry.name.endsWith('.json')).sort((a, b) => a.name.localeCompare(b.name)).map(async entry => ({
            name: entry.name.slice(0, -5),
            available: await fs.stat(path.join(dir, entry.name)).then(stat => stat.isFile(), () => false),
        })));
    }

    async entries(dir) {
        try { return await fs.readdir(dir, { withFileTypes: true }); }
        catch (error) { if (error.code === 'ENOENT') return []; throw error; }
    }

    connect(id, credential) {
        return this.api('executor.connect', { id, credential }, 65_000);
    }

    stat(host, file) {
        return this.rpc(host, 'stat', { filePath: file }).then(result => {
            const data = result.metadata?.file;
            if (!data?.kind) throw new Error('The executor did not return file metadata.');
            return data;
        });
    }

    async listDirectory(host, file) {
        const result = [];
        for (let offset = 1; ; ) {
            const { metadata, output } = await this.rpc(host, 'read', { filePath: file, offset, limit: 512 });
            if (metadata?.file?.kind !== 'directory') throw new Error('This RexE path is not a directory.');
            for (const line of (output?.text ?? '').split('\n')) {
                if (!line) continue;
                if (line.endsWith('/')) result.push([line.slice(0, -1), true]);
                else result.push([line, false]);
            }
            const next = Number(output?.info?.match(/Use offset=(\d+) to continue\./)?.[1]);
            if (!Number.isSafeInteger(next) || next <= offset) return result;
            offset = next;
        }
    }

    async transfer(host, mode, file, localPath, options = {}) {
        return this.api('transfers.local', { host, options: { mode, path: file, localPath, ...options } });
    }

    async waitTransfer(id, timeout = 110_000) {
        const end = Date.now() + timeout;
        do {
            const result = await this.api('transfers.show', { id });
            if (result.status === 'done') return result;
            if (['error', 'cancelled'].includes(result.status)) {
                throw new Error(result.error || result.message || 'RexE file transfer ' + result.status + '.');
            }
            await delay(160);
        } while (Date.now() < end);
        await this.api('transfers.cancel', { id }).catch(() => {});
        throw new Error('RexE file transfer timed out.');
    }

    // Editor reads and writes wait for the receipt on the same RPC. They do not depend
    // on progress history, which can already have been pruned when many files are open.
    editorTransfer(host, mode, file, localPath, overwrite = false) {
        return this.rpc(host, 'file_transfer', {
            mode, targetPath: file, localPath, overwrite, title: mode === 'upload' ? 'VS Code: Save file' : 'VS Code: Read file',
        }, undefined, undefined, 110_000);
    }

    readFile(host, file) {
        return this.temporary(async dir => {
            checkEditorFileSize((await this.stat(host, file)).size);
            const local = path.join(dir, 'content');
            await this.editorTransfer(host, 'download', file, local);
            checkEditorFileSize((await fs.stat(local)).size);
            return fs.readFile(local);
        });
    }

    writeFile(host, file, data, overwrite) {
        checkEditorFileSize(data.byteLength);
        return this.temporary(async dir => {
            const local = path.join(dir, 'content');
            await fs.writeFile(local, data, { mode: 0o600 });
            return this.editorTransfer(host, 'upload', file, local, overwrite);
        });
    }

    async fileOperation(host, windows, operation, file, options = {}) {
        if (isRoot(file, windows) || (options.destination && isRoot(options.destination, windows))) {
            throw new Error('A filesystem root cannot be changed.');
        }
        let command;
        if (windows) {
            const p = psLiteral(file.replace(/\//g, '\\'));
            if (operation === 'mkdir') {
                command = '[void][System.IO.Directory]::CreateDirectory(' + p + ')';
            } else if (operation === 'delete') {
                const remove = options.recursive
                    ? "if ($i.PSIsContainer -and -not ($i.Attributes -band [IO.FileAttributes]::ReparsePoint)) { Remove-Item -LiteralPath $p -Recurse -Force } elseif ($i.PSIsContainer) { [IO.Directory]::Delete($p) } else { Remove-Item -LiteralPath $p -Force }"
                    : "if ($i.PSIsContainer) { [IO.Directory]::Delete($p) } else { Remove-Item -LiteralPath $p -Force }";
                command = '$p = ' + p + '; $i = Get-Item -Force -LiteralPath $p; ' + remove;
            } else if (operation === 'rename') {
                const q = psLiteral(options.destination.replace(/\//g, '\\'));
                const existing = options.overwrite
                    ? 'if ($i.PSIsContainer -or [IO.Directory]::Exists($q)) { throw "Cannot overwrite a directory" }; [IO.File]::Replace($p,$q,$null)'
                    : 'throw "The destination already exists"';
                command = '$p = ' + p + '; $q = ' + q + '; $i = Get-Item -Force -LiteralPath $p; '
                    + 'if ($p -ceq $q) { return }; '
                    + 'if ([string]::Equals($p,$q,[StringComparison]::OrdinalIgnoreCase)) { '
                    + 'Rename-Item -LiteralPath $p -NewName ([IO.Path]::GetFileName($q)); return }; '
                    + 'if (Test-Path -LiteralPath $q) { ' + existing + ' } '
                    + 'elseif ($i.PSIsContainer) { [IO.Directory]::Move($p,$q) } else { [IO.File]::Move($p,$q) }';
            } else throw new Error('Unsupported RexE file operation.');
            command = '$ErrorActionPreference = "Stop"; ' + command;
        } else {
            const p = shLiteral(file);
            if (operation === 'mkdir') command = 'mkdir -- ' + p;
            else if (operation === 'delete') {
                command = options.recursive ? 'rm -r -- ' + p
                    : 'if [ -d ' + p + ' ] && [ ! -L ' + p + ' ]; then rmdir -- ' + p + '; else rm -- ' + p + '; fi';
            } else if (operation === 'rename') {
                const q = shLiteral(options.destination);
                const check = options.overwrite
                    ? 'if [ -d ' + q + ' ] || [ -d ' + p + ' ]; then printf "%s\\n" "Cannot overwrite a directory" >&2; exit 1; fi; '
                    : 'printf "%s\\n" "The destination already exists" >&2; exit 1; ';
                command = 'if [ -e ' + q + ' ] || [ -L ' + q + ' ]; then ' + check + 'fi; mv -T -- ' + p + ' ' + q;
            } else throw new Error('Unsupported RexE file operation.');
        }
        let reply = await this.rpc(host, 'exbash', {
            mode: 'shell', io: 'stdio', title: 'VS Code: ' + operation, command, read_timeout: 1000, timeout: 10000,
        });
        const id = reply.metadata?.asyncID;
        if (!id) throw new Error('The executor did not start the file operation.');
        const end = Date.now() + 12_000;
        while (reply.metadata?.state !== 'stopped' && Date.now() < end) {
            reply = await this.api('tasks.read', { host, id, wait_ms: 400, from_start: true });
            if (reply.metadata?.state !== 'stopped') await delay(140);
        }
        if (reply.metadata?.state !== 'stopped' || reply.metadata?.exitCode !== 0) {
            throw new Error((reply.output?.stderr || reply.output?.text || 'RexE could not ' + operation + ' this path.').trim().slice(0, 600));
        }
    }

    tasks(host) { return this.api('tasks.list', { host }); }
    forwards(host) { return this.api('forwards.list', host ? { host } : {}); }
    transfers() { return this.api('transfers.list'); }
    forward(host, listen, target, title) {
        return this.api('forwards.start', {
            host, options: { listen: '127.0.0.1:' + listen, target: { kind: 'tcp', address: target }, title },
        });
    }
}

module.exports = { RexE, rows, string, publicUrl };
