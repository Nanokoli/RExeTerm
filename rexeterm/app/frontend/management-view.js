import { escapeHTML as esc, formatBytes } from './utils.js';
import { icon } from './icons.js';

export const managementPages = [
  ['clients','hosts','客户端'], ['programs','terminal','任务'], ['pty','terminal','PTY 通道'],
  ['file','transfer','文件通道'], ['forward','forward','转发通道'],
];
const date = value => value ? esc(new Date(value).toLocaleString()) : '—';
const mono = value => `<span class="mono">${esc(value ?? '—')}</span>`;
const owner = value => value ? `${esc(value.username)} · ${esc(value.clientName)}<div class="small mono muted">客户端 ${esc(value.clientId || value.sessionId)}</div><div class="small mono muted">会话 ${esc(value.sessionId)}</div>` : '—';
const title = value => `<span title="${esc(value.description || '')}">${esc(value.title || '未命名')}</span>`;
const table = (headers, rows) => rows.length ? `<div class="management-scroll"><table class="management-table"><thead><tr>${headers.map(h=>`<th>${h}</th>`).join('')}</tr></thead><tbody>${rows.map(row=>`<tr>${row.map(cell=>`<td>${cell}</td>`).join('')}</tr>`).join('')}</tbody></table></div>` : '<div class="overview-empty">当前没有此类活动。</div>';
function rates(client) {
  const rate = value => Number.isFinite(value) && value >= 0 ? (value / 1e6).toFixed(2) : '—';
  return client.trafficComplete ? `${rate(client.traffic?.receiveBytesPerSecond)} / ${rate(client.traffic?.sendBytesPerSecond)}` : '<span class="muted" title="此客户端存在无法统计的链路">统计不完整</span>';
}
function clients(snapshot) {
  const groups = new Map();
  for (const client of snapshot.clients || []) {
    const name = client.owner?.username || '未知用户';
    if (!groups.has(name)) groups.set(name, []);
    groups.get(name).push(client);
  }
  if (!groups.size) return '<div class="overview-empty">当前没有已连接的客户端。</div>';
  return [...groups].sort(([a],[b])=>a.localeCompare(b)).map(([username, rows]) => `<details class="management-user" open><summary>${esc(username)}<span class="counter">${rows.length} 个客户端</span></summary>${table(['客户端 / 身份','来源 / 链路','入站 / 出站 MB/s','累计入站 / 出站','连接时间 / 最后心跳'],rows.map(c=>[
    `${esc(c.owner?.clientName)}<div class="small mono muted">${esc(c.owner?.clientId || c.owner?.sessionId)}</div><div class="small muted">会话 ${esc(c.owner?.sessionId)}</div>`,
    `${mono(c.peer)}<div class="small muted">${esc(c.transport)} · ${esc(c.activeLinks)} 条链路</div>`,
    `<span class="rate">${rates(c)}</span>`,
    `${esc(formatBytes(c.traffic?.receivedBytes))} / ${esc(formatBytes(c.traffic?.sentBytes))}`,
    `${date(c.createdAtMs)}<div class="small muted">${date(c.lastSeenAtMs)}</div>`,
  ]))}</details>`).join('');
}
function programs(snapshot) {
  return table(['标题 / 任务 ID','程序 / 工作目录','状态 / PID','创建者','创建时间','PTY 主控 / 会话'],(snapshot.programs || []).map(p=>[
    `${title(p)}<div class="small mono muted">${esc(p.programId)}</div>`,
    `<div class="management-command" title="${esc((p.command || []).join(' '))}">${mono((p.command || []).join(' '))}</div><div class="small muted" title="${esc(p.cwd || '')}">${esc(p.cwd || '—')}</div>`,
    `${esc(p.state)}<div class="small mono muted">PID ${esc(p.pid ?? '—')}</div>`,owner(p.createdBy),date(p.createdAtMs),
    `${mono(p.controller)}<div class="small muted">${owner(p.controllerSession)}</div>`,
  ]));
}
function channels(snapshot, kind) {
  const tasks = new Map((snapshot.programs || []).map(p=>[p.programId,p]));
  return table(['标题 / 通道 ID','目标','创建者','来源 / 链路','创建时间'],(snapshot.channels || []).filter(c=>c.kind===kind).map(c=>{
    const labels = kind==='pty' ? tasks.get(c.target) || {title:'未命名',description:''} : c.metadata || {};
    return [`${title(labels)}<div class="small mono muted">${esc(c.channelId)}</div>`, `<div class="management-command" title="${esc(c.target)}">${mono(c.target)}</div>`,owner(c.owner),`${mono(c.peer)}<div class="small muted">${esc(c.transport)}</div>`,date(c.createdAtMs)];
  }));
}
export function managementCount(snapshot, page) {
  if (!snapshot) return '—';
  return page==='clients' ? snapshot.clients?.length || 0 : page==='programs' ? snapshot.programs?.length || 0 : (snapshot.channels || []).filter(c=>c.kind===page).length;
}
export function renderManagementView({hosts, host, page='clients', result=null, busy=false}) {
  const snapshot = result?.status==='connected' ? result.snapshot : null;
  const states = {connected:'已连接', denied:'没有运维访问权限', unavailable:'运维端不可用或认证失败',unconfigured:'需要配置运维地址',invalid_configuration:'运维认证配置无效'};
  const heading = `<div class="overview-heading"><div><div class="eyebrow">REXED OPERATIONS</div><h1>运维</h1><p>独立管理连接 · 只读全局状态</p></div><div class="management-toolbar"><select id="management-host" aria-label="运维 Host">${hosts.map(h=>`<option value="${esc(h.id)}" ${h.id===host?'selected':''}>${esc(h.name)}</option>`).join('')}</select><button id="management-configure" class="secondary" ${host?'':'disabled'}>${icon('settings','sm')} 连接设置</button><button id="management-refresh" class="secondary" ${busy || !host?'disabled':''}>${icon('refresh','sm')} ${busy?'读取中…':'刷新'}</button></div></div>`;
  if (!hosts.length) return heading+'<div class="overview-empty">先在 Host 列表添加一个执行器。</div>';
  const connection = `<div class="management-connection"><span class="dot ${snapshot?'live':''}"></span><strong>${busy?'正在读取…':esc(states[result?.status] || '等待连接')}</strong><span class="mono muted">${esc(result?.target || '自动尝试业务端口 + 1')}</span>${snapshot?`<span class="small muted">观测于 ${date(snapshot.observedAtMs)}</span>`:''}</div>`;
  if (!snapshot) return heading+connection+`<div class="management-message" role="status">${esc(result?.message || '选择 Host 后尝试读取其运维端。远端默认 socket 需要单独填写可访问的管理地址。')}<p>运维端需要相应访问权限；可在连接设置中指定独立管理员配置。</p></div>`;
  const tabs=`<div class="management-tabs" role="tablist" aria-label="运维页面">${managementPages.map(([id,,name])=>`<button role="tab" aria-selected="${page===id}" data-management-page="${id}">${name}<span class="counter">${managementCount(snapshot,id)}</span></button>`).join('')}</div>`;
  const content = page==='clients'?clients(snapshot):page==='programs'?programs(snapshot):channels(snapshot,page);
  return heading+connection+tabs+`<section class="management-content">${content}</section><div class="management-footnote">${esc(snapshot.executorId)} · 运维连接不登记为业务客户端。速率为 RE 视角下该客户端全部链路的合计，1 MB = 1,000,000 字节。</div>`;
}
