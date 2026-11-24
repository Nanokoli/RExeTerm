'use strict';

const windowsPath = /^(?:[a-z]:[\\/]|\\\\|\/\/[^/]+\/[^/]+)/i;
const drivePath = /^[a-z]:\//i;
const namePattern = /^[a-z\d][a-z\d._-]{0,99}$/i;

function remotePath(value, windows = windowsPath.test(value)) {
    if (typeof value !== 'string' || value.includes('\0')) throw new Error('Enter an absolute RexE path.');
    let source = value;
    if (windows) {
        source = source.replace(/^\\\\\?\\UNC\\/i, '\\\\').replace(/^\\\\\?\\/, '').replace(/\\/g, '/');
    }
    const prefix = windows
        ? (drivePath.test(source) ? source.slice(0, 3) : source.match(/^\/\/[^/]+\/[^/]+(?:\/|$)/)?.[0])
        : (source.startsWith('/') ? '/' : undefined);
    if (!prefix) throw new Error(windows ? 'Enter a drive or UNC path such as C:/Users.' : 'Enter an absolute path beginning with /.');
    const parts = [];
    for (const part of source.slice(prefix.length).split('/')) {
        if (part === '' || part === '.') continue;
        if (part === '..') parts.pop();
        else parts.push(part);
    }
    return prefix.replace(/\/$/, '') + '/' + parts.join('/');
}

function joinRemote(parent, name, windows) {
    if (!name || name === '.' || name === '..' || name.includes('/') || (windows && name.includes('\\'))) {
        throw new Error('The executor returned an unsupported file name.');
    }
    return remotePath(parent.replace(/\/$/, '') + '/' + name, windows);
}

function pathName(value) {
    return value.replace(/\/$/, '').split('/').at(-1) || value;
}

function isRoot(value, windows) {
    return windows ? /^(?:[a-z]:|\/\/[^/]+\/[^/]+)\/$/i.test(value) : value === '/';
}

function uriParts(id, value, windows = windowsPath.test(value)) {
    if (!id || id.includes('\0')) throw new Error('Invalid RexE executor ID.');
    const file = remotePath(value, windows);
    return {
        scheme: 'rexeterm',
        authority: 'r-' + Buffer.from(id, 'utf8').toString('hex'),
        path: windows ? (file.startsWith('//') ? '/~unc' + file.slice(1) : '/' + file) : file,
        query: windows ? 'win' : '',
    };
}

function fromUri(uri) {
    if (uri.scheme !== 'rexeterm' || !/^r-(?:[\da-f]{2})+$/.test(uri.authority)) {
        throw new Error('Invalid RexE folder address.');
    }
    const id = Buffer.from(uri.authority.slice(2), 'hex').toString('utf8');
    const windows = uri.query === 'win';
    const path = windows
        ? (uri.path.startsWith('/~unc/') ? '/' + uri.path.slice(5) : uri.path.slice(1))
        : uri.path;
    return { id, path: remotePath(path, windows), windows };
}

function endpointHost(value) {
    const { isIP } = require('node:net');
    const host = value.trim();
    if (isIP(host) === 6) return '[' + host + ']';
    if (host.startsWith('[') && host.endsWith(']') && isIP(host.slice(1, -1)) === 6) return host;
    if (/^(?:[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?)$/.test(host)) return host;
    throw new Error('Enter a host name or IP address (without a port).');
}

function portNumber(value, allowZero = false) {
    const port = /^\d{1,5}$/.test(value) ? Number(value) : -1;
    if (port < (allowZero ? 0 : 1) || port > 65535) throw new Error('Enter a valid TCP port.');
    return port;
}

function psLiteral(value) { return "'" + value.replace(/'/g, "''") + "'"; }
function shLiteral(value) { return "'" + value.replace(/'/g, "'\"'\"'") + "'"; }

module.exports = { remotePath, joinRemote, pathName, isRoot, uriParts, fromUri, endpointHost, portNumber, psLiteral, shLiteral, namePattern };
