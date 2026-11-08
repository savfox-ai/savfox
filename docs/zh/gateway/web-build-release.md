# Gateway Web 构建与发布

浏览器应用使用 Glory 语义 UI 与 Sapid 控件，源码位于
`crates/gateway-glory`。前后端共享模型继续位于 `crates/gateway-shared`；
认证、HTTP 与 WebSocket 协议仍由 gateway 负责。

## 依赖与存储位置

工作区使用 crates.io 发布的 Glory `0.7.0` 与 Sapid `0.7.1`。
`Cargo.toml` 要求这两个确切版本，`Cargo.lock` 固定 registry 来源与校验和，
使本地、CI 和 Docker 使用相同版本。共享兄弟目录不是构建输入，
这两个包不使用 Git 依赖或本地路径覆盖。
升级版本时更新工作区版本要求与 `scripts/ui-sources.cjs`，再运行
`cargo update -p glory -p sapid`，检查锁文件差异，并验证原生测试、WASM
和浏览器行为。`.github/actions/ui-sources` 检查锁文件并记录双方版本。

需要安装 Rust 的 `wasm32-unknown-unknown` target，以及与 `Cargo.lock` 中
`wasm-bindgen` 版本一致的 `wasm-bindgen-cli`。可通过 `WASM_BINDGEN` 环境变量
或 `-Bindgen <路径>` 指定 CLI，构建脚本会校验版本。

构建继承调用环境中的 Cargo 和临时目录设置，通过 cargo metadata 获取实际 target 目录。前端产物和指纹写入该目录下的 web-build 子目录。脚本不会启动 Docker。

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
带 `--locked` 的 Cargo 和 wasm-bindgen，从 `cargo metadata` 实际解析的 crates.io
依赖中复制对应 Sapid CSS 与应用资源，
将完整 bundle 同步到 `crates/gateway-server/static`。
中间产物位于 `<cargo-target>/web-build/web/<profile>`。
`-SkipCopy` 仅构建中间 bundle；`-Force` 强制构建与同步。
内容指纹覆盖应用、翻译、共享模型、锁文件、Glory、Sapid、样式和构建工具。
构建失败时不会替换 gateway 已有静态 bundle。

静态产物被 Git 忽略，禁止手工编辑。Gateway 编译时嵌入 bundle，应用路由
回退到 `index.html`。入口 HTML 使用绝对路径，支持刷新嵌套路由；无内容哈希
的资源使用 `no-cache`。

原生 release 与 npm 工作流会先构建一份锁定依赖的 Web bundle，再供所有平台复用。
各平台在编译二进制前将产物下载到 `static`，并运行
`node scripts/verify-web-bundle.cjs`。检查会拒绝缺失或空的入口、CSS、JavaScript
以及无效 WebAssembly，避免从空 `static` 目录生成缺少网页的发布二进制。

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

点击站内链接的文字或图标时，必须保留当前文档和 WebSocket；折叠导航栏和手机
菜单也应如此。浏览器验收还覆盖前进后退、键盘激活与 Ctrl+点击新标签页。
链接内部的装饰元素将鼠标事件交给链接本身，使导航标记正确处理普通和修饰键点击。

选项控件统一使用带应用主题的 Sapid 下拉列表，使触发按钮和展开的选项保持同一
配色。浏览器验收必须实际展开菜单，检查对比度与视口边界，并验证鼠标和键盘
切换用量时间范围后发送的 RPC 参数。

布尔字段使用保留默认样式的 Sapid Checkbox，启用和可见性开关使用 Sapid Switch。
组件内部的原生 input 保留键盘和标签点击行为；不要禁用 Sapid 样式后在页面 CSS
里重新绘制控件。通用控件尺寸与配色集中在 `glory-bridge.css`，页面样式只负责布局。

控件、导航折叠组、操作菜单、卡片、徽章、加载提示、空状态和代码块优先复用 Sapid
widget，共用辅助函数只适配应用状态与翻译。代码块超过 Sapid 的字节、行数或 token
限制时保留文本显示，超过 64 KiB 的复制请求直接调用浏览器剪贴板。
导航开关保留 Glory 按钮，因为 Sapid 0.7.1 的 Burger 标签不能响应语言切换。

密码框保留 Sapid 样式并提供可见字段标签；输入框填满所在行，显示密码按钮在输入框
右侧插槽内垂直居中。字段组底板、标题和帮助文字在深浅主题下均使用应用配色。
操作区默认横排并在窄屏换行；配置导航预留选中边框的宽度，切换分区时标签不应偏移。

共用按钮皮肤仅作用于启用样式的 Sapid 按钮和明确带 `.btn` 的操作按钮。
未启用样式的自定义控件保持自身外观；文本按钮统一使用 `.btn-plain`，避免逐页
覆盖背景与阴影。标题、标签和提供商名称按原文显示，不通过 CSS 转换大小写。
按钮和链接共用键盘焦点轮廓线，输入框的聚焦阴影只作用于输入控件，避免泛化到
所有可聚焦元素。


视觉验收还需连接已运行的真实 gateway：`node scripts/check-web-visual.cjs`。
脚本读取本地 gateway token，不输出密钥，只导航和读取页面。可通过
`SAVFOX_VISUAL_BASE`、`SAVFOX_VISUAL_TOKEN_FILE`、`SAVFOX_VISUAL_REFERENCE`
指定地址、token 文件和旧版截图目录。截图与 HTML 对照报告输出到
`artifacts/web-visual`。检查覆盖 25 个路由，包含新建页面及服务商、
渠道配置表单，验证桌面/手机、深浅主题、标签与帮助文字对比度、图标、表单裁切、
密码框与按钮对齐、会话列表独立滚动、输入框边界及不刷新切换窗口尺寸。长页面还会
检查滚动至中部与底部的显示；依赖真实记录但当前没有数据的检查会在报告中标记为跳过。
除了断言，还必须人工对照迁移前 Dioxus 的真实截图。页面保留原来的
56px 顶栏、260px 导航栏和桌面 320px 会话列表；手机输入框必须位于底部导航上方。

Docker 使用同一锁定 crates.io 依赖，并从 Cargo 包复制 Sapid CSS，无需额外源码上下文：

```powershell
docker buildx build -t savfox .
```

Docker 使用当前引擎的存储配置，前端构建不重定向 Docker 存储。CI 在远端 runner 上构建。


可组合 `SAVFOX_VISUAL_ROUTES=sessions` 与
`SAVFOX_VISUAL_VIEWPORTS=1440x900,1120x900,800x560,390x560` 验证中间聊天栏
宽度和矮窗口，检查输入区控件裁切、重叠及搜索框聚焦前后的边框对齐。
