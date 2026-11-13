import test from 'node:test';
import assert from 'node:assert/strict';
import { managementCount, renderManagementView } from './management-view.js';
const owner = {username:'alice', clientName:'App', clientId:'client-1', sessionId:'session-1'};
const snapshot = {executorId:'server',observedAtMs:1,clients:[
  {owner,trafficComplete:true,activeLinks:3,traffic:{receiveBytesPerSecond:12500000,sendBytesPerSecond:2500000}},
  {owner:{...owner,username:'bob',clientName:'Other'},trafficComplete:false},
],programs:[{programId:'task-1',title:'<img src=x>',description:'" onmouseover="evil',command:['<script>bad()</script>'],createdBy:owner,controllerSession:owner}],channels:[
  {kind:'pty',channelId:'pty-id',target:'task-1',owner},
  {kind:'file',channelId:'file-id',target:'file-target',owner,metadata:{title:'<b>file</b>',description:'file description'}},
  {kind:'forward',channelId:'forward-id',target:'forward-target',owner,metadata:{title:'forward'}},
]};
const render = (page='clients', result={status:'connected',snapshot}) => renderManagementView({hosts:[{id:'h',name:'Host'}],host:'h',page,result});
test('groups clients by RE user and uses aggregate decimal MB/s',()=>{
  const html=render();
  assert.equal((html.match(/class="management-user"/g)||[]).length,2);
  assert.match(html,/alice/);assert.match(html,/bob/);assert.match(html,/12\.50 \/ 2\.50/);
  assert.match(html,/统计不完整/);assert.match(html,/3 条链路/);
});
test('five pages count and filter channel kinds and reuse task labels',()=>{
  assert.equal(managementCount(snapshot,'clients'),2);assert.equal(managementCount(snapshot,'programs'),1);
  for(const kind of ['pty','file','forward']){
    assert.equal(managementCount(snapshot,kind),1);
    const html=render(kind);assert.ok(html.includes(`${kind}-id`));
    for(const other of ['pty','file','forward'].filter(v=>v!==kind))assert.ok(!html.includes(`${other}-id`));
  }
  assert.match(render('pty'),/&lt;img src=x&gt;/);
  assert.match(render('programs'),/会话 session-1/);
  assert.match(render('programs'),/客户端 client-1/);
});
test('escapes all remote labels, descriptions and commands',()=>{
  for(const page of ['programs','pty','file']){
    const html=render(page);assert.ok(!html.includes('<img'));assert.ok(!html.includes('<script>'));assert.ok(!html.includes('<b>file'));
  }
  assert.match(render('programs'),/&lt;script&gt;bad\(\)&lt;\/script&gt;/);
  assert.match(render('pty'),/&quot; onmouseover=&quot;evil/);
});
test('failed permissions never render an old snapshot',()=>{
  for(const status of ['denied','unavailable','invalid_configuration']){
    const html=render('programs',{status,snapshot,message:'<script>error</script>'});
    assert.ok(!html.includes('task-1'));assert.ok(!html.includes('management-table'));
    assert.match(html,/&lt;script&gt;error&lt;\/script&gt;/);
  }
  assert.equal(managementCount(null,'clients'),'—');
});
