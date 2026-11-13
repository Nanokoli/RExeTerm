import test from 'node:test';
import assert from 'node:assert/strict';
import { escapeHTML, parentPath, joinPath, breadcrumbs, formatBytes } from './utils.js';

test('remote names are rendered as literal text', () => {
  assert.equal(escapeHTML('<img src=x onerror="alert(1)">&\''), '&lt;img src=x onerror=&quot;alert(1)&quot;&gt;&amp;&#39;');
  assert.equal(escapeHTML('中文目录'), '中文目录');
});
test('path navigation preserves drive roots and POSIX roots', () => {
  assert.equal(parentPath('E:\\项目\\src'), 'E:\\项目');
  assert.equal(parentPath('E:\\项目'), 'E:\\');
  assert.equal(parentPath('E:\\'), 'E:\\');
  assert.equal(parentPath('/home'), '/');
  assert.equal(parentPath('/'), '/');
  assert.equal(joinPath('E:\\', '中文.txt'), 'E:\\中文.txt');
  assert.equal(joinPath('/', 'tmp'), '/tmp');
});
test('breadcrumbs retain valid remote paths', () => {
  assert.deepEqual(breadcrumbs('E:\\Projects\\RexETerm'), [
    {name:'E:',path:'E:\\'}, {name:'Projects',path:'E:\\Projects'},
    {name:'RexETerm',path:'E:\\Projects\\RexETerm'},
  ]);
  assert.deepEqual(breadcrumbs('/home/user'), [{name:'/home',path:'/home'}, {name:'user',path:'/home/user'}]);
});
test('missing file sizes remain unknown', () => {
  assert.equal(formatBytes(undefined), '—');
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(1024), '1 KB');
});
