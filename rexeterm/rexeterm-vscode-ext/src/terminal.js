'use strict';

const vscode = require('vscode');
const { StringDecoder } = require('node:string_decoder');
const { setTimeout: wait } = require('node:timers/promises');

function size(dimensions) {
    return {
        cols: Math.min(300, Math.max(1, dimensions?.columns || 80)),
        rows: Math.min(120, Math.max(1, dimensions?.rows || 24)),
    };
}

function* chunks(value) {
    let result = '', count = 0;
    for (const char of value) {
        const bytes = Buffer.byteLength(char);
        if (count + bytes > 4096) {
            yield result;
            result = '';
            count = 0;
        }
        result += char;
        count += bytes;
    }
    if (result) yield result;
}

class RexETerminal {
    constructor(client, executor, cwd, attached, onChange, onDispose) {
        this.client = client;
        this.executor = executor;
        this.cwd = cwd;
        this.attached = Boolean(attached);
        this.id = attached;
        this.onChange = onChange;
        this.onDispose = onDispose;
        this.write = new vscode.EventEmitter();
        this.exit = new vscode.EventEmitter();
        this.onDidWrite = this.write.event;
        this.onDidClose = this.exit.event;
        this.decoder = new StringDecoder('utf8');
        this.offset = 0;
        this.input = '';
    }

    open(dimensions) {
        if (this.start) return;
        this.start = this.begin(dimensions);
        this.scheduleInput();
        this.start.then(() => this.pump()).catch(error => this.fail(error));
    }

    async begin(dimensions) {
        if (!this.id) {
            const win = this.executor.system === 'windows';
            const command = win ? 'powershell.exe -NoLogo -NoProfile -NoExit' : 'exec "${SHELL:-/bin/sh}" -i';
            const params = {
                mode: 'shell', io: 'pty', command, title: 'VS Code · ' + this.executor.id,
                read_timeout: 200,
            };
            const start = params => this.client.rpc(this.executor.id, 'exbash', params, this.cwd, { raw: true }, 22_000);
            const result = await start({ ...params, ...size(dimensions) }).catch(error => {
                // Older rexed builds reject these fields before allocating a task.
                if (!/unknown field [\x60"'](?:cols|rows)[\x60"']/i.test(error.message)) throw error;
                return start(params);
            });
            this.id = result.metadata?.asyncID;
            if (!this.id) throw new Error('The executor did not allocate a PTY.');
            this.show(result);
            this.onChange();
        }
    }

    show(result) {
        const metadata = result.metadata ?? {};
        if (!this.closed) {
            if (metadata.from > this.offset) this.write.fire('\r\n[RexE: earlier output is no longer available]\r\n');
            const encoded = result.output?.rawBase64;
            if (encoded) {
                const value = this.decoder.write(Buffer.from(encoded, 'base64'));
                if (value) this.write.fire(value);
            }
        }
        if (Number.isSafeInteger(metadata.next_offset)) this.offset = metadata.next_offset;
        return metadata;
    }

    async pump() {
        while (!this.closed) {
            const result = await this.client.api('tasks.read', {
                host: this.executor.id, id: this.id, since: this.offset, wait_ms: 550, terminal: { raw: true },
            });
            if (this.closed) break;
            const metadata = this.show(result);
            if (metadata.terminalOutput?.error && metadata.state !== 'stopped') {
                throw new Error(metadata.terminalOutput.error);
            }
            if (metadata.state === 'stopped' && !metadata.detached) {
                this.stopped = true;
                const tail = this.decoder.end();
                if (tail) this.write.fire(tail);
                const code = Number(metadata.exitCode);
                this.exit.fire(Number.isInteger(code) && code >= 0 ? code : 1);
                this.onChange();
                return;
            }
            if (!result.output?.rawBase64) await wait(50);
        }
    }

    handleInput(value) {
        if (this.closed || this.stopped) return;
        if (this.input.length + value.length > 1_048_576) {
            this.write.fire('\r\n[RexE: terminal input queue is full]\r\n');
            return;
        }
        this.input += value;
        this.scheduleInput();
    }

    scheduleInput() {
        if (!this.start || !this.input || this.sending) return;
        this.sending = setTimeout(() => this.flush().catch(error => this.fail(error)), 16);
    }

    async flush() {
        try {
            await this.start;
            while (this.input && !this.closed && !this.stopped && this.id) {
                const batch = this.input;
                this.input = '';
                for (const text of chunks(batch)) {
                    if (this.closed || this.stopped) break;
                    await this.client.rpc(this.executor.id, 'exbash', {
                        mode: 'attach', asyncID: this.id, text, read_timeout: 0,
                    });
                }
            }
        } finally { this.sending = undefined; }
    }

    fail(error) {
        if (this.closed || this.failed) return;
        this.failed = true;
        this.write.fire('\r\nRexE: ' + error.message.replace(/[\x00-\x1f]/g, ' ') + '\r\n');
        this.exit.fire(1);
        this.onChange();
    }

    close() {
        if (this.closed) return;
        this.closed = true;
        clearTimeout(this.sending);
        this.input = '';
        if (!this.attached && !this.stopped) {
            this.start?.then(() => this.id && this.client.api('tasks.stop', { host: this.executor.id, id: this.id }))
                .catch(() => {}).finally(this.onChange);
        }
        this.onDispose(this);
        this.write.dispose();
        this.exit.dispose();
    }
}

module.exports = { RexETerminal, chunks };
