'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { RexE } = require('../src/rexe');
const { uriParts, fromUri, joinRemote, isRoot, endpointHost, portNumber } = require('../src/paths');

test('VS Code can put the Ports view in its own panel container', () => {
    const { contributes } = require('../package.json');
    const container = contributes.viewsContainers.panel.find(item =>
        contributes.views[item.id]?.some(view => view.id === 'rexeterm.ports'));
    assert.match(container?.id ?? '', /^[A-Za-z0-9_-]+$/);
});

test('virtual folder addresses preserve Windows and POSIX names and case', () => {
    for (const [file, windows, canonical] of [
        ['C:\\Users\\Demo Folder\\资料 #1.txt', true, 'C:/Users/Demo Folder/资料 #1.txt'],
        ['D:/', true, 'D:/'],
        ['\\\\server\\share\\Documents', true, '//server/share/Documents'],
        ['/home/ubuntu/资料# %?\\name', false, '/home/ubuntu/资料# %?\\name'],
        ['/C:/literal-posix-path', false, '/C:/literal-posix-path'],
    ]) {
        assert.deepEqual(fromUri(uriParts('Case-Sensitive', file, windows)), { id: 'Case-Sensitive', path: canonical, windows });
    }
    assert.equal(joinRemote('C:/', 'RexETerm', true), 'C:/RexETerm');
    assert.equal(isRoot('//server/share/', true), true);
    assert.throws(() => fromUri({ scheme:'rexeterm', authority:'r-not-hex', path:'/' }));
});

test('port inputs yield valid destinations and reject unrelated text', () => {
    assert.equal(endpointHost('::1'), '[::1]');
    assert.equal(endpointHost('localhost'), 'localhost');
    assert.equal(portNumber('0', true), 0);
    assert.equal(portNumber('65535'), 65535);
    for (const value of ['0','65536','-1','2.5','3:4']) assert.throws(() => portNumber(value));
    assert.throws(() => endpointHost('localhost:8080'));
    assert.throws(() => endpointHost('http://localhost'));
});

test('inventory handles both layout generations without returning profile secrets', async t => {
    const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'rexe-vscode-test-'));
    t.after(() => fs.rm(directory, { recursive:true, force:true }));
    const root = path.join(directory, '.rexeterm', 'config-dir');
    await fs.mkdir(path.join(root, 'executors', 'example'), { recursive:true });
    await fs.mkdir(path.join(root, 'credentials'));
    await fs.writeFile(path.join(root, 'credentials', 'example.json'), '{"password":"local-secret"}');
    await fs.writeFile(path.join(root, 'executors', 'bad.json'), 'not json');
    await fs.writeFile(path.join(root, 'executors', 'example.json'), JSON.stringify({ id:'example', name:'Legacy', root:'/' }));
    await fs.writeFile(path.join(root, 'executors', 'example', 'executor.json'), JSON.stringify({
        id:'example', name:'Current', url:'tls://host:123', root:'/workspace', connectionProfile:'../example.json', password:'local-secret',
    }));
    await fs.writeFile(path.join(root, 'executors', 'unsafe.json'), JSON.stringify({ id:'unsafe', url:'tls://user:local-secret@host:123' }));
    const rx = new RexE({ directory });
    const result = await rx.saved();
    assert.deepEqual(result.failures, ['bad.json']);
    assert.deepEqual(result.items.map(e => [e.id,e.name,e.url,e.hasProfile]), [
        ['example','Current','tls://host:123',true], ['unsafe','unsafe',undefined,false],
    ]);
    assert.deepEqual(await rx.credentials(), [{ name:'example', available:true }]);
    assert.doesNotMatch(JSON.stringify(result), /local-secret/);
});

test('directory listing follows CLI pages without using recursive glob', async () => {
    const requests = [];
    const rx = new RexE({});
    rx.rpc = async (_host, tool, params) => {
        requests.push([tool, params.offset]);
        return params.offset === 1
            ? { metadata:{ file:{ kind:'directory' } }, output:{ text:'.hidden/\nnotes.md', info:'Showing entries 1-2 of 3. Use offset=3 to continue.' } }
            : { metadata:{ file:{ kind:'directory' } }, output:{ text:'资料 folder/', info:'total 3 entries' } };
    };
    assert.deepEqual(await rx.listDirectory('example', '/srv'), [['.hidden',true],['notes.md',false],['资料 folder',true]]);
    assert.deepEqual(requests, [['read',1],['read',3]]);
});
