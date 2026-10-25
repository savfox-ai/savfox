# Gateway Web 构建与发布

浏览器应用使用 Glory 语义 UI 与 Sapid 控件，源码位于
`crates/gateway-glory`。前后端共享模型继续位于 `crates/gateway-shared`；
认证、HTTP 与 WebSocket 协议仍由 gateway 负责。

## 依赖与存储位置

工作区使用相对根目录的 `../../glory-rs/glory` 与 `../../glory-rs/sapid`。
本地对应 `D:/Works/glory-rs/glory` 和 `D:/Works/glory-rs/sapid`。
CI 通过 `.github/actions/ui-sources` 检出固定版本，并恢复相同目录关系。

需要安装 Rust 的 `wasm32-unknown-unknown` target，以及与 `Cargo.lock` 中
`wasm-bindgen` 版本一致的 `wasm-bindgen-cli`。可通过 `WASM_BINDGEN` 环境变量
或 `-Bindgen <路径>` 指定 CLI，构建脚本会校验版本。

Windows 编译产物、Cargo 下载缓存、临时文件、指纹和前端 bundle 都使用
`D:/BuildCache`。WSL 必须使用已确认挂载到 D 盘的 `/mnt/d`。
脚本在编译前通过 `cargo metadata --no-deps --format-version 1` 核对实际 target
目录。远端 CI 使用 runner 存储。脚本不会启动 Docker。

## 本地流程

```powershell
just web-build                 # debug WASM 构建并同步 gateway 静态资源
just web-build-release         # release 构建并同步
just gateway                   # 构建前端后启动 gateway
just gateway-release
```

分离开发时，分别运行 `just gateway-frontend` 与 `just gateway-backend`。
前端开发服务器使用 Node.js，默认端口 18080；监听应用和 Glory/Sapid 源码，
重建 WASM 后自动刷新页面，并把 `/api`、`/health` 与 WebSocket `/ws` 代理到
18881。可用 `SAVFOX_DEV_BACKEND` 修改代理目标。
`just web-serve` 和 `just gateway-skip-web` 兼容入口继续保留。

## 构建边界

`scripts/build-web.ps1` 转交 Node.js 构建引擎 `scripts/build-web.cjs` 执行
Cargo 和 wasm-bindgen，复制 Sapid CSS 与应用资源，
将完整 bundle 同步到 `crates/gateway-server/static`。
中间产物位于 `D:/BuildCache/savfox-glory/web/<profile>`。
`-SkipCopy` 仅构建中间 bundle；`-Force` 强制构建与同步。
内容指纹覆盖应用、翻译、共享模型、锁文件、Glory、Sapid、样式和构建工具。
构建失败时不会替换 gateway 已有静态 bundle。

静态产物被 Git 忽略，禁止手工编辑。Gateway 编译时嵌入 bundle，应用路由
回退到 `index.html`。入口 HTML 使用绝对路径，支持刷新嵌套路由；无内容哈希
的资源使用 `no-cache`。

也可以直接运行 `node scripts/build-web.cjs`，选项为 `--release`、`--skip-copy`、
`--force` 和 `--bindgen <路径>`。

## 验证与部署

生成 bundle 后运行前端测试、WASM 构建，以及 gateway 中默认忽略的
`built_frontend_assets_are_served_with_spa_fallback` 测试。
浏览器验收需覆盖认证、导航、RPC、聊天流式输出与断线重连。

原生测试命令为 `cargo test -p savfox-gateway-glory -p savfox-gateway-shared`。
`node scripts/check-web.cjs` 在 Chromium 中加载真实 bundle，连接确定性的 gateway
测试数据，验证认证、深链、流式会话、审批与问题事件、登录取消、渠道编辑、
刷新与移动端。可用 `SAVFOX_PLAYWRIGHT` 指定 Playwright 安装目录；默认使用
Glory 的浏览器测试目录。CI 独立执行此验收。
`node scripts/check-web-proxy.cjs` 还会验证真实开发服务器的 HTTP/WebSocket 代理
与静态 bundle 路径。

Docker 构建需要显式提供命名上下文：

```powershell
docker buildx build --build-context glory=D:/Works/glory-rs/glory --build-context sapid=D:/Works/glory-rs/sapid -t savfox .
```

本地执行前必须核实 builder 的实际数据存储位于 D 盘或远端；现有 Docker
Desktop 的 C 盘虚拟磁盘不满足要求。CI 在远端 runner 上构建并传入相同上下文。
