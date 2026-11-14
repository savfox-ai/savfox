# Savfox 多维代码评审与改进报告

日期：2026-11-12。代码基线：`7745d2761f98adfe8efdea70a690837079161c2a`。评审开始时工作区干净。

后续修复与验证结果见 [修复与复审记录](2026-11-12-remediation-review.md)。以下保留初始评审结论，代码行号对应初始基线。

本轮由主评审及三个独立 agents 完成，覆盖安全边界、性能与资源效率、正确性与生命周期、架构与可维护性、测试、交付与供应链、可观测性、跨平台兼容性和运行成本。重点抽查 gateway、core、app-server 和发布脚本；48 个 workspace packages、1251 个受 Git 跟踪的 Rust 文件构成评审范围背景，并不表示逐文件审计。TUI、前端和移动端未做完整交互或平台专项验证。

最先应处理的是凭据与目标地址绑定、匿名入口的能力隔离、文件路径约束，以及取消和断连后的资源释放。已有质量检查较完整，但这些跨机制边界仍存在缺口。本轮整理 **12 项具体问题：7 项 P1，5 项 P2**，另列长期改进与待验证风险。未发现足以认定为无条件紧急事件的 P0；这不构成安全保证。

## 评审方法与验证范围

三个 agents 分别独立追踪安全入口与权限、性能与并发、正确性与架构。主评审检查 CI 和发布链路、复核关键源码，并整合重复问题。结论以当前源码为依据，不依据旧文档推断实现。

- **静态确认**：入口、调用路径及危险操作或资源保留关系可从代码确认；仍需运行时回归验证平台行为。
- **隔离实验**：实际运行打包脚本，但使用假二进制和临时文件，不发布产物。
- **待验证风险**：存在可疑机制，但尚不足以确认为独立缺陷，不计入 12 项。
- **P1**：优先修复的安全、资源生命周期或发布阻断问题；严重性取决于各项列出的前置条件。
- **P2**：明确的正确性、效率或维护问题，按负载和使用频率排期。

没有使用真实凭据验证外传，没有访问攻击者端点、执行越界删除、触发真实匿名 agent 工作或启动 Docker 构建。没有运行全 workspace 编译、Clippy、Rust 测试、浏览器验收和负载测试，也没有重新查询在线漏洞数据库。性能复杂度为代码推导，未声称实测加速比或实际故障率。

## 需要保留的现有机制

项目已具备集中式 HTTP 权限声明、RPC scope 校验、SSRF DNS pinning、原子写文件、workspace lints、严格依赖审计、UI registry 来源校验、配置 schema 新鲜度检查和多类测试矩阵。WebSocket 已有 1 MiB 消息上限、256 条 outbound 队列和每连接 32 个并发 RPC 限制；渠道协调器也有接收、执行和同会话并发上限。

这些措施应保留。改进重点是把权限、凭据、目的地址、逻辑会话、core 会话、进程和异步任务的生命周期衔接起来，而不是简单增加更多零散检查。

## 问题总览

| 编号 | 优先级 | 主维度 | 问题 | 证据级别 |
| --- | --- | --- | --- | --- |
| S1 | P1 | 安全与凭据 | 只读模型发现可向自定义公网地址发送服务端 token | 静态确认 |
| S2 | P1 | 访问控制与运行成本 | 默认注册匿名 WebChat 并使用全局 agent 配置 | 静态确认 |
| S3 | P1 | 文件系统边界 | 删除模型账号时 account_id 可越出模型目录 | 静态确认 |
| L1 | P1 | 资源生命周期 | new、reset、prune 未闭环释放旧 core session | 静态确认 |
| L2 | P1 | 取消与进程监督 | user-shell 中断绕过进程组清理 | 静态确认 |
| L3 | P1 | 连接可靠性 | 未响应审批可让 app-server 在 stdin EOF 后无法退出 | 静态确认 |
| D1 | P1 | 发布正确性 | release.sh 更新版本后未同步锁文件及内部版本约束 | 静态确认 |
| E1 | P2 | 历史读取效率 | history limit 在整文件读取和全量解析后才应用 | 静态确认 |
| E2 | P2 | 写放大与并发 | 单条元数据更新全库克隆、序列化、重写并串行等待 | 静态确认 |
| E3 | P2 | 锁与文件系统效率 | TTL 刷新在写锁内重复扫描未被使用的 mtime | 静态确认 |
| C1 | P2 | 输出正确性 | 排空超时会丢弃已读取输出，且没有截断标识 | 静态确认 |
| D2 | P2 | 分发效率与平台契约 | npm 主包捆绑全部平台，ARM64 Windows 生成空平台包 | 隔离实验及静态确认 |

## 安全问题

### S1 只读模型发现接口可以外传服务端凭据

**位置**：[model.rs:301](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:301)、[model.rs:340](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:340)、[model.rs:399](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:399)、[model.rs:775](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:775)、[auth.rs:501](D:/Works/savfox-ai/savfox/crates/gateway-server/src/security/auth/auth.rs:501)。

`models.list` 按后缀分类只要求 Read，Viewer 也具有该权限。调用者可以提供 `provider="openai"` 和自定义 `base_url`；省略 `api_key` 时，fallback 从服务端 AuthManager 取得 OpenAI 或 ChatGPT token，随后通过 bearer header 发送。目的地址是否属于该凭据的授权服务未被校验。URL 判断目前仅决定是否重写 ChatGPT endpoint。

**前置条件及影响**：调用者持有 Viewer/OperatorRead token；gateway 存有可用凭据；出站策略允许访问调用者控制的公网服务。响应不需要是合法模型列表，解析响应前凭据已发送。现有 SSRF 私网限制、DNS pinning 和关闭重定向不能阻止向允许访问的公网域名外传凭据。

**修复**：将隐式凭据绑定至规范化的可信 origin；自定义 endpoint 要求显式凭据或管理员预先授权。将本地只读列表和会携带服务端凭据的远程发现操作分开授权，不能仅通过提高 scope 替代 origin 约束。

**验收**：假 token 加 mock transport 验证不可信 origin 永不收到服务端 Authorization/account header；可信 endpoint 保持可用。覆盖端口、userinfo、尾点域名、大小写和重定向变体。

### S2 默认匿名 WebChat 继承全局 agent 能力

**位置**：[server.rs:587](D:/Works/savfox-ai/savfox/crates/gateway-server/src/server.rs:587)、[webchat.rs:184](D:/Works/savfox-ai/savfox/crates/gateway-server/src/webchat.rs:184)、[webchat.rs:331](D:/Works/savfox-ai/savfox/crates/gateway-server/src/webchat.rs:331)、[webchat.rs:375](D:/Works/savfox-ai/savfox/crates/gateway-server/src/webchat.rs:375)、[session_bridge.rs:294](D:/Works/savfox-ai/savfox/crates/gateway-server/src/channel/session_bridge.rs:294)。

路由无条件注册；创建 session 只拒绝“提供了但无效”的 gateway token，未提供时允许创建并领取 session token。消息随后进入 `invoke_agent_text(..., "default")`，继承全局配置并使用 unattended 模式。现有限速按 session 计数，而 session 可匿名新建。

**前置条件及影响**：gateway 对不可信网络可达时，设置主 API bearer token 并不能限制这条独立入口。匿名用户可消耗模型额度并启动 agent 工作；文件和命令能力取决于全局 sandbox、tool、approval 配置，不能直接认定为任意代码执行。公共 widget 显然是代码预期功能，问题在于默认开放且缺少独立能力与预算边界。

**修复**：公共 WebChat 默认关闭；显式启用时指定受限 agent、文件系统和网络策略、工具白名单及模型预算。创建入口和消息入口共享来源级、身份级和全局配额，并限制总 session 数与执行并发。

**验收**：默认配置不能匿名取得 agent 执行能力；显式启用后继承公共 agent 策略；不断换 session 不能绕过同来源预算；预算耗尽和取消能够终止工作。

### S3 模型账号删除缺少路径边界校验

**位置**：[model.rs:1034](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:1034)、[model.rs:1053](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:1053)、[model.rs:92](D:/Works/savfox-ai/savfox/crates/gateway-server/src/ws_rpc/handlers/model.rs:92)。

`models.deleteAccount` 只要求 Write；`account_id` 仅 trim 和非空校验，随后直接拼接为 `models_dir.join(format!("{account_id}.json"))` 并删除。前面的 provider 文件读取或解析失败会返回默认对象，不会阻止删除。

**前置条件及影响**：OperatorWrite 调用者传入 `../auth`，即可定位到模型目录上层的 `auth.json`；更多父目录组件或绝对路径可影响其他进程有权限删除的 JSON 文件。范围受 `.json` 后缀和 OS 文件权限限制。示例路径已进行只读归约验证，未执行删除。

**修复**：引入统一的受约束 AccountId，所有读写删除入口共用；拒绝分隔符、父目录、盘符、UNC 和绝对路径。对凭据账号的修改权限重新核对，并处理模型目录的符号链接或 reparse point 边界。

**验收**：在临时 home 外放置哨兵 JSON；穿越、绝对路径、Windows 盘符和 UNC 输入均失败且哨兵不变，合法账号仍可删除。

## 生命周期与正确性问题

### L1 逻辑会话清理没有释放旧 core 会话

**位置**：[runtime.rs:236](D:/Works/savfox-ai/savfox/crates/gateway-server/src/channels/runtime.rs:236)、[session_bridge.rs:1058](D:/Works/savfox-ai/savfox/crates/gateway-server/src/channel/session_bridge.rs:1058)、[session_bridge.rs:945](D:/Works/savfox-ai/savfox/crates/gateway-server/src/channel/session_bridge.rs:945)、[session_manager.rs:423](D:/Works/savfox-ai/savfox/crates/core/src/session_manager.rs:423)、[lib.rs:266](D:/Works/savfox-ai/savfox/crates/gateway-server/src/lib.rs:266)。

`/new` 和 reset 清除元数据里的 core ID，并解绑逻辑映射，却没有 shutdown 或从 core manager 移除旧会话。新消息再创建一个 `cleanup_after_turn=false` 的会话。core manager 用 HashMap 强引用保存会话，定期 prune 只清 metadata 及 ambient/idle 状态，不能解除该引用。

**影响**：反复 new/reset 后，旧会话保留量随创建次数增长；SessionStore 默认 500 条上限不能约束 core manager。每个会话还可能持有 writer、工具或 MCP 资源，具体额外资源取决于配置。

**修复与验收**：统一结束逻辑会话的接口，在旧 ID 丢失前执行 shutdown、移除 core manager、清审批并解绑，供 new/reset/delete/prune 共用。用 fake core 连续执行 100 次 new/reset，验证 manager 数量回到稳定上界，shutdown 恰好执行一次；补充活动 turn 不被 idle eviction 驱逐的检查。

### L2 user-shell 取消绕过进程组清理

**位置**：[user_shell.rs:101](D:/Works/savfox-ai/savfox/crates/core/src/tasks/user_shell.rs:101)、[user_shell.rs:116](D:/Works/savfox-ai/savfox/crates/core/src/tasks/user_shell.rs:116)、[async-utils/lib.rs:121](D:/Works/savfox-ai/savfox/crates/async-utils/src/lib.rs:121)、[exec.rs:707](D:/Works/savfox-ai/savfox/crates/core/src/exec.rs:707)、[exec.rs:723](D:/Works/savfox-ai/savfox/crates/core/src/exec.rs:723)、[spawn.rs:132](D:/Works/savfox-ai/savfox/crates/core/src/spawn.rs:132)。

user-shell 通过外层 `or_cancel` 取消执行 future，执行层仅得到一个长超时。丢弃 future 不会经过执行层 expiration/OS ctrl-c 分支的进程组清理。`kill_on_drop` 处理直接 child，不能替代完整进程树监督；独立 spawn 的 stdout/stderr reader 的 JoinHandle 被丢弃也不自动 abort。上层额外终止的是 unified exec manager，而该路径未注册到此 manager。

**触发及影响**：命令产生长期运行、继承管道的子孙进程后收到应用层 Interrupt，界面已中断但后代仍可能执行、写文件或持有管道。各 OS 具体表现需要进程树回归验证。

**修复与验收**：把取消传入执行层，由监督器统一 kill/wait 进程组并收拢 reader；Windows 使用对应的进程树生命周期机制。记录父子孙 PID 后中断，检查整个未主动脱离的进程树退出、延迟写入不发生、终止事件后没有输出 delta。现有 [user_shell_cmd.rs:86](D:/Works/savfox-ai/savfox/crates/core/tests/suite/user_shell_cmd.rs:86) 侧重 TurnAborted 事件，需补物理资源释放断言。

### L3 未响应审批让 app-server 在客户端 EOF 后自保持

**位置**：[bespoke_event_handling.rs:163](D:/Works/savfox-ai/savfox/crates/app-server/src/bespoke_event_handling.rs:163)、[bespoke_event_handling.rs:1406](D:/Works/savfox-ai/savfox/crates/app-server/src/bespoke_event_handling.rs:1406)、[outgoing_message.rs:43](D:/Works/savfox-ai/savfox/crates/app-server/src/outgoing_message.rs:43)、[lib.rs:349](D:/Works/savfox-ai/savfox/crates/app-server/src/lib.rs:349)、[lib.rs:372](D:/Works/savfox-ai/savfox/crates/app-server/src/lib.rs:372)。

审批任务持有 outgoing 的 Arc，并等待 oneshot receiver；对应 sender 又保存在该对象的 callback map。没有客户端响应或统一取消时，这条所有权链不能自行释放。stdin EOF 后普通处理结束，但审批任务仍持有 outgoing 的 mpsc sender，stdout writer 等待通道关闭，而主任务 join 等待 writer 结束。

**触发及影响**：客户端收到审批后关闭 stdin，但保留 stdout 读取端。app-server 可能无法按预期退出；取消未答复审批也可能累积 callbacks 和任务。该判断来自生命周期静态追踪，尚未做真实客户端断连复现。

**修复与验收**：连接结束时关闭 outgoing、清 callback map、取消并 await 所有请求任务；审批绑定 turn/session cancellation，并通过 RAII 注销 pending request。补“待审批时关闭 stdin、持续 drain stdout、进程限时退出”和“反复中断审批后 pending 数回归基线”的测试。不要用过短审批超时替代连接生命周期管理。

### C1 排空超时丢失已读取的执行输出

**位置**：[exec.rs:748](D:/Works/savfox-ai/savfox/crates/core/src/exec.rs:748)。

命令先输出内容后退出，但后台后代继续持有管道时，reader 在 EOF 前不会返回已采集 buffer。两秒排空超时会 abort reader，直接生成空 `Vec` 且 `truncated_after_lines=None`，丢失已读诊断内容。实时 UI 可能收到过 delta，最终工具结果和持久化上下文却为空。

**修复与验收**：保留排空时限；将已采集 buffer 留在监督任务可提取的位置，超时返回部分结果并明确标识不完整，两个流使用统一 deadline 收拢。回归命令先写唯一标记，再让后台后代持有 stdout；应及时返回、保留标记并报告截断。现有 [tools.rs:404](D:/Works/savfox-ai/savfox/crates/core/tests/suite/tools.rs:404) 的不挂起检查需要增加输出保全断言。

## 性能与资源效率问题

### E1 历史 limit 未限制读取与解析成本

**位置**：[history.rs:70](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/history.rs:70)、[recorder.rs:382](D:/Works/savfox-ai/savfox/crates/core/src/rollout/recorder.rs:382)。

history 先读取整个 rollout，逐行解析全部 events，再提取所有消息及 provenance，最后才取 tail。即使只请求最近 20 条，成本仍随完整文件字节 B、事件 E 增长，至少 O(B+E)；并发请求各自保留原文及派生结构。JSON 同步循环还占用 async worker 时间。

**修复**：短期用流式读取与有界 deque 降低峰值内存，并把大解析移到 blocking pool；要降低读取量，需要偏移索引、反向扫描或持久化消息分页索引。仅改 BufReader 不会改变全文件扫描复杂度。

**验收**：构造不同大小、最后 20 条相同的 rollout，统计读取字节、峰值分配和并发尾延迟；覆盖 compaction、工具事件、过滤和总数语义。

### E2 单条 SessionStore 更新引起全库写放大

**位置**：[store.rs:629](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:629)、[store.rs:639](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:639)、[store.rs:935](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:935)、[tracking.rs:201](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/tracking.rs:201)。

常规 token usage 等字段更新会取得全局 mutation 锁，克隆全部 entries、pretty JSON 序列化并原子重写，等待持久化后才释放 mutation 锁。N 个 session、平均体积 S、更新率 R 时，复制序列化与写盘工作量约 O(R×N×S)。默认 500 条限制降低规模，但较长 provenance 和多渠道并发仍会放大成本。

**修复**：先合并同一轮内的多次元数据变更，再按 entry 分片或迁移到事务化存储。允许延迟的统计可经有界 writer 合并；要求 durable ACK 的 UI 操作必须保留持久化成功语义。

**验收**：N=10/100/500、provenance=0/1024 的矩阵记录更新写入字节、分配量、锁等待时间；磁盘失败注入验证不会成功应答后静默丢失关键修改。

### E3 TTL 刷新在写锁内扫描未使用的 mtime

**位置**：[store.rs:656](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:656)、[store.rs:680](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:680)、[store.rs:800](D:/Works/savfox-ai/savfox/crates/gateway-server/src/runtime/session/store.rs:800)。

`load_from_disk` 后再取得 cache 写锁，并 await 完整目录的 `get_latest_mtime` 扫描。`file_modified_at` 仅定义和赋值，没有读取；缓存有效性只检查 TTL。每次过期增加 O(N) 文件 metadata IO，并在此期间阻止 cache 读者。

**修复与验收**：删除未使用字段与重复扫描；如确需 mtime 失效，放在写锁外并复用首次扫描。用文件系统调用计数验证每次刷新不再二次遍历，并验证慢 IO 时 reader 不因快照交换而长期阻塞。

## 发布与分发问题

### D1 发布脚本没有同步锁文件和内部依赖版本

**位置**：[release.sh:35](D:/Works/savfox-ai/savfox/scripts/release.sh:35)、[release.sh:61](D:/Works/savfox-ai/savfox/scripts/release.sh:61)、[Cargo.toml:30](D:/Works/savfox-ai/savfox/Cargo.toml:30)、[release.yml:121](D:/Works/savfox-ai/savfox/.github/workflows/release.yml:121)。

`release.sh` 修改 workspace package version 后只提交 Cargo.toml 与 CHANGELOG，未更新 Cargo.lock 中 workspace package 版本。后续发布工作流使用 `cargo build --locked`，不允许锁文件更新，因此直接通过该脚本生成的版本提交不能满足发布构建前提。minor/major bump 还会让显式声明的 `savfox-*` 内部依赖版本与新的 path package 版本不兼容。

**修复与验收**：结构化更新 workspace version 和内部依赖版本，生成并提交锁文件，在打 tag 前验证版本一致性与 locked 构建。发布脚本先检查工作区状态和必要前提，再进行 commit/tag。用隔离仓库分别测试 patch/minor/major；验证 tag、workspace、lockfile 与二进制版本一致。此项未通过执行真实发布脚本复现，以避免创建提交或 tag。

### D2 npm 平台分发合同与产物校验不一致

**位置**：[package-npm.sh:22](D:/Works/savfox-ai/savfox/nodejs/scripts/package-npm.sh:22)、[package-npm.sh:27](D:/Works/savfox-ai/savfox/nodejs/scripts/package-npm.sh:27)、[package-npm.sh:34](D:/Works/savfox-ai/savfox/nodejs/scripts/package-npm.sh:34)、[package-npm.sh:68](D:/Works/savfox-ai/savfox/nodejs/scripts/package-npm.sh:68)、[publish-npm.yml:179](D:/Works/savfox-ai/savfox/.github/workflows/publish-npm.yml:179)、[savfox.js:15](D:/Works/savfox-ai/savfox/nodejs/bin/savfox.js:15)。

隔离实验用 5 个假平台产物运行实际打包脚本：主包包含全部 5 个平台目录，主 package.json 没有 optionalDependencies；平台包又各复制相同二进制。普通主包安装依赖 vendor fallback 可以工作，但必须下载全部平台内容，未获得平台包方案应有的按需安装效率。

脚本另声明 Windows ARM64 并生成其包目录，即使缺少对应 binary 仍成功；发布工作流只构建和发布 Windows x64，而启动器宣称支持 ARM64。该平台用户无法得到对应 binary。此项没有声称空 ARM64 包当前会被该工作流发布。

**修复与验收**：建立单一平台矩阵，主包使用锁定同版本的 optionalDependencies，平台包先发布并验证，再发布主包；按产品决策实际构建 ARM64 Windows 或明确移除其支持声明。必需 binary 不存在时打包必须失败。用 npm pack 清单和各平台安装 smoke test 检查产物、可执行权限及启动器版本；度量主包压缩体积，禁止再次夹带全部平台。

## 后续改进路线

以下为工作拆分建议，工时是初步人日估计，需由实际实现者结合兼容性调整，不是承诺排期。

| 阶段 | 工作包 | 建议责任域 | 粗估 | 完成标准 |
| --- | --- | --- | --- | --- |
| 第一阶段 | S1 凭据 origin 绑定、S3 AccountId 约束 | Gateway 安全 | 2–4 人日 | 假凭据与哨兵文件反例测试通过 |
| 第一阶段 | S2 公共入口显式开关与独立策略 | Gateway 产品及安全 | 2–4 人日 | 默认关闭，启用后权限与预算隔离 |
| 第一阶段 | D1 版本、锁文件和 tag 一致性 | 发布工具 | 1–2 人日 | 三种 bump 隔离回归，locked 构建通过 |
| 第二阶段 | L1 统一逻辑会话结束接口 | Gateway 与 Core | 2–4 人日 | 多次 reset/prune 后 manager 和资源回归基线 |
| 第二阶段 | L2 执行取消监督、C1 输出保全 | Core 跨平台执行 | 3–6 人日 | 进程树退出，管道收拢，部分输出保留 |
| 第二阶段 | L3 连接及审批任务生命周期 | App-server | 2–4 人日 | EOF 限时退出，pending callback 不累积 |
| 第三阶段 | E3 删除重复扫描、D2 精简分发 | Gateway 存储及发布 | 1–3 人日 | TTL 仅一次遍历，主包按平台安装 |
| 第三阶段 | E1 分页索引、E2 增量持久化 | Gateway 与存储 | 5–10 人日 | 数据规模矩阵证明 IO 与锁开销下降，故障语义不退化 |

### 架构与代码质量

将连接、turn、逻辑 session 的结束动作表达为明确状态转换：停止接收新工作、取消子任务、终止进程或请求、等待退出、释放映射和持久化状态。每种资源指定唯一所有者。审批 callback、执行 reader、core session 不应依赖“最后一个 Arc 自然释放”隐式完成清理。

将 AccountId、可信 provider origin 和公共 agent policy 建成受约束类型，放在边界构造处校验；业务函数接收已验证的值。HTTP/RPC 权限最好由同一份方法元数据生成，减少按字符串后缀猜测语义带来的差异；保留现有 method 名称和兼容别名。

### 测试与质量门禁

优先增加本报告的失效反例，断言物理资源和数据结果，而不只断言“已发送取消事件”或“接口成功返回”。安全修复测试不使用真实 provider token；生命周期测试使用 fake provider、受控子进程和临时 home。

当前 PR CI 已覆盖较多 targeted crate、all-feature Clippy、schema、UI 浏览器和严格 advisory 检查。发布和 Docker tag 工作流自身没有依赖测试及 audit 的验证 job；发布流程应要求同一 commit 的质量结果成功，并增加打包产物 smoke test。仓库外的分支保护、tag 保护和发布环境审批本轮未读取，因此这里列为门禁改进，未单列为已发生的发布绕过缺陷。

可进一步将 Windows/macOS 高风险取消、keyring 和路径反例保留为 PR 小矩阵，完整跨平台检查继续定期运行。新增安全或状态边界时同步相关英文与中文产品文档，避免默认开关、权限和持久化语义只存在于实现中。

### 可观测性与性能基线

建立低基数指标：活跃及保留 core session 数、pending approvals、活跃执行进程和 reader tasks、队列深度、mutation 锁等待、历史读取字节、metadata 写入字节、取消至资源释放耗时、公共 agent token 和费用预算。不使用任意 session ID 或路径作为指标 label，日志不得包含凭据。

复用现有 [bench-rpc.sh](D:/Works/savfox-ai/savfox/scripts/bench-rpc.sh) 和 [e2e_gateway.rs:868](D:/Works/savfox-ai/savfox/crates/gateway-server/tests/e2e_gateway.rs:868) 的 p50/p95/p99 输出，增加长历史、慢盘、500 会话和持续 new/reset 场景。先保存可重复基线，再设容差门槛；当前材料不足以给出可信的绝对延迟预算或收益百分比。

### 供应链与运行成本

保留 Cargo.lock、UI registry 校验和严格 advisory audit。进一步缩小发布 job token 权限，评估把第三方 Actions 固定至审核过的 commit；将目前占位的二进制签名升级为真实签名或可验证来源证明，并明确消费者如何验证。以上为加固建议，本轮未对依赖和 Actions 声称存在新漏洞。

运行成本首先从公共入口总预算、session 生命周期、全库写放大和多平台 npm 包下载量入手；不要在缺少 baseline 时优先引入全局缓存或改变 Cargo target 路径。

## 待验证风险

- 渠道流式输出存在无界队列，消费者可能等待远端消息编辑。需要结合输出上限、网络超时和并发量压测队列增长，再决定是否引入有界合并队列；本轮未确认实际内存失控。
- 默认限流、会话隔离与取消语义在多个渠道和入口之间是否完全一致，需要跨入口契约测试。S2 已证明的匿名入口问题不代表其他入口也存在相同缺陷。
- TUI 可访问性、Sapid 控件复用、前端性能、移动端与 sandbox 的完整平台覆盖未完成，不据此评定好坏；建议分别建立后续专项评审。

## 本轮执行记录

| 检查 | 结果与解释 |
| --- | --- |
| `cargo metadata --no-deps --format-version 1` | 成功；48 packages；实际 target 为 `D:\Works\savfox-ai\savfox\target` |
| 6 个 Node 测试文件 | 32 项通过，0 失败、0 跳过；包含 UI 来源、安装校验、bundle、CI matrix、浏览器响应大小和 Cargo target 解析 |
| `node scripts/check-ci-test-matrix.cjs` | 通过；验证 targeted packages、可选 Gateway features、all-feature Clippy 和严格 advisory job 的配置映射 |
| `cargo fmt --all -- --check` | exit 0，但稳定版忽略 nightly 专用配置，不能作为完整格式验收 |
| `cargo +nightly fmt --all -- --check` | exit 0，无输出；完整格式检查通过 |
| npm 打包隔离实验 | 实际脚本产出全部 5 平台 vendor、无主包 optionalDependencies、无 binary 的 ARM64 Windows 包目录；临时目录已清理 |
| Rust 单元及集成测试、Clippy、负载、浏览器、Docker、在线漏洞数据库 | 本轮未执行，不将配置存在等同于运行通过 |

Node 测试命令：

```powershell
node --test scripts/ui-sources.test.cjs scripts/install.test.cjs scripts/verify-web-bundle.test.cjs scripts/check-ci-test-matrix.test.cjs scripts/gateway-browser-response.test.cjs scripts/cargo-target.test.cjs
```

本轮仅新增此评审文档，没有修改业务代码、依赖、构建配置或产品文档。未提交 Git commit、创建 PR 或发布任何产物。所有建议均为后续改进任务，尚未实现。
