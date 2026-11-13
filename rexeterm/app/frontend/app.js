import { icon } from './icons.js';
import { renderManagementView, managementPages } from './management-view.js';
import { decodeTerminalBatch } from './terminal-batch.js';
import { escapeHTML as esc, basename, parentPath, joinPath, formatBytes, fileType, breadcrumbs } from './utils.js';

const $ = selector => document.querySelector(selector);
const state = {
  saved:{hosts:[],lastHost:null},runtime:{executors:[]},host:null,path:'',entries:[],nextOffset:null,
  side:'hosts',panel:'tasks',selected:null,search:null,history:[],tree:new Map(),expanded:new Set(),
  epoch:0,previewEpoch:0,panelEpoch:0,panelBusy:false,mcp:null,listen:'127.0.0.1:8080',
  managementHost:null,managementPage:'clients',managementResult:null,managementBusy:false,managementEpoch:0,
  tasks:[],transfers:[],forwards:[],busy:false,properties:null,propertyEpoch:0,terminal:null,terminalEpoch:0,
};
const invoke = (action,args={}) => {
  if (!window.__TAURI__) return Promise.reject(new Error('请在 RexETerm Tauri 应用中打开此界面。'));
  return window.__TAURI__.core.invoke('desktop_call',{action,args});
};
const perform = fn => Promise.resolve().then(fn).catch(error => toast(error,true));
const hostArgs = () => ({host:state.host.id});
const connected = id => state.runtime.executors?.some(node => node.id === id);
const hostName = id => state.saved.hosts.find(host => host.id === id)?.name || id;
const empty = (name,title,description='',error=false) => `<div class="empty ${error?'error':''}">${icon(name)}<strong>${esc(title)}</strong><p>${esc(description)}</p></div>`;

$('#app').innerHTML = `
<header class="topbar">
  <div class="brand"><span class="brand-mark">R</span>RexETerm<small>WORKSPACE</small></div>
  <div class="connection-badge"><span id="connection-dot" class="dot"></span><strong id="connection-name">尚未连接</strong><span id="connection-url" class="mono"></span></div>
  <div class="spacer"></div><span class="top-caption">远端工作区</span><div class="divider"></div>
  <div class="mcp-group"><button id="mcp-toggle" class="mcp-switch" role="switch" aria-checked="false" title="开启 MCP HTTP"><span>MCP HTTP</span><span class="switch-track"></span></button><button id="mcp-settings" class="icon-button" title="MCP HTTP 设置">${icon('settings')}</button></div>
</header>
<div class="workspace">
  <nav class="rail" aria-label="主导航"><button class="rail-button" data-side="overview" title="当前 App 总览">${icon('overview')}<span>总览</span></button>
    <button class="rail-button active" data-side="hosts" title="Host 列表">${icon('hosts')}<span>HOSTS</span></button>
    <button class="rail-button" data-side="folders" title="当前 RE 的文件夹列表">${icon('folder')}<span>文件夹</span></button>
    <button class="rail-button" data-side="management" title="各 Host 的全局运维状态">${icon('settings')}<span>运维</span></button><div class="rail-footer">REXETERM · PREVIEW</div>
  </nav>
  <aside class="sidebar"><div class="side-heading"><span id="side-title">HOST 列表</span><span id="side-count" class="counter"></span><div class="spacer"></div><button id="side-refresh" class="icon-button" title="刷新">${icon('refresh','sm')}</button><button id="add-host" class="icon-button" title="添加 Host">${icon('plus','sm')}</button></div><div class="side-search"><span>${icon('search')}</span><input id="host-filter" placeholder="筛选 Host…" aria-label="筛选 Host"></div><div id="side-body" class="side-body"></div><section id="properties" class="properties" aria-label="选中项目的属性" hidden></section><div class="side-footer">${icon('info','sm')}<span id="side-footer-text">Host 配置保存在此设备</span></div></aside>
  <div class="splitter-vertical" id="side-resizer" title="调整侧栏宽度"></div>
  <main class="work-area">
    <div class="document-tabbar"><button id="files-tab" class="document-tab active">${icon('folder')}<span>文件浏览器</span></button><button id="preview-tab" class="document-tab" hidden>${icon('code')}<span id="preview-tab-name">预览</span><span class="tab-dot"></span></button><div class="document-hint">${icon('connect','sm')}<span id="document-hint">连接 Host 后浏览文件</span></div></div>
    <section id="management" class="overview management" aria-label="Host 运维" hidden></section><section id="overview" class="overview" aria-label="当前 App 总览" hidden></section><section class="explorer" aria-label="文件浏览器">
      <div class="browser-toolbar">
        <button id="go-back" class="icon-button" title="后退" disabled>${icon('back')}</button><button id="go-up" class="icon-button" title="上一级 · Alt+↑" disabled>${icon('up')}</button><button id="refresh-files" class="icon-button" title="刷新目录" disabled>${icon('refresh')}</button>
        <div id="path-control" class="path-control"><div id="breadcrumbs" class="breadcrumbs"><span class="crumb">选择一个 Host 开始</span></div><input id="path-edit" class="path-edit" spellcheck="false" aria-label="远端目录路径"><button id="edit-path" class="icon-button" title="输入目录路径">${icon('code','sm')}</button></div>
        <form id="search-form" class="search-control">${icon('search')}<input id="search-input" placeholder="搜索当前目录…" autocomplete="off" spellcheck="false" aria-label="搜索文件"><select id="search-mode" aria-label="搜索方式"><option value="name">文件名</option><option value="content">内容</option></select><button type="button" id="clear-search" class="icon-button" title="清除搜索" hidden>${icon('close','sm')}</button><span class="keyhint">Ctrl K</span></form>
      </div>
      <div class="explorer-actions"><button id="upload-file" class="subtle-action" disabled>${icon('upload')}上传文件</button><button id="download-file" class="subtle-action" disabled>${icon('download')}下载</button><div class="spacer"></div><span id="view-summary" class="view-summary">尚未加载目录</span><div class="divider"></div><span class="small muted" id="view-kind">远端文件</span></div>
      <div class="browser-content"><div id="file-list" class="file-list"><div class="loading-line"></div><div class="file-columns"><span class="col-name">名称 ↑</span><span class="col-type">类型</span><span class="col-action"></span></div><div id="file-rows" class="file-rows">${empty('folderOpen','你的远端工作区','从左侧选择 Host，浏览目录和文件。')}</div><button id="more-files" class="more-files" hidden>加载更多文件</button></div>
</div>
    </section>
    <section id="center-preview" class="center-preview" hidden>      <aside id="preview" class="preview" hidden><div class="preview-heading">${icon('code')}<span class="filename" id="preview-name"></span><button class="icon-button" id="preview-download" title="下载文件">${icon('download','sm')}</button><button class="icon-button" id="close-preview" title="关闭预览">${icon('close','sm')}</button></div><div id="preview-meta" class="preview-meta"></div><pre id="preview-content" class="preview-content"></pre><div id="preview-note" class="preview-note">只读预览</div></aside><div id="terminal-preview" class="terminal-preview" hidden><div class="terminal-toolbar">${icon('terminal')}<span id="terminal-title" class="mono"></span><span id="terminal-badge" class="tag">连接中</span><div class="spacer"></div><button id="terminal-control" class="subtle-action" disabled>获取主控</button><button id="terminal-close" class="icon-button" title="断开终端预览（保留远端任务）">${icon('close','sm')}</button></div><div id="terminal-view" class="terminal-view"></div><div class="terminal-footnote">PTY · 输入直接发送到当前任务 · 关闭预览会释放本窗口的终端通道</div></div></section>
    <div id="panel-resizer" class="splitter-horizontal" title="调整底部面板高度"></div>
    <section id="bottom-panel" class="bottom-panel" aria-label="活动列表"><div class="panel-tabs">
      <button class="panel-tab active" data-panel="tasks" title="exbash 管理的程序">${icon('terminal')}任务<span id="tasks-count" class="count">0</span></button>
      <button class="panel-tab" data-panel="transfers">${icon('transfer')}文件传输<span id="transfers-count" class="count">0</span></button>
      <button class="panel-tab" data-panel="forwards">${icon('forward')}端口转发<span id="forwards-count" class="count">0</span></button>
      <div class="panel-tools"><span id="panel-scope" class="small muted"></span><button id="panel-add" class="subtle-action">${icon('plus','sm')}<span>运行命令</span></button><button id="panel-refresh" class="icon-button" title="刷新列表">${icon('refresh','sm')}</button><button id="collapse-panel" class="icon-button" title="收起面板">${icon('down','sm')}</button></div>
    </div><div id="panel-body" class="panel-body">${empty('terminal','尚未连接 Host','连接后查看 exbash 管理的程序。')}</div></section>
  </main>
</div>
<footer class="statusbar"><span class="status-item current-status"><span id="status-dot" class="dot"></span><span id="status-connection">未连接</span></span><span id="status-os" class="status-item"></span><span id="status-items"></span><div class="spacer"></div><span id="status-message">就绪</span><span class="keyboard">Ctrl K</span><span class="mono">TAURI 2 · PREVIEW</span></footer>
<div id="toasts" class="toast-container" aria-live="polite"></div>
<dialog id="dialog"><form id="dialog-form"><div class="dialog-heading"><span id="dialog-icon"></span><span id="dialog-title"></span><button type="button" id="dialog-close" class="icon-button" title="关闭">${icon('close')}</button></div><div id="dialog-subtitle" class="dialog-subtitle"></div><div id="dialog-fields" class="dialog-fields"></div><div id="dialog-error" class="dialog-error" hidden></div><div class="dialog-actions"><button type="button" id="dialog-cancel" class="secondary">取消</button><button id="dialog-submit" class="primary" type="submit">确认</button></div></form></dialog>`;

function toast(message,error=false) {
  const element = document.createElement('div'); element.className=`toast ${error?'error':''}`;
  element.textContent=String(message); $('#toasts').append(element); setTimeout(()=>element.remove(),error?8500:4000);
}
function status(message){ $('#status-message').textContent=message; }
function updateWorkspace(value) { state.saved=value.saved; state.runtime=value.runtime; renderSide(); renderHeader(); }
function renderHeader() {
  const active = state.host && connected(state.host.id);
  $('#connection-name').textContent=state.host?.name || '尚未连接'; $('#connection-url').textContent=state.host?.url || '';
  $('#connection-dot').classList.toggle('live',Boolean(active)); $('#status-dot').classList.toggle('live',Boolean(active));
  $('#status-connection').textContent=active?`${state.host.name} · 已连接`:'未连接';
  const info = state.runtime.executors?.find(node=>node.id===state.host?.id);
  $('#status-os').innerHTML=info?`${icon(info.system==='windows'?'windows':'hosts','sm')}<span>${esc(info.system || 'RE')}</span>`:'';
  $('#document-hint').textContent=active?`RE · ${info?.device || state.host.name}`:'连接 Host 后浏览文件';
  $('#upload-file').disabled=!active; $('#refresh-files').disabled=!active; $('#go-up').disabled=!active;
  $('#panel-add').disabled=!active; $('#panel-scope').textContent=(state.host?.name || '')+(state.panel==='tasks'?' · exbash':'');
  $('#go-back').disabled=state.history.length<2; $('#edit-path').disabled=!active;
}
function renderSide() {
  document.querySelectorAll('[data-side]').forEach(button=>button.classList.toggle('active',button.dataset.side===state.side));
  const hosts=state.side==='hosts'; renderProperties();
  if(state.side==='management'){
    $('#side-title').textContent='RE 运维';$('#side-count').textContent='';$('.side-search').hidden=true;$('#add-host').hidden=true;$('#side-footer-text').textContent='独立管理连接 · 全局只读';
    $('#side-body').innerHTML='<div class="side-group-label">运维页面</div>'+managementPages.map(([id,symbol,label])=>`<button class="overview-nav ${id===state.managementPage?'active':''}" data-management-page="${id}">${icon(symbol)}${label}</button>`).join('');
    bindManagementPages($('#side-body'));return;
  }
  if(state.side==='overview'){
    $('#side-title').textContent='工作区总览';$('#side-count').textContent='';$('.side-search').hidden=true;$('#add-host').hidden=true;$('#side-footer-text').textContent='显示此 App 管理的连接与活动';
    $('#side-body').innerHTML='<div class="side-group-label">当前 APP</div>'+[['executors','hosts','执行器'],['ptys','terminal','PTY'],['transfers','transfer','文件传输'],['forwards','forward','转发通道']].map(([id,symbol,label])=>`<button class="overview-nav" data-overview-anchor="${id}">${icon(symbol)}${label}</button>`).join('');
    $('#side-body').querySelectorAll('[data-overview-anchor]').forEach(button=>button.addEventListener('click',()=>$('#overview-'+button.dataset.overviewAnchor)?.scrollIntoView({behavior:'smooth',block:'start'})));
    return;
  } $('#side-title').textContent=hosts?'HOST 列表':'文件夹';
  $('#side-count').textContent=hosts?String(state.saved.hosts.length):'';
  $('.side-search').hidden=!hosts; $('#add-host').hidden=!hosts;
  $('#side-footer-text').textContent=hosts?'Host 配置保存在此设备':state.host?.name || '选择 Host 后查看文件夹';
  if(!hosts){renderTree();return;}
  const filter=$('#host-filter').value.toLowerCase();
  const rows=state.saved.hosts.filter(host=>`${host.name} ${host.url}`.toLowerCase().includes(filter));
  $('#side-body').innerHTML=`<div class="side-group-label">已保存的连接</div>`+rows.map(host=>`<div class="host-row ${host.id===state.host?.id?'selected':''}" data-host="${esc(host.id)}" tabindex="0" role="button"><span class="host-symbol">${icon(host.url.includes('127.0.0.1')?'windows':'hosts')}</span><div class="host-row-text"><div class="host-name">${esc(host.name)}</div><div class="host-url">${esc(host.url)}</div><div class="host-meta ${connected(host.id)?'online':''}"><span class="dot ${connected(host.id)?'live':''}"></span>${connected(host.id)?'已连接':'未连接'}${host.id===state.host?.id?'<span class="tag connected">当前</span>':''}</div></div><span class="host-connect">${icon('connect','sm')}</span></div>`).join('')+(rows.length?'':'<div class="sidebar-empty">没有匹配的 Host</div>');
  $('#side-body').querySelectorAll('[data-host]').forEach(row=>{
    row.addEventListener('click',()=>perform(()=>connectHost(row.dataset.host)));
    row.addEventListener('keydown',event=>{if(event.key==='Enter')perform(()=>connectHost(row.dataset.host));});
  });
}
async function connectHost(id) {
  if(state.busy) return;
  const host=state.saved.hosts.find(host=>host.id===id); if(!host)return;
  const epoch=++state.epoch; state.busy=true; state.host=host; state.history=[]; state.path=host.root; state.entries=[];state.search=null;state.selected=null;state.tasks=[];state.forwards=[];
  state.panelEpoch++;state.panelBusy=false;state.properties=null;state.propertyEpoch++;closePreview(); renderSide();renderHeader();
  $('#file-rows').innerHTML=empty('connect','正在连接',host.name); $('#file-list').classList.add('busy'); status(`正在连接 ${host.name}…`);
  try {
    const workspace=await invoke('connect',{host:id});if(epoch!==state.epoch)return;
    updateWorkspace(workspace); state.busy=false;
    await openDirectory(host.root); state.expanded.add(treeKey(host.root)); await loadTree(host.root);
    status('已连接'); await refreshPanel();
  } catch(error) {
    if(epoch===state.epoch){$('#file-rows').innerHTML=empty('connect','连接或目录加载失败',String(error),true);status('连接失败');renderHeader();}
    throw error;
  } finally {if(epoch===state.epoch){state.busy=false;$('#file-list').classList.remove('busy');}}
}
function treeKey(path){return `${state.host?.id}::${path}`;}
async function loadTree(path) {
  if(!state.host || !connected(state.host.id))return;
  const host=state.host.id,key=treeKey(path);
  if(!state.tree.has(key)){
    try{const result=await invoke('browse',{host,path});state.tree.set(key,result.entries);}
    catch(error){state.tree.set(key,{error:String(error)});}
  }
  if(host===state.host?.id && state.side==='folders')renderTree();
}
function renderTree() {
  if(!state.host || !connected(state.host.id)){$('#side-body').innerHTML='<div class="sidebar-empty">先连接一个 Host，再浏览它的文件夹。</div>';return;}
  const root=state.host.root;
  const row=(entry,depth)=>{
    const key=treeKey(entry.path),directory=entry.directory!==false,open=directory&&state.expanded.has(key),children=state.tree.get(key);
    let html=`<div class="tree-row ${(state.selected?.path||state.path)===entry.path?'active':''}" style="padding-left:${12+depth*15}px" data-tree-path="${esc(entry.path)}">${directory?`<button class="tree-chevron" data-expand="${esc(entry.path)}" title="展开文件夹">${icon(open?'down':'chevron')}</button>`:'<span class="tree-chevron"></span>'}${icon(directory?(open?'folderOpen':'folder'):'file')}<span class="tree-name">${esc(entry.name)}</span></div>`;
    if(open){if(Array.isArray(children))html+=children.map(child=>row(child,depth+1)).join('');else html+=`<div class="tree-row loading" style="padding-left:${42+depth*15}px">${children?.error?'无法读取此文件夹':'正在读取…'}</div>`;}
    return html;
  };
  $('#side-body').innerHTML=`<div class="tree-root">${icon('windows','sm')}<span class="mono">${esc(state.host.name)}</span></div>`+row({name:basename(root),path:root},0);
  $('#side-body').querySelectorAll('[data-tree-path]').forEach(element=>element.addEventListener('click',()=>perform(async()=>{const path=element.dataset.treePath;const entry=[...state.tree.values()].flatMap(v=>Array.isArray(v)?v:[]).find(e=>e.path===path);if(entry?.directory===false)await selectFile(entry);else await openDirectory(path);})));
  $('#side-body').querySelectorAll('[data-expand]').forEach(element=>element.addEventListener('click',event=>{
    event.stopPropagation();const path=element.dataset.expand,key=treeKey(path);
    if(state.expanded.has(key))state.expanded.delete(key);else{state.expanded.add(key);perform(()=>loadTree(path));}renderTree();
  }));
}
function renderBreadcrumbs() {
  $('#breadcrumbs').innerHTML=breadcrumbs(state.path).map((crumb,index)=>`${index?icon('chevron'):''}<button class="crumb" data-path="${esc(crumb.path)}">${esc(crumb.name)}</button>`).join('') || '<button class="crumb" data-path="/">/</button>';
  $('#breadcrumbs').querySelectorAll('[data-path]').forEach(element=>element.addEventListener('click',()=>perform(()=>openDirectory(element.dataset.path))));
  $('#path-edit').value=state.path; $('#path-control').classList.remove('editing');
}
async function openDirectory(path,{history=true,append=false}={}) {
  if(!state.host)return;const host=state.host.id,epoch=++state.epoch;const offset=append?state.nextOffset:1;
  if(!append){state.search=null;$('#search-input').value='';$('#clear-search').hidden=true;state.selected=null;closePreview();}
  $('#file-list').classList.add('busy');status('正在读取目录…');
  try{
    const result=await invoke('browse',{host,path,offset});if(epoch!==state.epoch || host!==state.host?.id)return;
    state.path=result.path;state.entries=append?[...state.entries,...result.entries]:result.entries;state.nextOffset=result.nextOffset;state.properties={file:result.file,path:result.path};state.propertyEpoch++;renderProperties();
    if(history && state.history.at(-1)!==state.path)state.history.push(state.path);
    renderBreadcrumbs();renderFiles();renderHeader();if(state.side==='folders')renderTree();
    status('就绪');
  }finally{if(epoch===state.epoch)$('#file-list').classList.remove('busy');}
}
function renderFiles() {
  $('#more-files').hidden=!state.nextOffset || Boolean(state.search);
  $('#download-file').disabled=!state.selected || state.selected.directory;
  const entries=state.search?.entries || state.entries;
  $('#view-summary').textContent=state.search?`${entries.length} 个搜索结果`:`${state.entries.filter(e=>e.directory).length} 个文件夹 · ${state.entries.filter(e=>!e.directory).length} 个文件`;
  $('#view-kind').textContent=state.search?(state.search.content?'内容搜索 · 正则':'文件名搜索'):'远端文件';
  $('#status-items').textContent=state.search?`${entries.length} 个匹配`:`${state.entries.length} 个项目`;
  if(!entries.length){$('#file-rows').innerHTML=empty(state.search?'search':'folderOpen',state.search?'没有搜索结果':'此文件夹为空',state.search?'尝试更换关键词或搜索方式。':'可以上传文件，或输入其他目录路径。');return;}
  if(state.search){
    $('#file-rows').innerHTML=entries.map((entry,index)=>`<div class="search-hit ${state.selected?.path===entry.path?'selected':''}" data-result="${index}" tabindex="0"><div class="search-hit-title">${icon('file','sm')}<span>${esc(basename(entry.path))}</span>${entry.line?`<span class="tag">L${entry.line}</span>`:''}</div><div class="search-hit-path">${esc(entry.path)}</div>${entry.text?`<div class="search-hit-snippet">${esc(entry.text)}</div>`:''}</div>`).join('');
    $('#file-rows').querySelectorAll('[data-result]').forEach(row=>row.addEventListener('click',()=>perform(()=>selectFile(entries[Number(row.dataset.result)]))));return;
  }
  $('#file-rows').innerHTML=entries.map((entry,index)=>`<div class="file-row ${entry.directory?'is-directory':''} ${state.selected?.path===entry.path?'selected':''}" data-entry="${index}" role="button" tabindex="0"><div class="file-name">${icon(entry.directory?'folder':/\.(rs|js|ts|toml|json|css|html)$/.test(entry.name)?'code':'file','file-icon')}<span>${esc(entry.name)}</span></div><div class="file-type">${esc(entry.directory?'文件夹':fileType(entry.name))}</div><div class="col-action">${entry.directory?icon('chevron','arrow'):`<button class="icon-button download-entry" data-download="${index}" title="下载文件">${icon('download','sm')}</button>`}</div></div>`).join('');
  $('#file-rows').querySelectorAll('[data-entry]').forEach(row=>{
    const entry=entries[Number(row.dataset.entry)];
    row.addEventListener('click',()=>perform(async()=>{if(entry.directory){state.selected=entry;renderFiles();await selectProperties(entry.path);}else await selectFile(entry);}));
    row.addEventListener('dblclick',()=>perform(()=>entry.directory?openDirectory(entry.path):selectFile(entry)));
    row.addEventListener('keydown',event=>{if(event.key==='Enter')perform(()=>entry.directory?openDirectory(entry.path):selectFile(entry));});
  });
  $('#file-rows').querySelectorAll('[data-download]').forEach(button=>button.addEventListener('click',event=>{event.stopPropagation();perform(()=>transferDialog('download',entries[Number(button.dataset.download)].path));}));
}
async function selectFile(entry) {
  detachTerminal();const epoch=++state.previewEpoch,host=state.host.id;state.selected={...entry,directory:false};renderFiles();
  showCenter('file');$('#preview-tab-name').textContent=basename(entry.path);$('#preview-name').textContent=basename(entry.path);$('#preview-content').textContent='正在读取文件…';$('#preview-meta').textContent='';$('#preview-note').textContent='只读预览';
  try{
    const result=await invoke('preview',{host,path:entry.path});if(epoch!==state.previewEpoch || host!==state.host?.id)return;
    state.properties={file:result.file,path:entry.path};state.propertyEpoch++;renderProperties();if(state.side==='folders')renderTree();
    $('#preview-content').textContent=result.content;
    $('#preview-meta').innerHTML=`<span>${esc(fileType(entry.path))}</span><span>·</span><span>${esc(formatBytes(result.file.size))}</span><span>·</span><span>${result.file.mtimeMs?esc(new Date(result.file.mtimeMs).toLocaleString()):'修改时间未知'}</span>`;
    $('#preview-note').textContent=result.mode==='binary'?'二进制文件 · 前 128 字节':result.mode==='large'?'大文件 · 下载后打开':`只读预览 · ${result.info || '前 300 行'}`;
  }catch(error){if(epoch===state.previewEpoch){$('#preview-content').textContent=String(error);$('#preview-note').textContent='无法预览';}}
}
function closePreview(){state.previewEpoch++;detachTerminal();$('#center-preview').hidden=true;$('#preview-tab').hidden=true;$('#files-tab').classList.add('active');$('.explorer').hidden=false;}
async function searchFiles() {
  if(!state.host)return;const query=$('#search-input').value.trim();if(!query){await openDirectory(state.path,{history:false});return;}
  const epoch=++state.epoch,host=state.host.id;$('#file-list').classList.add('busy');$('#clear-search').hidden=false;status('正在搜索…');
  try{const result=await invoke('search',{host,path:state.path,query,content:$('#search-mode').value==='content'});if(epoch!==state.epoch)return;
    state.search=result;state.selected=null;closePreview();renderFiles();status(result.info?.timedOut?'搜索达到时间限制，已显示当前结果':result.info?.truncated?'已显示前 100 个结果':'搜索完成');
  }finally{if(epoch===state.epoch)$('#file-list').classList.remove('busy');}
}

function setPanel(panel) {
  const same=state.panel===panel;state.panel=panel;state.panelEpoch++;state.panelBusy=false;
  if(same && $('#bottom-panel').classList.contains('collapsed'))$('#bottom-panel').classList.remove('collapsed');
  document.querySelectorAll('[data-panel]').forEach(button=>button.classList.toggle('active',button.dataset.panel===panel));
  $('#panel-add span').textContent=({tasks:'运行命令',transfers:'上传文件',forwards:'新建转发'})[panel];
  renderHeader();renderPanel();perform(()=>refreshPanel());
}
const pill = status => `<span class="pill ${esc(status)}"><span class="dot ${['running','done'].includes(status)?'live':''}"></span>${({running:'运行中',stopped:'已退出',done:'已完成',cancelled:'已取消',error:'失败'})[status] || esc(status)}</span>`;
function renderPanel() {
  const host=state.host?.id;const transfers=state.transfers.filter(row=>row.host===host);const forwards=state.forwards.filter(row=>row.executor===host);
  $('#tasks-count').textContent=state.tasks.length;$('#transfers-count').textContent=transfers.length;$('#forwards-count').textContent=forwards.length;
  if(!host || !connected(host)){$('#panel-body').innerHTML=empty('connect','尚未连接 Host','连接后查看当前 RE 的活动。');return;}
  let html='';
  if(state.panel==='tasks'){
    const rows=state.tasks;
    if(!rows.length){$('#panel-body').innerHTML=empty('terminal','暂无任务','通过「运行命令」创建 exbash 任务，或刷新已有任务。');return;}
    html=`<table class="panel-table"><thead><tr><th style="width:10%">状态</th><th style="width:18%">标题</th><th style="width:18%">任务 ID</th><th style="width:22%">命令</th><th style="width:16%">工作目录</th><th style="width:11%">创建时间</th><th style="width:5%"></th></tr></thead><tbody>`+rows.slice().reverse().map(row=>`<tr class="task-row" data-task="${esc(row.asyncID)}" tabindex="0" title="exbash ${esc(row.asyncID)} · PID ${esc(row.pid || '—')} · ${esc(row.command)}"><td>${pill(row.state)}</td><td title="${esc(row.description || '')}">${esc(row.title || '未命名')}</td><td class="mono">${esc(row.asyncID)}</td><td class="command" title="${esc(row.command)}">${esc(row.command || row.description)}</td><td class="small muted" title="${esc(row.cwd)}">${esc(row.cwd || '—')}</td><td class="small muted">${row.startedAt?esc(new Date(row.startedAt).toLocaleTimeString()):'—'}</td><td><div class="row-actions">${row.state==='running'?`<button class="icon-button danger" data-stop-task="${esc(row.asyncID)}" title="停止任务">${icon('stop','sm')}</button>`:''}</div></td></tr>`).join('')+'</tbody></table>';
  } else if(state.panel==='transfers'){
    if(!transfers.length){$('#panel-body').innerHTML=empty('transfer','暂无文件传输','上传或下载文件后，在这里查看进度。');return;}
    html=`<table class="panel-table"><thead><tr><th style="width:10%">方向</th><th>标题</th><th style="width:30%">文件</th><th style="width:20%">状态 / 进度</th><th style="width:22%">已传输</th><th style="width:13%">平均速率</th><th style="width:5%"></th></tr></thead><tbody>`+transfers.slice().reverse().map(row=>`<tr title="${esc(row.error || (row.localPath+' ↔ '+row.remotePath))}"><td class="small">${icon(row.mode==='upload'?'upload':'download','sm')} ${row.mode==='upload'?'上传':'下载'}</td><td title="${esc(row.description || '')}">${esc(row.title || '未命名')}</td><td>${esc(basename(row.remotePath))}</td><td>${pill(row.status)}${row.status==='running'?`<div class="progress-track"><div class="progress-value" style="width:${Math.min(100,row.total?row.bytes/row.total*100:0)}%"></div></div>`:''}</td><td class="rate">${esc(formatBytes(row.bytes))} / ${esc(formatBytes(row.total))}</td><td class="rate">${Number.isFinite(row.rate)?`${(row.rate/1e6).toFixed(2)} MB/s`:'—'}</td><td><div class="row-actions">${row.status==='running'?`<button class="icon-button" data-cancel-transfer="${esc(row.id)}" title="取消传输">${icon('close','sm')}</button>`:''}</div></td></tr>`).join('')+'</tbody></table>';
  }else{
    if(!forwards.length){$('#panel-body').innerHTML=empty('forward','暂无端口转发','将本地 TCP 端口连接到远端服务。');return;}
    html=`<table class="panel-table"><thead><tr><th style="width:12%">状态</th><th>标题</th><th style="width:30%">本地监听</th><th style="width:31%">远端目标</th><th style="width:12%">活动连接</th><th style="width:10%">协议</th><th style="width:5%"></th></tr></thead><tbody>`+forwards.map(row=>`<tr title="${esc(row.lastError || row.forwardID)}"><td>${pill(row.running?'running':'stopped')}</td><td title="${esc(row.description || '')}">${esc(row.title || '未命名')}</td><td class="command">${esc(row.listen)}</td><td class="command">${esc(row.target?.address)}</td><td class="mono">${esc(row.activeConnections)}</td><td class="small muted">TCP</td><td><div class="row-actions"><button class="icon-button danger" data-stop-forward="${esc(row.forwardID)}" title="停止转发">${icon('close','sm')}</button></div></td></tr>`).join('')+'</tbody></table>';
  }
  $('#panel-body').innerHTML=html;
  $('#panel-body').querySelectorAll('[data-task]').forEach(row=>{const open=()=>perform(()=>openTask(state.tasks.find(task=>task.asyncID===row.dataset.task)));row.addEventListener('click',event=>{if(!event.target.closest('button'))open();});row.addEventListener('keydown',event=>{if(event.key==='Enter')open();});});
  $('#panel-body').querySelectorAll('[data-stop-task]').forEach(button=>button.addEventListener('click',()=>perform(async()=>{await invoke('stop_task',{...hostArgs(),id:button.dataset.stopTask});await refreshPanel();})));
  $('#panel-body').querySelectorAll('[data-stop-forward]').forEach(button=>button.addEventListener('click',()=>perform(async()=>{await invoke('stop_forward',{id:button.dataset.stopForward});await refreshPanel();})));
  $('#panel-body').querySelectorAll('[data-cancel-transfer]').forEach(button=>button.addEventListener('click',()=>perform(async()=>{await invoke('cancel_transfer',{id:button.dataset.cancelTransfer});await refreshPanel();})));
}
async function refreshPanel() {
  if(state.panelBusy || !state.host || !connected(state.host.id))return;
  const host=state.host.id,panel=state.panel,epoch=state.panelEpoch;state.panelBusy=true;
  try{
    const result=await invoke(panel,panel==='transfers'?{}:{host});
    if(epoch!==state.panelEpoch || host!==state.host?.id)return;
    if(panel==='tasks')state.tasks=result.metadata?.runs || [];else if(panel==='forwards')state.forwards=result.metadata?.forwards || [];else state.transfers=result;
    renderPanel();
  }catch(error){if(epoch===state.panelEpoch)$('#panel-body').innerHTML=empty('info','无法读取此列表',String(error),true);}
  finally{if(epoch===state.panelEpoch)state.panelBusy=false;}
}

let dialogSubmit=null;
function openDialog({title,symbol='settings',subtitle='',fields='',submit='确认',onSubmit}) {
  $('#dialog-title').textContent=title;$('#dialog-icon').innerHTML=icon(symbol);$('#dialog-subtitle').textContent=subtitle;
  $('#dialog-fields').innerHTML=fields;$('#dialog-error').hidden=true;$('#dialog-submit').textContent=submit;$('#dialog-submit').disabled=false;
  dialogSubmit=onSubmit;$('#dialog').showModal();$('#dialog-fields input')?.focus();
}
const field = (name,label,placeholder='',value='',required=true) => `<label class="field">${esc(label)}<input name="${esc(name)}" placeholder="${esc(placeholder)}" value="${esc(value)}" ${required?'required':''} autocomplete="off" spellcheck="false"></label>`;
function addHostDialog() {
  openDialog({title:'添加 Host',symbol:'hosts',subtitle:'保存一个 RE 执行器地址，连接后即可浏览文件。',submit:'保存 Host',fields:field('name','名称','例如：开发服务器')+field('url','RE 地址','tls://192.168.1.20:9001')+field('root','起始目录','例如：/home/user 或 E:\\Projects')+field('username','RE 用户名（可选）','','',false)+field('connectionProfile','认证配置文件（本机路径，可选）','','',false)+field('managementTarget','运维地址（可选，默认尝试业务端口 + 1）','例如 tls://192.168.1.20:9002','',false)+field('managementProfile','运维认证配置（可选，默认复用 Host 认证）','','',false),onSubmit:async data=>{
    const value=await invoke('save_host',{id:`host-${Date.now()}`,name:data.get('name').trim(),url:data.get('url').trim(),root:data.get('root').trim(),username:data.get('username')?.trim() || null,connectionProfile:data.get('connectionProfile')?.trim() || null,management:{target:data.get('managementTarget')?.trim() || null,connectionProfile:data.get('managementProfile')?.trim() || null}});updateWorkspace(value);toast('Host 已保存');
  }});
}
function runDialog() {
  const host=state.host.id,path=state.path;
  openDialog({title:'运行命令',symbol:'terminal',subtitle:`${state.host.name} · ${path}`,submit:'运行',fields:field('title','标题（最多 40 字符）','','未命名',false)+field('description','描述（最多 200 字符）','','',false)+field('command','命令','输入要在远端执行的命令'),onSubmit:async data=>{
    const result=await invoke('run',{host,path,command:data.get('command'),title:data.get('title'),description:data.get('description')});setPanel('tasks');if(result.metadata?.asyncID)await openTask(result.metadata);else showFinishedTask(data.get('command'),result);toast('命令已提交');
  }});
}
function forwardDialog() {
  const host=state.host.id;
  openDialog({title:'新建端口转发',symbol:'forward',subtitle:`通过 ${state.host.name} 访问远端 TCP 服务。`,submit:'创建转发',fields:field('title','标题（最多 40 字符）','','未命名',false)+field('description','描述（最多 200 字符）','','',false)+field('listen','本地监听','127.0.0.1:0','127.0.0.1:0')+field('target','远端目标','127.0.0.1:8080','127.0.0.1:8080')+'<div class="small muted">本地端口填 0 时自动分配可用端口。</div>',onSubmit:async data=>{
    const result=await invoke('start_forward',{host,listen:data.get('listen'),target:data.get('target'),title:data.get('title'),description:data.get('description')});setPanel('forwards');toast(`转发已创建：${result.metadata.listen}`);
  }});
}
async function transferDialog(mode,remotePath='') {
  if(!state.host)return;const host=state.host.id,path=state.path;
  const localPath=await window.__TAURI__.core.invoke('pick_local',{save:mode==='download',name:remotePath?basename(remotePath):null});if(!localPath)return;
  const remote=remotePath || joinPath(path,basename(localPath));
  openDialog({title:mode==='upload'?'上传文件':'下载文件',symbol:mode==='upload'?'upload':'download',subtitle:state.host.name,submit:mode==='upload'?'开始上传':'开始下载',fields:field('title','标题（最多 40 字符）','','未命名',false)+field('description','描述（最多 200 字符）','','',false)+field('localPath','本地路径','',localPath)+field('targetPath','远端路径','',remote)+'<label class="checkbox"><input type="checkbox" name="overwrite">允许覆盖已存在的文件</label>',onSubmit:async data=>{
    await invoke('start_transfer',{host,options:{mode,title:data.get('title'),description:data.get('description'),localPath:data.get('localPath'),targetPath:data.get('targetPath'),overwrite:data.get('overwrite')==='on'}});setPanel('transfers');toast('文件传输已开始');
  }});
}
function renderMcp() {
  $('#mcp-toggle').setAttribute('aria-checked',String(Boolean(state.mcp)));$('#mcp-toggle').title=state.mcp?`关闭 ${state.mcp}`:'开启 MCP HTTP';
}
async function toggleMcp(enabled) {
  $('#mcp-toggle').disabled=true;
  try{const result=await invoke('mcp_toggle',{enabled,listen:state.listen});state.mcp=result.address;renderMcp();toast(state.mcp?`MCP HTTP 已开启：${state.mcp}`:'MCP HTTP 已关闭，RE 连接保留');}
  finally{$('#mcp-toggle').disabled=false;}
}
function mcpDialog() {
  openDialog({title:'MCP HTTP',symbol:'network',subtitle:state.mcp?`正在监听 ${state.mcp}`:'向外部工具提供当前 App 工作区。关闭监听不会断开 RE。',fields:field('listen','监听地址','127.0.0.1:8080',state.listen),submit:state.mcp?'关闭监听':'开启监听',onSubmit:async data=>{state.listen=data.get('listen');await toggleMcp(!state.mcp);}});
}

$('#dialog-form').addEventListener('submit',async event=>{
  event.preventDefault();$('#dialog-submit').disabled=true;$('#dialog-error').hidden=true;
  try{await dialogSubmit(new FormData($('#dialog-form')));$('#dialog').close();}
  catch(error){$('#dialog-error').textContent=String(error);$('#dialog-error').hidden=false;}
  finally{$('#dialog-submit').disabled=false;}
});
$('#dialog-close').addEventListener('click',()=>$('#dialog').close());$('#dialog-cancel').addEventListener('click',()=>$('#dialog').close());
$('#add-host').addEventListener('click',addHostDialog);$('#host-filter').addEventListener('input',renderSide);
document.querySelectorAll('[data-side]').forEach(button=>button.addEventListener('click',()=>{chooseSide(button.dataset.side);}));
$('#side-refresh').addEventListener('click',()=>perform(async()=>{if(state.side==='management')await refreshManagement(true);else if(state.side==='overview')await refreshOverview();else if(state.side==='hosts')updateWorkspace(await invoke('workspace'));else if(state.host){state.tree.clear();renderTree();await loadTree(state.host.root);}}));
$('#go-up').addEventListener('click',()=>perform(()=>openDirectory(parentPath(state.path))));
$('#go-back').addEventListener('click',()=>perform(async()=>{state.history.pop();const path=state.history.at(-1);if(path)await openDirectory(path,{history:false});}));
$('#refresh-files').addEventListener('click',()=>perform(()=>openDirectory(state.path,{history:false})));
$('#edit-path').addEventListener('click',()=>{$('#path-control').classList.add('editing');$('#path-edit').focus();$('#path-edit').select();});
$('#path-edit').addEventListener('keydown',event=>{if(event.key==='Enter')perform(()=>openDirectory(event.target.value));if(event.key==='Escape')$('#path-control').classList.remove('editing');});
$('#search-form').addEventListener('submit',event=>{event.preventDefault();perform(()=>searchFiles());});
$('#clear-search').addEventListener('click',()=>perform(()=>openDirectory(state.path,{history:false})));
$('#more-files').addEventListener('click',()=>perform(()=>openDirectory(state.path,{history:false,append:true})));
$('#close-preview').addEventListener('click',closePreview);
$('#files-tab').addEventListener('click',()=>{$('#center-preview').hidden=true;$('.explorer').hidden=false;$('#files-tab').classList.add('active');$('#preview-tab').classList.remove('active');});
$('#preview-tab').addEventListener('click',()=>showCenter(state.previewKind||'file'));
$('#terminal-close').addEventListener('click',closePreview);
$('#terminal-control').addEventListener('click',()=>perform(async()=>{if(state.terminal)await invoke('terminal_control',{id:state.terminal.id});}));
$('#download-file').addEventListener('click',()=>perform(()=>transferDialog('download',state.selected.path)));
$('#preview-download').addEventListener('click',()=>perform(()=>transferDialog('download',state.selected.path)));
$('#upload-file').addEventListener('click',()=>perform(()=>transferDialog('upload')));
document.querySelectorAll('[data-panel]').forEach(button=>button.addEventListener('click',()=>setPanel(button.dataset.panel)));
$('#panel-add').addEventListener('click',()=>perform(()=>({tasks:runDialog,forwards:forwardDialog,transfers:()=>transferDialog('upload')})[state.panel]()));
$('#panel-refresh').addEventListener('click',()=>perform(refreshPanel));
$('#collapse-panel').addEventListener('click',()=>$('#bottom-panel').classList.toggle('collapsed'));
$('#mcp-toggle').addEventListener('click',()=>perform(()=>toggleMcp(!state.mcp)));$('#mcp-settings').addEventListener('click',mcpDialog);
function resize(handle,property,compute) {
  handle.addEventListener('pointerdown',event=>{
    event.preventDefault();handle.setPointerCapture(event.pointerId);
    const move=event=>document.documentElement.style.setProperty(property,`${compute(event)}px`);
    const up=()=>{handle.removeEventListener('pointermove',move);handle.removeEventListener('pointerup',up);};
    handle.addEventListener('pointermove',move);handle.addEventListener('pointerup',up);
  });
}
resize($('#side-resizer'),'--sidebar-width',event=>Math.max(180,Math.min(400,event.clientX-54)));
resize($('#panel-resizer'),'--panel-height',event=>Math.max(135,Math.min(500,window.innerHeight-event.clientY-25)));
document.addEventListener('keydown',event=>{
  if((event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='k'){event.preventDefault();$('#search-input').focus();$('#search-input').select();}
  if(event.altKey&&event.key==='ArrowUp'&&state.host){event.preventDefault();perform(()=>openDirectory(parentPath(state.path)));}
});
setInterval(()=>{if(!document.hidden)perform(state.side==='management'?()=>refreshManagement(false):state.side==='overview'?refreshOverview:refreshPanel);},1800);

function renderProperties() {
  const pane=$('#properties');if(!pane)return;pane.hidden=state.side!=='folders';
  const value=state.properties;
  if(!value){pane.innerHTML='<div class="property-title">属性</div><div class="property-empty">选择文件或文件夹，查看它的属性。</div>';return;}
  const file=value.file || {},directory=file.kind==='directory';
  pane.innerHTML=`<div class="property-title">${icon('info','sm')}属性</div><div class="property-name">${icon(directory?'folder':'file','sm')}<span>${esc(basename(value.path))}</span></div><dl><dt>类型</dt><dd>${esc(directory?'文件夹':fileType(value.path))}</dd><dt>大小</dt><dd>${directory?'—':esc(formatBytes(file.size))}</dd><dt>修改</dt><dd>${file.mtimeMs?esc(new Date(file.mtimeMs).toLocaleString()):'—'}</dd><dt>路径</dt><dd class="property-path">${esc(value.path)}</dd></dl>`;
}
async function selectProperties(path) {
  const epoch=++state.propertyEpoch,host=state.host.id;
  const result=await invoke('stat',{host,path});
  if(epoch!==state.propertyEpoch || host!==state.host?.id)return;
  state.properties={file:result.metadata.file,path};renderProperties();
}
function showCenter(kind) {
  state.previewKind=kind;$('.explorer').hidden=true;$('#center-preview').hidden=false;
  $('#preview').hidden=kind!=='file';$('#terminal-preview').hidden=kind!=='terminal';
  $('#preview-tab').hidden=false;$('#preview-tab').classList.add('active');$('#files-tab').classList.remove('active');
  if(kind==='terminal')requestAnimationFrame(fitTerminal);
}
let terminalUI=null,terminalFit=null,inputQueue=Promise.resolve();
function ensureTerminal() {
  if(terminalUI)return;
  terminalUI=new window.Terminal({fontFamily:'"Cascadia Code",Consolas,monospace',fontSize:13,lineHeight:1.3,cursorBlink:true,scrollback:5000,convertEol:false,allowProposedApi:false,theme:{background:'#101720',foreground:'#c6d4e7',cursor:'#a0dfc7',selectionBackground:'#35564d',black:'#182432',red:'#e8a0aa',green:'#9bd9b7',yellow:'#d4c68b',blue:'#90b1e6',magenta:'#bea2da',cyan:'#83c7d3',white:'#d4deed'}});
  terminalFit=new window.FitAddon.FitAddon();terminalUI.loadAddon(terminalFit);terminalUI.open($('#terminal-view'));
  terminalUI.onData(data=>{const session=state.terminal;if(!session || !session.controller)return;inputQueue=inputQueue.then(async()=>{if(session.id===state.terminal?.id)await invoke('terminal_input',{id:session.id,data});}).catch(error=>toast(error,true));});
  terminalUI.onResize(({cols,rows})=>{if(state.terminal?.controller)perform(()=>invoke('terminal_resize',{id:state.terminal.id,cols,rows}));});
  new ResizeObserver(()=>fitTerminal()).observe($('#terminal-view'));
}
function fitTerminal(){if((!state.terminal || state.terminal.controller) && terminalUI && !$('#terminal-preview').hidden && !$('#center-preview').hidden)terminalFit.fit();}
function detachTerminal() {
  state.terminalEpoch++;const old=state.terminal;state.terminal=null;
  if(old)perform(()=>invoke('terminal_close',{id:old.id}));
}
async function openTask(task) {
  if(!task)return;detachTerminal();state.previewEpoch++;
  const epoch=++state.terminalEpoch,host=state.host.id;
  ensureTerminal();showCenter('terminal');terminalUI.reset();terminalUI.options.disableStdin=true;
  $('#preview-tab-name').textContent=`任务 · ${task.title || '未命名'}`;$('#terminal-title').textContent=task.title || '未命名';$('#terminal-title').title=[task.description,task.command,task.asyncID].filter(Boolean).join(' · ');
  $('#terminal-badge').textContent='连接中';$('#terminal-control').disabled=true;
  try{
    const session=await invoke('terminal_open',{host,pty:task.asyncID,cols:terminalUI.cols,rows:terminalUI.rows});
    if(epoch!==state.terminalEpoch){await invoke('terminal_close',{id:session.id});return;}
    session.title=task.title || '未命名';session.description=task.description || '';state.terminal=session;fitTerminal();terminalUI.focus();pollTerminal(session,epoch);
  }catch(error){if(epoch===state.terminalEpoch){$('#terminal-badge').textContent='连接失败';terminalUI.writeln(String(error));}throw error;}
}
function updateTerminalRole(session) {
  terminalUI.options.disableStdin=!session.controller || session.exited;
  $('#terminal-badge').textContent=session.exited?`已退出 · ${session.exitCode}`:session.controller?'主控 · 可输入':'只读';
  $('#terminal-control').disabled=session.readOnly || session.controller || session.exited;
}
async function pollTerminal(session,epoch) {
  try{
    while(epoch===state.terminalEpoch && session.id===state.terminal?.id){
      const buffer=await window.__TAURI__.core.invoke('terminal_poll',{id:session.id});if(epoch!==state.terminalEpoch)return;
      const packet=decodeTerminalBatch(buffer);
      for(const event of packet.events){
        if(epoch!==state.terminalEpoch)return;
        if(event.type==='data'){await new Promise(resolve=>terminalUI.write(event.data,resolve));}
        else if(event.type==='meta'){
          session.exited=event.exit_code!==null && event.exit_code!==undefined;session.exitCode=event.exit_code;const controller=String(event.role).toLowerCase()==='controller';session.controller=controller;session.readOnly=Boolean(event.read_only);if(!controller && event.cols && event.rows)terminalUI.resize(event.cols,event.rows);updateTerminalRole(session);
        }else if(event.type==='error'){$('#terminal-badge').textContent='通道错误';terminalUI.writeln(`\r\n${event.message}`);}
      }
      if(packet.closed){session.closed=true;terminalUI.options.disableStdin=true;$('#terminal-badge').textContent='通道已结束';$('#terminal-control').disabled=true;return;}
      // terminal_poll waits when idle and returns immediately when bytes arrive.
    }
  }catch(error){if(epoch===state.terminalEpoch){$('#terminal-badge').textContent='已断开';terminalUI.options.disableStdin=true;toast(error,true);}}
}
function showFinishedTask(command,result) {
  detachTerminal();state.previewEpoch++;ensureTerminal();showCenter('terminal');terminalUI.reset();terminalUI.options.disableStdin=true;
  $('#preview-tab-name').textContent=`任务输出 · ${result.metadata?.title || '未命名'}`;$('#terminal-title').textContent=result.metadata?.title || '未命名';$('#terminal-title').title=[result.metadata?.description,command].filter(Boolean).join(' · ');$('#terminal-badge').textContent=`已退出 · ${result.metadata?.exitCode ?? '—'}`;$('#terminal-control').disabled=true;
  terminalUI.write((result.output?.text || '').replace(/\r?\n/g,'\r\n'));
}

function chooseSide(side) {
  const wasFull=['overview','management'].includes(state.side),full=['overview','management'].includes(side);
  if(full && !wasFull)state.resumePreview=!$('#center-preview').hidden;
  if(state.side==='management' && side!=='management'){
    state.managementEpoch++;state.managementBusy=false;
    if(state.managementHost)perform(()=>invoke('management_disconnect',{host:state.managementHost}));
  }
  state.side=side;
  $('#overview').hidden=side!=='overview';$('#management').hidden=side!=='management';$('.document-tabbar').hidden=full;$('#bottom-panel').hidden=full;$('#panel-resizer').hidden=full;
  if(full){$('.explorer').hidden=true;$('#center-preview').hidden=true;if(side==='overview')perform(refreshOverview);else{if(!state.managementHost)state.managementHost=state.host?.id || state.saved.hosts[0]?.id || null;renderManagement();perform(()=>refreshManagement(true));}}
  else if(state.resumePreview && !$('#preview-tab').hidden)showCenter(state.previewKind||'file');
  else{$('.explorer').hidden=false;$('#center-preview').hidden=true;}
  renderSide();if(side==='folders' && state.host)perform(()=>loadTree(state.host.root));
}
function bindManagementPages(root) {
  root.querySelectorAll('[data-management-page]').forEach(button=>button.addEventListener('click',()=>{state.managementPage=button.dataset.managementPage;renderSide();renderManagement();}));
}
function renderManagement() {
  $('#management').innerHTML=renderManagementView({hosts:state.saved.hosts,host:state.managementHost,page:state.managementPage,result:state.managementResult,busy:state.managementBusy});
  bindManagementPages($('#management'));
  $('#management-host')?.addEventListener('change',event=>{
    const old=state.managementHost;state.managementEpoch++;state.managementBusy=false;state.managementResult=null;state.managementHost=event.target.value;
    if(old)perform(()=>invoke('management_disconnect',{host:old}));
    renderManagement();perform(()=>refreshManagement(true));
  });
  $('#management-refresh')?.addEventListener('click',()=>perform(()=>refreshManagement(true)));
  $('#management-configure')?.addEventListener('click',managementDialog);
}
async function refreshManagement(force=false) {
  if(state.side!=='management' || state.managementBusy || !state.managementHost)return;
  if(!force && (state.managementResult?.status!=='connected' || document.activeElement===$('#management-host')))return;
  const host=state.managementHost,epoch=++state.managementEpoch;state.managementBusy=true;
  if(force)renderManagement();
  try{
    const result=await invoke('management_snapshot',{host});
    if(epoch!==state.managementEpoch || host!==state.managementHost || state.side!=='management')return;
    state.managementResult=result;
  }catch(error){if(epoch===state.managementEpoch)state.managementResult={host,status:'unavailable',message:String(error),snapshot:null};}
  finally{if(epoch===state.managementEpoch){state.managementBusy=false;if(state.side==='management')renderManagement();}}
}
function managementDialog() {
  const host=state.saved.hosts.find(h=>h.id===state.managementHost);if(!host)return;
  openDialog({title:'运维连接设置',symbol:'settings',subtitle:`${host.name} · 留空地址时尝试业务端口 + 1，认证配置留空时复用 Host 认证。`,fields:field('target','运维地址','tls://192.168.1.20:9002 或 unix:///run/rexed/admin.sock',host.management?.target || '',false)+field('profile','管理员认证配置文件（本机路径）','',host.management?.connectionProfile || '',false),submit:'保存并连接',onSubmit:async data=>{
    state.managementEpoch++;state.managementBusy=false;state.managementResult=null;renderManagement();
    const value=await invoke('management_configure',{host:host.id,settings:{target:data.get('target').trim() || null,connectionProfile:data.get('profile').trim() || null}});
    updateWorkspace(value);await refreshManagement(true);
  }});
}

let overviewBusy=false;
async function refreshOverview() {
  if(overviewBusy || state.side!=='overview')return;overviewBusy=true;
  try{
    const [workspace,allTransfers,forwardResult]=await Promise.all([invoke('workspace'),invoke('transfers'),invoke('forwards')]);
    if(state.side!=='overview')return;updateWorkspace(workspace);state.transfers=allTransfers;const transfers=allTransfers.filter(t=>t.status==='running');state.forwards=forwardResult.metadata?.forwards || [];
    const executors=state.runtime.executors || [],ptys=state.terminal && !state.terminal.closed && !state.terminal.exited?[state.terminal]:[],forwards=state.forwards.filter(f=>f.running);
    const cards=[['hosts','执行器',executors.length],['terminal','PTY',ptys.length],['transfer','文件传输',transfers.filter(t=>t.status==='running').length],['forward','转发通道',forwards.length]];
    const table=(headers,rows,none)=>rows.length?`<table class="overview-table"><thead><tr>${headers.map(h=>`<th>${h}</th>`).join('')}</tr></thead><tbody>${rows.join('')}</tbody></table>`:`<div class="overview-empty">${none}</div>`;
    const section=(id,title,rows)=>`<section class="overview-section" id="overview-${id}"><h2>${icon(id==='executors'?'hosts':id==='ptys'?'terminal':id==='transfers'?'transfer':'forward','sm')}${title}</h2>${rows}</section>`;
    $('#overview').innerHTML=`<div class="overview-heading"><div><div class="eyebrow">APP WORKSPACE</div><h1>总览</h1><p>当前 App 的活跃连接、终端与传输活动</p></div><button class="secondary" id="overview-refresh">${icon('refresh','sm')} 刷新</button></div><div class="overview-cards">${cards.map(([symbol,title,count])=>`<div class="overview-card">${icon(symbol)}<span>${title}</span><strong>${count}</strong>${symbol==='transfer'?'<small>正在传输</small>':''}</div>`).join('')}</div>`+
    section('executors','已连接的执行器',table(['执行器','地址','系统 / 设备',''],executors.map(e=>`<tr><td>${esc(hostName(e.id))}${e.id===state.host?.id?' <span class="tag connected">当前</span>':''}</td><td class="mono">${esc(state.saved.hosts.find(h=>h.id===e.id)?.url || e.url || '—')}</td><td>${esc([e.system,e.device].filter(Boolean).join(' · ') || '—')}</td><td><button class="subtle-action" data-overview-host="${esc(e.id)}">打开 ${icon('chevron','sm')}</button></td></tr>`),'尚未连接执行器。前往 Host 列表添加并连接。'))+
    section('ptys','已打开的 PTY',table(['任务 / PTY','执行器','状态',''],ptys.map(p=>`<tr><td title="${esc(p.description || '')}">${esc(p.title || '未命名')}<div class="small mono muted">${esc(p.pty)}</div></td><td>${esc(hostName(p.host))}</td><td>${esc($('#terminal-badge').textContent)}</td><td><button class="subtle-action" data-overview-pty>打开 ${icon('chevron','sm')}</button></td></tr>`),'暂无打开的 PTY。点击任务可打开终端。'))+
    section('transfers','文件传输',table(['文件','执行器','方向','状态','已传输',''],transfers.slice().reverse().map(t=>`<tr><td title="${esc(t.remotePath)}">${esc(basename(t.remotePath))}</td><td>${esc(hostName(t.host))}</td><td>${t.mode==='upload'?'上传':'下载'}</td><td>${pill(t.status)}</td><td class="mono">${esc(formatBytes(t.bytes))} / ${esc(formatBytes(t.total))}</td><td><button class="subtle-action" data-overview-panel="transfers" data-host="${esc(t.host)}">查看</button></td></tr>`),'当前没有文件传输。'))+
    section('forwards','转发通道',table(['本地监听','执行器','远端目标','活动连接',''],forwards.map(f=>`<tr><td class="mono">${esc(f.listen)}</td><td>${esc(hostName(f.executor))}</td><td class="mono">${esc(f.target?.address)}</td><td>${esc(f.activeConnections)}</td><td><button class="subtle-action" data-overview-panel="forwards" data-host="${esc(f.executor)}">查看</button></td></tr>`),'暂无转发通道。'));
    $('#overview-refresh').addEventListener('click',()=>perform(refreshOverview));
    $('#overview').querySelectorAll('[data-overview-host]').forEach(b=>b.addEventListener('click',()=>perform(async()=>{chooseSide('hosts');await connectHost(b.dataset.overviewHost);})));
    $('#overview').querySelectorAll('[data-overview-pty]').forEach(b=>b.addEventListener('click',()=>{chooseSide('folders');showCenter('terminal');terminalUI?.focus();}));
    $('#overview').querySelectorAll('[data-overview-panel]').forEach(b=>b.addEventListener('click',()=>perform(async()=>{chooseSide('hosts');if(state.host?.id!==b.dataset.host)await connectHost(b.dataset.host);chooseSide('folders');setPanel(b.dataset.overviewPanel);})));
  }catch(error){if(state.side==='overview')$('#overview').innerHTML=empty('info','无法读取总览',String(error),true);}
  finally{overviewBusy=false;}
}

async function boot() {
  try{
    updateWorkspace(await invoke('workspace'));const mcp=await invoke('mcp_status');state.mcp=mcp.address;renderMcp();
    const selected=state.saved.lastHost || state.saved.hosts[0]?.id;if(selected)await connectHost(selected);
    document.documentElement.dataset.ready='true';
  }catch(error){toast(error,true);status('请检查连接');document.documentElement.dataset.ready='error';}
}
boot();
