# 工作区前端

静态 ES modules，通过 Tauri command 调用 Rust 后台，无独立网页服务器。
app.js 负责 Host、文件树与属性、文件/PTY 预览及三个底栏；utils.js 提供路径与安全文本处理。
terminal-batch.js 解码 Tauri 二进制 PTY 批次，以字节视图交给 xterm；仅状态元数据解析 JSON。
style.css 定义工作台布局，icons.js 提供代码 SVG 图标。

vendor 包含 npm 官方发布的 @xterm/xterm 5.5.0 与 @xterm/addon-fit 0.10.0 浏览器文件及 MIT 许可证，
随应用打包，运行时不从 CDN 加载。PTY 二进制输出交给 xterm，其他远端文本按纯文本展示。

```bash
node --check app.js
node --check terminal-batch.js
node --test
```

窗口构建和功能边界见[运行指南](../docs/getting-started.md)与[模块文档](../docs/modules/frontend.md)。

## 运维展示

`management-view.js` 渲染五个只读管理页面；Host、地址与认证通过 Desktop 后台连接。
`management-view.test.mjs` 检查用户分组、通道筛选、MB/s、权限失败时清除数据与远端文本转义。
