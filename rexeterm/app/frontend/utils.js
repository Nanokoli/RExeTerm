export const escapeHTML = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
export function basename(path) { return String(path).replace(/[\\/]+$/, '').split(/[\\/]/).pop() || path; }
export function parentPath(path) {
  const p = String(path).replace(/[\\/]+$/, '');
  if (/^[A-Za-z]:$/.test(p)) return `${p}\\`;
  const index = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
  if (index < 0) return path;
  const parent = p.slice(0, index);
  if (!parent) return '/';
  return /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
}
export function joinPath(parent, name) { return `${parent.replace(/[\\/]+$/, '')}${parent.includes('\\') || /^[A-Za-z]:/.test(parent) ? '\\' : '/'}${name}`; }
export function formatBytes(bytes) {
  if (bytes === undefined || bytes === null) return '—';
  if (bytes < 1024) return `${bytes} B`;
  const unit = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 4);
  return `${(bytes / (1024 ** unit)).toFixed(unit === 1 ? 0 : 1)} ${['B','KB','MB','GB','TB'][unit]}`;
}
export function fileType(name) {
  const ext = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  return ({rs:'Rust',js:'JavaScript',ts:'TypeScript',json:'JSON',md:'Markdown',toml:'TOML',yml:'YAML',yaml:'YAML',html:'HTML',css:'CSS',txt:'文本',log:'日志',ps1:'PowerShell',sh:'Shell',exe:'应用程序',png:'PNG 图片',jpg:'JPEG 图片',zip:'ZIP 压缩包',gz:'GZip 压缩包'})[ext] || (ext ? ext.toUpperCase() : '文件');
}
export function breadcrumbs(path) {
  const windows = /^[A-Za-z]:/.test(path) || path.startsWith('\\\\');
  const separator = windows ? '\\' : '/';
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.map((name,index) => ({name: index === 0 && !windows ? `/${name}` : name, path:(windows ? '' : '/')+parts.slice(0,index+1).join(separator)+(index === 0 && windows ? '\\' : '')}));
}
