# Savfox 修复与复审记录

日期：2026-11-12。基于 [初始多维代码评审](2026-11-12-code-review.md)，在 `7745d2761f98adfe8efdea70a690837079161c2a` 的工作区上实施修改，未创建提交或发布产物。

## 修复范围

主线程和三个 agents 分别实现发布、安全、存储与会话、执行与连接生命周期修复，随后交叉评审其他实现者的代码。下表区分已经实施的修复与仍需后续设计的性能边界。

| 初始编号 | 实现 | 关键位置 |
| --- | --- | --- |
| S1 | 隐式凭据仅允许可信 OpenAI HTTPS 发现入口；自定义地址不再得到本机 token | `ws_rpc/handlers/model.rs` |
| S2 | WebChat 创建 session 强制 gateway 认证和 Chat scope；widget 通过可信后端预创建 session | `webchat.rs` |
| S3 | 账户文件入口统一验证 ID，拒绝路径穿越、Windows 设备名等；持久 provider/config 操作统一要求 Admin | `model.rs`、`security/auth/auth.rs` |
| L1 | 创建、重置、删除、持久化和辅助文件清理使用每逻辑会话锁；旧 turn 不得复活删除会话；prune 按旧 core generation 清理 | `channel/session_bridge.rs`、会话 HTTP/RPC、渠道 runtime、`lib.rs` |
| L2 | 取消传入执行监督器；终止并等待进程树；Windows 使用异步、有期限的树终止；取消和超时分别报告 | `core/exec.rs`、`tasks/user_shell.rs`、`utils/pty/process_group.rs` |
| L3 | 连接跟踪审批和后台任务，EOF 清 callbacks、取消并等待执行、关闭 core session/MCP/OAuth 资源 | `app-server`、`core`、`login-oauth` |
| C1 | stdout/stderr 同一 future 并行读取，保留排空超时前已读内容并显示不完整提示 | `core/exec.rs`、`core/tools/mod.rs` |
| E1 | 改善峰值内存与异步调度：blocking pool 双遍流式解析，只保留尾部及当前 JSONL 记录 | `runtime/session/history.rs` |
| E2 | JSON 一次事务导入 SQLite，正常更新仅写单行；跨实例更新使用最新记录和版本条件；持久化失败不发布成功缓存 | `runtime/session/store.rs` |
| E3 | 删除无消费者的 mtime 字段和 TTL 刷新重复扫描 | `runtime/session/store.rs` |
| D1 | 一起更新 workspace、内部依赖要求和锁文件；三种 bump 隔离验证；准备失败回滚；创建提交/tag 前 locked 检查 | `scripts/prepare-release.py`、`release.sh` |
| D2 | 主 npm 包只含 wrapper 和同版本可选平台依赖；统一平台矩阵；缺失 binary 拒绝打包；不再声明未发布的 Windows ARM64 | `nodejs/platforms.json`、打包与启动脚本、npm workflow |

E1 尚未实现索引分页：读取量仍是两次 O(文件大小) 扫描，不能将本轮改善描述为历史读取成本与返回条数成正比。单条极大 JSONL 记录仍会影响峰值内存。

## 交叉复审发现与处理

- 安全 reviewer 发现持久 provider 修改可通过保留旧凭据并改 base_url 间接外传；模型配置写入改为 Admin 权限，并补权限矩阵测试。
- 存储 reviewer 之外的安全 reviewer 发现 retire 与持久删除之间可重新绑定；结束与持久修改改为同锁，旧 turn 完成时校验绑定和墓碑。
- SQLite 初版全表重写仍会覆盖同 home 另一个实例的新数据；改成基于数据库最新记录的字段事务和版本条件，prune 只删除符合旧版本的行，读失败保留旧快照。
- 严格版本条件进一步区分读取 revision 与本地 touch，并结合持久 nonce 防止删除重建后的 ABA 覆盖；迁移改为只更新输入条目，保留快照之后新增的会话。终端委托也携带 generation，不能通过逻辑 ID 相等例外绕过校验。
- 迟到 prune 可能关闭重建后的新 core；清理携带被删除项的旧 core ID，并把 ambient、idle、media 等辅助清理纳入同一生命周期边界。
- 执行 reviewer 之外的性能 reviewer 发现 Windows 同步 taskkill、app-server 独立 commandExec 和 OAuth 任务未纳管；补异步期限、合作取消及连接收尾。
- Windows process cleanup 错误可能提前跳过临时 ACE 撤销；错误上报移到权限回滚之后，并补不操作系统权限的顺序反例。
- 运行验证发现 Windows 两秒树清理期限不足，改为十秒，并使 shell 宽限及上层关闭确认分别为二十秒和三十秒；测试改用 gate 握手验证清理后的副作用。大型组合 async future 另触发默认线程栈溢出，生产 resolver 与启动阶段改用堆分配，并添加 future 体积上界断言，没有调大全局线程栈。
- 发布 reviewer 发现新引入的 `mapfile` 不兼容 macOS 系统 Bash 3.2；替换为兼容循环。新增从其他目录运行构建脚本的行为测试又发现原有 Darwin target 匹配错误，一并修复。

最终只读复核确认，上述具体反例及原安全漏洞在源码层面已闭环；这一结论不代替下方运行验证，也不覆盖未触及模块。

## 行为与运维变化

模型账号和 provider 的持久配置写入现在需要 Admin；原 OperatorWrite 客户端应改用具备所需权限的凭据。WebChat 不再支持匿名领取执行能力，不应把 gateway token 嵌入公开页面。详见 [凭据边界](../gateway/credential-boundaries.md)，英文文档同步提供。

会话元数据的权威存储改为 `gateway-session-store.sqlite`。首次成功初始化会事务导入原 JSON 并保留原文件；原 JSON 此后不是当前数据备份。备份使用 SQLite online backup，或停机复制数据库和相关 WAL/SHM 文件。降级到仅读 JSON 的版本需要先显式导出数据，不能删除数据库作为清缓存手段。详见 [执行可靠性](../gateway/execution-reliability.md)。

npm 安装需要保留可选依赖；目前支持 Linux x64/ARM64、macOS x64/ARM64、Windows x64。发布按平台包先、主包后的顺序进行。版本准备需要 Python 3.11+；脚本只在显式 `--push` 时推送。

## 验证记录

最终验证通过：**42 项 Node 测试、77 项定向 Rust 用例（15 个测试组），均无失败和跳过**。覆盖七个相关 crate 的全目标 Clippy（`-D warnings`）、编译检查、Nightly 格式及 CI 测试矩阵检查通过。

| Rust crate | 定向用例数 | 主要验证内容 |
| --- | --- | --- |
| savfox-utils | 2 | Windows 进程树终止、工具期限及异步调度 |
| savfox-windows-sandbox | 2 | capture 取消、清理错误仍执行权限回滚 |
| savfox-login-oauth | 1 | 过早取消释放监听器 |
| savfox-rmcp-client | 1 | OAuth waiter 丢弃后释放回调端口 |
| savfox-core | 17 | 执行、取消、输出与 MCP 启动关闭 |
| savfox-app-server | 7 | 审批回调、连接关闭、实际命令树回收 |
| savfox-gateway-server | 47 | 存储并发与迁移、历史兼容、安全、WebChat、生命周期 |

七 crate 测试程序使用同一 Cargo 选择集构建，按 Cargo 返回的真实 executable 路径及对应 package 工作目录运行 Rust harness；每个过滤组先验证存在测试，防止零用例假通过。未变化的测试二进制复用已通过结果，重新链接的 Gateway 二进制重新运行相关用例。resolver 的 single/concurrent 入口 future 小于 128 KiB，以及默认测试线程上的完整生命周期场景均通过。

MSVC 链接曾报告一次 LNK1102 内存不足；保持正常配置、使用 `-j 1` 串行重试成功，没有修改 linker、debug profile 或全局缓存路径。Cargo 仍提示第三方 `proc-macro-error2 2.0.1` 的未来兼容性维护项，未通过忽略新告警来通过严格检查。

运行明细见 [Rust 测试汇总](../../../target/review-runtime-summary.json)、[Node 测试日志](../../../target/review-node-tests.log)、[严格 Clippy 日志](../../../target/review-final-clippy-confirm.log)。这些文件位于正常 target 目录，属于本地验证产物。

本轮没有执行 `cargo test --workspace`、全 feature 矩阵、Linux/macOS 运行测试、完整浏览器工作流、真实负载基准或 Windows elevated sandbox 的完整部署集成；Unix 的 I/O 排空与孙进程取消专项回归需由对应平台 CI 执行。以上通过结果限定于所列检查和用例。

所有 Cargo 产物和验证日志均使用 metadata 返回的正常项目 target 目录。没有使用或创建 `D:/BuildCache`，没有重定向 Cargo home、target 或 TEMP/TMP，没有启动 Docker 构建。

构建脚本测试的早期隔离失误曾给 stable 工具链新增 musl target；已改用 mock，并确认无活动构建使用后移除该新增 target，恢复了工具链目标集合。

## 后续边界

- 历史索引分页、单条记录大小限制及真实负载基线仍需独立设计和测量。
- WebChat session token 是独立能力凭据；撤销签发它的 gateway token 不会自动撤销已签发 session。
- AccountId 校验阻断路径字符串越界，不替代针对本地符号链接或目录替换的文件系统隔离。
- 部分后台 routing upsert 仍采用非 Result API，持久化失败通过日志体现，不能宣称所有后台写失败均可返回调用方。
- TUI、移动端和所有平台的完整交互验证、发布签名、远程发布门禁及线上负载测试未包含在本轮实现中。
