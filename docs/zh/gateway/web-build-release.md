# Gateway Web 构建与发布

浏览器应用使用 Glory 语义 UI 与 Sapid 控件，源码位于
`crates/gateway-glory`。前后端共享模型继续位于 `crates/gateway-shared`；
认证、HTTP 与 WebSocket 协议仍由 gateway 负责。

## 依赖与存储位置

工作区通过 Git 依赖使用 `https://github.com/glory-rs/glory.git` 与
`https://github.com/sapid-ui/sapid.git` 的 `main` 分支。
`Cargo.lock` 固定实际提交，使本地、CI 和 Docker 使用相同版本；共享兄弟目录不再
是构建输入。Sapid 当前仍以兄弟路径声明 Glory，工作区通过针对 Sapid Git 来源的
patch 将它统一到同一份 Glory Git 依赖，无本地路径回退。
升级上游版本时运行 `cargo update -p glory -p sapid`，检查锁文件差异，再验证原生
测试、WASM 和浏览器行为。`.github/actions/ui-sources` 检查锁文件并记录双方提交。

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
前端开发服务器使用 Node.js，默认端口 18080；监听应用源码、依赖清单与锁文件，
重建 WASM 后自动刷新页面，并把 `/api`、`/health` 与 WebSocket `/ws` 代理到
18881。可用 `SAVFOX_DEV_BACKEND` 修改代理目标。
`just web-serve` 和 `just gateway-skip-web` 兼容入口继续保留。

## 构建边界

`scripts/build-web.ps1` 转交 Node.js 构建引擎 `scripts/build-web.cjs` 执行
带 `--locked` 的 Cargo 和 wasm-bindgen，从 `cargo metadata` 实际解析的 Git
依赖中复制对应 Sapid CSS 与应用资源，
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

选项控件统一使用带应用主题的 Sapid 下拉列表，使触发按钮和展开的选项保持同一
配色。浏览器验收必须实际展开菜单，检查对比度与视口边界，并验证鼠标和键盘
切换用量时间范围后发送的 RPC 参数。


视觉验收还需连接已运行的真实 gateway：`node scripts/check-web-visual.cjs`。
脚本读取本地 gateway token，不输出密钥，只导航和读取页面。可通过
`SAVFOX_VISUAL_BASE`、`SAVFOX_VISUAL_TOKEN_FILE`、`SAVFOX_VISUAL_REFERENCE`
指定地址、token 文件和旧版截图目录。截图与 HTML 对照报告输出到
`D:/BuildCache/savfox-glory/visual-review`。检查覆盖全部 18 个导航页面的桌面/手机、
深浅主题、文字对比度、图标、表单裁切、会话列表独立滚动、输入框边界及不刷新
切换窗口尺寸。除了断言，还必须人工对照迁移前 Dioxus 的真实截图。页面保留原来的
56px 顶栏、260px 导航栏和桌面 320px 会话列表；手机输入框必须位于底部导航上方。

Docker 使用同一锁定 Git 依赖，并从 Cargo 包复制 Sapid CSS，无需额外源码上下文：

```powershell
docker buildx build -t savfox .
```

本地执行前必须核实 builder 的实际数据存储位于 D 盘或远端；现有 Docker
Desktop 的 C 盘虚拟磁盘不满足要求。CI 在远端 runner 上构建。


可组合 `SAVFOX_VISUAL_ROUTES=sessions` 与
`SAVFOX_VISUAL_VIEWPORTS=1440x900,1120x900,800x560,390x560` 验证中间聊天栏
宽度和矮窗口，检查输入区控件裁切、重叠及搜索框聚焦前后的边框对齐。
