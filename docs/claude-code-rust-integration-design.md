# Savfox Claude Code 纯 Rust 接入设计与审核报告

状态：已按用户授权实施纯 Rust 第一阶段并完成离线验收；新模式为实验性，真实 Claude CLI 验收尚未完成。

版本：v3.1，补充实施、自审修正、提交前复核与验证记录；日期：2026年10月11日，Asia/Shanghai。

源码基线：Savfox `f64173d6c087e47379515462bbdf5df85862733d`；hagency-rs `1212833a7bfe3b17f6f1d69fc8d528aca6ac0f36`。本文描述这些本地源码的行为，官方文档链接用于解释接口能力，不表示任意 Claude Code 版本都已通过兼容测试。

## 建议与范围

采用 Rust 直接启动本机 Claude Code，通过双向 `stream-json` 驱动其 agent 循环。借鉴 hagency-rs 的协议、审批和进程所有权设计，针对 Savfox 的连续聊天增加会话恢复、容错解析、取消路由和结构化用量。无需 TypeScript 或 Python 桥接运行时；Claude Code 本身仍是外部依赖，模型请求由它发送。

接入 Gateway 的 terminal agent 分支。Anthropic API provider 保持现有职责，由 Savfox 管理工具和 agent 循环；Claude Code adapter 使用厂商的工具和循环。第一阶段交付完整的聊天调用、恢复、基础审批、取消、错误与退出处理、前端配置和文档。跨机器恢复、进程预热池、长期授权和完整操作系统沙箱分别规划。

用户已在本会话授权“根据报告实施改进”。下文保留 v2 的设计与实施前基线；文末记录实际实现及差异。当前配置与能力以使用文档、代码和验收记录为准。

## 实施前基线与复用边界

| 位置 | 已有行为 | 接入时的工作 |
| --- | --- | --- |
| [terminal_agent.rs](../crates/gateway-server/src/terminal_agent.rs) | Claude preset 是 `claude -p {{prompt}}`，默认 `one_shot`、普通文本；管理 cwd、输出上限和子进程 | 增加专用 preset 与事件能力；保留旧 preset 的显式调用方式 |
| [agent_terminal_delegate.rs](../crates/gateway-server/src/agent_terminal_delegate.rs) | 拼接系统指令、历史和附件；处理工作树、日志和 rollout；目前结果的 `last_token_usage` 固定为 `None` | 分发 Claude adapter；传递运行上下文和用量；避免恢复时重复注入历史 |
| [codex_app_server.rs](../crates/gateway-server/src/codex_app_server.rs) | 已有双向 JSON-RPC、会话 ID 持久化和同会话串行锁；审批目前直接拒绝 | 参考会话锁、stderr 排空和事件转换；协议状态机独立实现 |
| [approval_coordinator.rs](../crates/gateway-server/src/security/approval_coordinator.rs) | 关联 nonce、Channel 范围、Core session 和审批 ID；决策通过 `session.submit(Op::...)` 回传 Core | 增加厂商执行目标，复用鉴权和通知；不能将 Claude 控制 ID 冒充 Core approval ID |
| [chat_session.rs](../crates/gateway-server/src/chat_session.rs) | 停止候选指向 Core session，提交 `Op::Interrupt` | 注册 Claude 活跃执行并增加精确取消路由 |
| [WS session handler](../crates/gateway-server/src/ws_rpc/handlers/session.rs) | terminal 分支直接等待调用，使用无界事件通道并广播给所有连接 | 接入生命周期、审批与用量；有界传输、受权事件投递；停止和审批可在执行中到达 |
| [gateway shared agents](../crates/gateway-shared/src/agents.rs) | 有 runtime、mode、workspace、审批桥接声明；字符串枚举允许 `Other` | 增加已知模式、字段及语义校验，不能依赖未知枚举值被接受 |
| [Dioxus agents page](../crates/gateway-dioxus/src/pages/agents.rs) | Claude 选项目前生成 `one_shot` 配置 | 新增专用模式及迁移选项，显示真实运行能力 |

Channel 也通过 [session_bridge.rs](../crates/gateway-server/src/channel/session_bridge.rs) 调用 terminal delegate。该调用目前没有流式 sink，不能把“没有 sink”解释为允许执行或存在可用的审批回路。

## 对 hagency-rs 方案的取舍

| 方面 | hagency-rs 当前方案 | Savfox 建议 |
| --- | --- | --- |
| 调用 | 双向流式协议，初始化后发送任务 | 沿用，独立封装控制消息 |
| 会话 | 每个 dispatch 新会话，关闭持久化；当前 factory 没有预热子进程 | 默认按聊天恢复；独立任务采用 `per_turn` |
| 兼容 | 未知事件拒绝，部分响应检查精确形状 | 权限与控制严格；未知非关键事件允许跳过；新增非关键字段兼容 |
| 审批 | owner 回路，保留原始输入；控制表上限 16，已完成条目仍占用 | 待处理、完成去重、审计历史分别限额 |
| 设置 | 固定 setting sources、MCP 和规则；启动时额外启用文件 guard hook | 区分用户设置与受控设置，记录实际能力 |
| 生命周期 | guardian 托管与进程树停止证据 | 保留明确的执行所有者；按平台实现和验证退出能力 |
| 隔离 | 文件 guard 覆盖 Write、Edit、MultiEdit、NotebookEdit；平台模块明确不是沙箱 | 不把 hook、工作树或审批桥接描述为文件与网络隔离 |

参考：[hagency Claude 参数](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-runtime/src/claude.rs)、[任务参数](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-runtime/src/claude/task_mcp.rs)、[审批控制](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-runtime/src/claude/session/control.rs)、[当前 factory](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-execution/src/factory.rs)、[文件 guard](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-runtime/src/claude/write_guard.rs)。

## 接入结构

```text
Gateway WS RPC 或 Channel
          |
          v
terminal delegate 分发
          |
          v
Claude 执行上下文与生命周期
    |             |              |
    v             v              v
协议读写       审批协调器      会话绑定与诊断
    |
    v
Claude Code 子进程
```

建议代码放在 `crates/gateway-server/src/claude_code/`。按职责划分协议读写、会话绑定、审批适配与运行所有者；不要把 Claude 协议混入通用文本 parser，也不要为了此次接入重构整个 Core。

每个调用保留唯一 `run_id`，运行上下文携带 agent、逻辑聊天、认证主体或 Channel 范围、实际 cwd、设置策略和可选事件 sink。执行注册表持有取消入口；执行任务始终持有子进程所有者。浏览器断线与用户停止是不同事件，不得因关闭页面就无条件中断任务。

运行所有权属于 Gateway 而非 WS 请求 future。请求被丢弃时，执行继续到明确结果或经过有界清理后结束；不能遗留只有 PID、没有所有者的进程。Gateway 关闭时停止接收新调用，取消活跃执行并等待清理。等待同一聊天锁的请求也必须可取消，不能在用户停止后排队启动。

## 配置与兼容迁移

增加 `terminal.mode = "claude_stream"`，仅允许与 `runtime = "claude"` 配合。旧 `one_shot`、`managed_pty`、`interactive_launch` 保留，已有配置不自动改写。新 agent 默认采用新模式的前提是目标 CLI 已通过能力检查；用户可显式选择旧模式，但执行失败不能静默退回旧模式或重发任务。

adapter 拥有 `--print`、输入输出格式、`--verbose`、部分消息输出、权限回传和恢复参数。模型、MCP、setting sources、permission mode 等由结构化配置生成。自定义 args 中与 adapter 冲突的参数必须拒绝，而不是依赖命令行参数先后顺序决定行为。

`io_protocol` 和 `stdin` 在此模式下由 adapter 管理，冲突配置返回参数错误。建议默认 `savfox_approval_bridge = required`；启动前检查当前入口有受权客户端能接收并回答请求，无回路时拒绝启动。`prompt` 允许明确的非交互拒绝策略，启动前展示该能力；`disabled` 仅保留旧执行模式的语义，在 `claude_stream` 中拒绝配置，不能让无终端 CLI 自己等待输入。`per_session` 恢复同一聊天，`per_turn` 创建独立会话；第一版拒绝 `per_agent` 和 `manual`，不允许跨聊天共享 Claude 上下文。

增加 adapter 专用可选模型配置：明确设置时传真实 Claude 模型名，未设置时使用 CLI 默认并从初始化信息记录实际模型。不能直接把 `effective_model` 的全局 provider slug、`terminal/<agent>` 或 agent ID 传给 `--model`。无效模型在可能范围内启动前拒绝，CLI 返回的模型错误与传输错误分别展示。

workspace 清理与恢复共同校验：需要恢复的 `per_session` 拒绝 `per_turn`、`delete_on_success`、`delete_always` 等会改变或删除运行目录的组合；独立 `per_turn` 不保留可恢复绑定。新建会话和停止执行是不同操作，均绑定准确的 agent、聊天和认证范围。

## 会话恢复

默认采用每轮新进程加 Claude 持久会话恢复，不做常驻进程池。恢复使用显式 Claude session ID，不使用“最近一个会话”，不扫描其他用户会话，也不以 Savfox UUID 冒充 Claude ID。官方 CLI/SDK 支持按 ID 恢复，实际参数和行为须在目标版本验证。[官方会话说明](https://code.claude.com/docs/en/agent-sdk/sessions)

绑定数据至少记录 agent、Savfox session、Claude session、认证范围、cwd、设置与权限策略指纹、CLI 版本和最近运行状态。metadata 通过临时文件替换等方式原子写入；权限限制遵循现有 Savfox 会话文件约定。

同一绑定的锁覆盖参数生成、恢复、执行、状态保存和 workspace cleanup。prompt 发出前先持久化本次运行的身份及启动状态；目标 CLI 支持显式新 session ID 时，预先生成并保存，接到 CLI 身份后校验。否则在首个身份事件取得 Claude ID 后原子补全。保存失败后停止执行并报告状态不确定，不能重发已可能产生效果的请求。

第一轮使用 CLI 专用的初始系统指令参数，必要历史以明确的上下文发送；恢复轮只发送新请求与新附件。不能把 Savfox 系统指令仅包装为普通 user 文本就声称保持相同指令优先级。Channel 本轮的可信上下文单独传递；该上下文不进入用户持久授权，不能因恢复而沿用旧发送者或旧权限。

读取现有历史的 20 条上限不等于完整迁移旧 Claude 会话。旧 one-shot 切换新模式时明确创建新 Claude 会话，并告知采用了多少 Savfox 历史上下文。恢复已知 Claude transcript 与导入 Savfox 历史是不同流程。

cwd、认证身份或执行策略改变时要求明确新建会话，不能自动携带旧上下文和权限。模型或初始系统指令变化必须有明确的重建策略。绑定有效但 transcript 丢失时返回恢复失败，不静默新建并重放任务。

超时、断连和响应丢失不触发自动重发；文件写入或外部操作可能已经发生。允许用户检查保留状态后恢复或新建，界面说明恢复不是撤销，也不保证此前操作只执行了一次。

调用入口用认证范围和客户端 request ID 关联唯一 run；重复提交先查询既有执行，不再次发送 prompt。相同 request ID 携带不同输入时拒绝。Gateway 重启后仅凭日志不能判断外部操作是否成功，持久启动记录用于报告 unknown，不能转为自动重试授权。

## 协议与容量

初始化确认后发送用户任务。读取循环独立于审批等待；用单一 writer 序列化写入，关联控制响应与唯一请求 ID。所有读写、关闭和取消均有期限，避免等待审批时无法接收取消消息。

| 消息情况 | 行为 |
| --- | --- |
| 已知文本、工具和用量事件 | 映射为明确的 Savfox 事件 |
| 未知普通事件或新增非关键字段 | 在有效 envelope 内有限记录并忽略 |
| 未知控制请求 | 返回明确 unsupported，不授权，不伪造成功 |
| 权限请求身份或原始输入不完整 | 拒绝处理，结束或隔离当前执行 |
| 无关联控制响应或错会话消息 | 根据状态机拒绝，不改变另一执行的状态 |
| 非法 JSON、截断帧或超大帧 | 报协议错误并执行有界清理 |

分别限制单帧、累计回复、stderr 保存、事件队列、待审批数量和完成去重记录。已完成条目不占待审批容量，去重记录有有界保留策略。记录已完成请求时保留摘要，不长期保存原始敏感参数。

stderr 达到保存上限后继续排空。事件发送采用有界队列；拥塞时可合并文本增量或截断诊断日志，但审批、状态和最终结果必须有可靠交付或明确失败，不能无限积累内存。任何消息上限以字节而非字符数定义，保证 UTF-8 截断合法。

协议 reader 不等待慢浏览器消费。最终回复和运行状态保存在执行结果中，客户端按序号检测缺失并通过受权查询校准；审批信号走独立的有界控制通道。控制通道容量耗尽时明确拒绝新请求或结束当前执行，不把授权决定混入可丢弃日志队列。

## 消息与用量

按主会话和消息身份关联文本增量，完成消息用于校准结果而不是重复追加。子代理文本、工具日志和思考事件不得混入主回复；未知事件的完整原始 JSON 不广播给所有客户端。

用量同时保存 input、output、cache read、cache creation、可用的 model usage 和成本字段。缺失数据表示未知，不填零。消息级累计与最终汇总是不同来源，应定义去重和校准规则，不能简单相加。

需要把 adapter 的计量传到 `TerminalRunResult`、`AgentInvocationResult`、会话存储和 WS 最终结果，而不止新增一个 usage 事件。兼容现有 `TokenUsage` 时明确 input 和 cache 的口径；保留 vendor 原始分类用于诊断。

## 审批与用户输入

扩展现有协调器的执行目标，使 Core 继续走原有 `Op`，Claude 走当前执行的控制回传。审批绑定为认证范围、agent、逻辑 session、run ID、Claude session、control request ID 和原始工具输入。服务端生成单次 nonce；授权主体和请求范围都必须匹配。

在解析 CLI request 时保留原始参数并计算输入摘要，UI 只提交 request ID、nonce 和决定，不允许提交替代工具参数。审批消费必须原子化；先检查目标执行仍活跃，再记录决定并投递给该执行。记录决定、写入控制响应和收到后续观察分别记状态，协议未提供确认时不能宣称已证明工具执行。Gateway 重启保留历史审计，但不恢复已经失去进程的活跃审批。

第一版提供允许一次、拒绝和停止任务。允许回传内存中保留的原始输入；展示和审计使用脱敏摘要，摘要不能重建工具参数。重复、迟到、已取消或旧运行的决定不能作用于新进程。CLI 取消请求、超时、退出和 Gateway 重启使原审批失效。

拒绝工具不等于停止任务；Claude 应有机会调整做法。等待审批不阻塞协议读取和取消。没有可靠审批客户端时采用明确拒绝策略，不无限等待。用户作出决定与决定实际写入 CLI 是不同状态，传输结果未知时不能显示“操作已获准并执行”。

审批桥接只处理 CLI 送出的权限请求。`allowedTools` 是自动批准规则，不是工具可见性的白名单；自动批准的调用不会必然到达 `can_use_tool`。需要逐次控制时，应另行设计经过验证的 hook 和执行隔离。[官方权限说明](https://code.claude.com/docs/en/agent-sdk/permissions)

第一版为 `AskUserQuestion` 实现专用问答事件，校验原问题与选项，按目标版本格式更新原始输入；取消和超时返回明确拒绝。MCP elicitation 第一版显式返回不支持，并在 MCP 能力清单里说明。两者不能伪装成普通工具批准，也不能让功能依赖 CLI 在无终端环境里等待输入。

新增 Claude 内容、审批摘要与 nonce 不走 `broadcast_to_all`。接收者同时满足相应 scope 和执行所属范围；仅检查订阅或 Operator scope 不能证明属于该聊天。现有 [session manager](../crates/gateway-server/src/runtime/session/session.rs) 的 scope 过滤和 session 投递可以作为基础，但目标范围必须经过服务端鉴权。查询状态、日志、用量、取消和审批均使用同一授权边界。

## 完成与取消

区分一轮 `result`、后台工作结束、stdin 关闭、stdout EOF 和进程退出。官方 SDK 对部分后台任务和运行状态另有跟踪，因此 `result` 本身不能作为立即 kill 的理由。[官方 Query 实现](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py)

目标版本支持运行状态时按已验证事件收尾；不支持时使用明确的退出等待和后台工作限制。无期限后台 shell、monitor 与应等待的有限子任务分别处理。最终结果仅结算一次；结果文本和清理结果分开保留，不能用清理失败覆盖已发生的回复。

停止按 run ID 路由：发 interrupt，继续处理响应和日志，超过宽限后执行平台终止，再回收子进程和管道任务。`chat.abort` 在指定 Claude 聊天或运行时只能停止该目标；目标不存在时返回 not found 或 already ended，不能回退到停止全部 Core 会话。显式全局停止单独鉴权并定义范围。

分别配置初始化、活动执行、用户审批等待、interrupt 宽限和退出清理期限；活动期限可暂停计算审批等待，但另有总期限与审批期限保证有界。取消在等待锁、启动、读写、审批和收尾阶段均生效。interrupt 应答只是控制请求的结果，不代表已经停止；清理失败必须保留可查询状态和诊断，不能删除仍可能被进程使用的 workspace。

状态至少区分启动失败、执行完成、CLI 错误、协议错误、取消、超时、结果未知和清理失败。断线后可通过受权查询读取状态；不依赖浏览器连接持有进程。

Unix 进程组能改善后代清理，但不能承诺停止已脱离进程组的全部进程；Windows 的 Job Object 与 npm shim 启动需要独立设计。第一版发布平台按实测结果列出，编译成功不代表所有平台行为已验证。工作目录删除必须等待清理确认。

## 设置与认证

提供用户设置和受控设置两种策略。用户设置加载用户选择的 Claude 配置；受控设置显式限制 setting sources、MCP 和必要 hooks。关闭 settings 来源不能推导出所有项目记忆、skills、插件或远程 connector 都已关闭，每项能力按目标 CLI 的实际行为检测。

本地登录通过 Claude 自身读取，不复制认证文件，不从 Savfox Anthropic provider 隐式注入 key。显式 API key、自定义 endpoint、代理和配置目录可作为独立选项；日志和指纹不得包含 key/token 原文。记录认证配置身份，不承诺能发现同一个登录目录里的账号切换。

hagency-rs 对默认 `~/.claude` 不设置 `CLAUDE_CONFIG_DIR`，源码记录过显式设置它导致配置查找变化。Savfox 将其作为目标版本的回归案例，而不是跨版本通用保证。[本地环境处理](/Volumes/Data/Works/hagency-org/hagency-rs/native/hagency-execution/src/local_codex.rs)

版本检查使用显式可执行文件或 PATH 解析结果，记录版本和能力。未知版本可查询健康信息，但涉及审批控制等关键能力时须通过兼容检查才执行；不要无依据接受任意“未来版本”。兼容矩阵必须保存实测版本、协议 fixtures 来源和相关 SDK 源码 commit；文档版本号或 `--version` 成功本身不能证明控制协议可用。

记录启动时的文件身份或摘要用于判断运行期间升级，升级使新调用重新检查能力，但不终止正在运行的任务。受控运行可关闭 CLI 自动更新；不把每次升级强制改写固定哈希设为唯一工作流。Windows 原生 exe、npm shim 和路径空格的处理应复用经过验证的 argv 启动能力，不拼接可执行 shell 字符串。

多个 agent 使用同一登录配置时可能共享额度、限流和认证状态，独立 session 不等于独立账号。个人本地使用与向其他用户提供订阅能力分别判断；官方关于第三方提供 claude.ai 登录或额度的限制不能因自行实现 Rust 协议而忽略。[官方认证说明](https://code.claude.com/docs/en/agent-sdk/overview)

## 隔离能力

审批、工作树和 guardian 均不能替代文件和网络沙箱。第一阶段报告真实的运行边界；没有平台沙箱时不能启用或标注 `read_only`、`workspace_write` 的隔离保证。受控 settings 只控制配置来源，不限制进程对宿主机的全部访问。

完整沙箱另行设计并验证 Bash、解释器、符号链接、外部 MCP、网络、认证读取和后代进程。不要只给文件工具加路径 hook 就宣称覆盖所有越界写入。[官方部署隔离说明](https://code.claude.com/docs/en/agent-sdk/secure-deployment)

## 实施顺序与估算

先用目标 CLI 验证初始化、流式输出、权限回传、interrupt、resume 和后台结束语义，形成可重放的协议 fixtures，并固定测试版本。之后依次实现 adapter、会话与取消、审批和事件、前端与兼容迁移。新模式必须在完整验收后开放；不能将缺少审批或取消的阶段视为已完成接入。

| 第一阶段工作 | 自审后代码量估算 |
| --- | ---: |
| Rust 协议、会话与生命周期 | 1,000～1,700 行 |
| Gateway 审批、取消、授权和计量接入 | 500～900 行 |
| shared 类型、前端配置与基础审批展示 | 250～500 行 |
| 协议和集成测试 | 1,000～1,600 行 |
| 合计，不含文档 | 约 2,750～4,700 行 |

估算是新增或修改行数的数量级，不是工期承诺，不包含完整沙箱、跨机器会话、常驻池和永久授权。当前工具进程的 PATH 未发现 `claude`；这不等于用户机器未安装。真实协议与登录态验收仍是发布前提。

初稿估算未充分计入 Core 之外的精确取消、审批执行目标、事件授权、无界队列改造和 shared 字段变更，已在本版提高预算。平台后代进程管理如果需要新增独立 supervisor、Windows Job Object 封装或跨平台 helper，须根据平台调查另估，不能包含在上述数字中就假定已解决。

## 验收条件

| 验收项 | 必须证明的结果 |
| --- | --- |
| 恢复与串行 | 两轮真实请求共享正确会话；不重复灌入历史；相同聊天串行，不同范围不共享 |
| 指令变化 | 恢复不会吞掉新的可信 Channel 上下文；cwd、身份、策略和初始指令变化有明确行为 |
| 文本与用量 | 增量和完整消息不重复；子代理不污染主回复；缺失用量不填零，最终汇总不重复累计 |
| 审批 | 允许、拒绝、超时、取消、重复、迟到、跨运行、跨身份及错误 nonce 均有测试 |
| 审批期间取消 | 同一执行可停止；旧审批不能唤醒新运行；没有授权回路时不无限挂起 |
| 问答 | 问题和选项来自原始请求，回答格式有效；取消、超时和未支持的 elicitation 不挂起执行 |
| 精确停止 | 目标已结束、未知 run、等待锁和跨身份请求不会停止别的 Core 或 Claude 执行 |
| 事件授权 | 不同客户端和 Channel 不能读取其他范围的文本、nonce、日志和运行状态；订阅不能绕过授权 |
| 协议边界 | 新增展示事件可兼容；未知控制请求不授权；坏 JSON、EOF、超大帧有明确错误 |
| 输出拥塞 | 长 stdout/stderr 不死锁，队列内存有界，最终结果可校准 |
| 后台任务 | 有限子任务与长期任务分别处理；不能仅凭 `result` 提前结束 |
| 生命周期 | CLI 异常退出、超时、任务取消和 Gateway 关闭后回收已支持的平台进程与管道 |
| 失败恢复 | transcript 缺失、绑定不兼容、状态保存失败不自动重做已可能产生效果的任务 |
| 重复调用 | 相同 request ID 不发第二个 prompt；重启后的未知效果不自动重试 |
| 兼容 | 旧 one-shot、PTY 和 Codex app-server 流程保持原有显式模式 |
| 配置组合 | 未知 mode、冲突 args、模型 slug、跨聊天 scope、删除 workspace 的恢复组合在启动前拒绝 |
| 构建 | Gateway 定向测试、Clippy、fmt；shared 测试；canonical Web 构建及更新静态资源服务 |

fixtures 只证明解析和路由，真实 CLI 测试证明协议兼容，两者不能互相替代。涉及 Windows 或 Linux 发布时，对应平台必须有真实进程与取消证据。没有真实 CLI 的环境可以保存报告和做离线测试，不能宣称完整接入已通过。

## 待用户评审的默认选择

建议默认按聊天恢复；新模式使用受控设置；第一阶段只提供一次审批与明确拒绝；CLI 权限模式优先选择可预测的 `default` 并显式配置日常允许规则，不直接照搬 hagency 的 `auto`。未接入操作系统沙箱时，能力说明明确为原生执行。

实施采用按聊天恢复、受控 settings、一次审批和 CLI default 权限模式。受控 MCP 默认空，可显式配置；期限有界。完整 OS 沙箱与 Windows 支持不在此次实现中，真实平台发布仍按实测决定。

## 自审修订结果

| 审核发现 | 本版修订 | 实施前仍需验证 |
| --- | --- | --- |
| 指定聊天停止找不到 Core 时，现有 handler 会回退到全量 Core 停止 | Claude 目标必须精确匹配，不沿用该回退；取消覆盖排队阶段 | 多运行与无目标情况下的取消回归 |
| terminal 事件无界且广播全部 WS 连接 | 有界事件队列，按 scope 与认证范围投递 | 同时连接的异身份客户端和 Channel |
| 审批消费和 nonce 校验不能自动证明 owner 范围和 CLI 接收 | 增加执行目标及原始输入绑定，分别记录决定与投递 | 重复、迟到、响应写入中断及重启 |
| 持久会话可能与 workspace 删除策略冲突 | 启动前拒绝不兼容组合，同会话锁覆盖 cleanup | worktree 和 patch-only 恢复及取消清理 |
| 当前 terminal 调用的 model 不一定是 Claude 模型 | 专用模型配置，记录 CLI 实际模型 | CLI 默认、显式模型与无效模型 |
| 只新增 usage 事件无法更新现有最终用量 | 贯通 adapter、结果、会话与 WS，定义 cache 口径 | 消息去重及最终汇总校准 |
| 只看 result、EOF 或 interrupt 应答都可能过早宣告结束 | 分开运行结果、后台状态、退出及清理 | 目标版本的后台事件和后代进程 |
| 关闭设置来源不代表关闭所有记忆与扩展 | 设置、skills、hooks、MCP 与 connector 分项验证 | 受控配置下的真实能力清单 |
| 初始代码量低估外围接入 | 改为约 2,750～4,700 行，平台 supervisor 另估 | CLI 探针和平台调查后的分解 |

以上为 v2 的设计自审记录。v3 已新增代码、测试和使用文档；以下区分离线已验证行为与尚未认证的真实 CLI/平台能力。

## v3 实施与再次审核

已新增 [Claude Rust adapter](../crates/gateway-server/src/claude_code/mod.rs)，拆为协议状态机、进程驱动和运行/审批 registry。前端提供 Claude Stream (Rust) 配置、逐次审批、AskUserQuestion 回答和按聊天停止。用法与迁移见[中文说明](zh/gateway/claude-code.md)及[英文说明](en/gateway/claude-code.md)。没有引入 SDK 桥接进程，也没有切换 Anthropic API provider 的职责。

| 已实现项 | 实际行为与再次审核修正 |
| --- | --- |
| 控制协议 | 单写者 stdin；独立 stdout/stderr 排空；initialize 成功后持久记录 running 再发送 user。1 MiB 帧/回复上限、64 KiB stderr 上限；非 UTF-8 stderr 转换也受字节上限约束。未知展示事件限量记类型，未知控制返回 error。 |
| 连续会话 | 原子保存 Claude session ID 与绑定摘要；同 session 串行。恢复只发送本轮 prompt/附件，首轮最多导入 20 条历史；初始指令走 CLI 参数。重复请求不改写元数据、不重发 prompt、不重复写 rollout 或累计用量。 |
| 绑定与兼容 | 绑定 owner、agent、逻辑 session、cwd、配置/系统指令、显式及相关继承环境、可执行文件路径/长度/修改时间、精确版本。摘要包含环境身份但不保存 key 原文；版本变化要求新聊天，运行前验证参数和 initialize。 |
| 文本与计量 | 去重 partial/full，忽略子代理文本，以 final result 校准；保留厂商模型/cache/cost 分类。缺失 usage 不伪造 TokenUsage。错误保留已收到的完整或部分回复、stderr 和退出诊断。 |
| 审批及问答 | owner + run + control ID + nonce，原始 input 留内存，UI 替换参数无效。回答使用问题索引并映射原文，重复问题文本拒绝。持久消费成功才允许回传；过期、取消或已消费的审计记录不能授权。 |
| 凭证隔离 | 身份取实际 token 摘要，不依赖显示 label。Claude 事件/历史/运行/审批按 owner；历史别名解析后再次检查。跨身份批准和读取失败；同一凭证重连可查询并解决原审批。 |
| 取消与关闭 | 注册后再等 session 锁，可以取消尚未启动的执行。WS/REST 按准确目标停止，缺失目标不回退全量 Core。Gateway Ctrl-C/SIGTERM 请求取消；Unix interrupt 有界排空后终止组并回收管道。浏览器断开不终止执行。 |
| 输出与后台 | stdout 帧队列 16，WS 增量队列 64，审批最多 16/运行，活跃运行最多 64，内存 registry 总计 256。有限 local-agent/workflow 跟踪至后续 result 和 idle；最终回复补偿增量丢失。 |
| 配置失败处理 | agents 保存时校验；手工文件类型错误或未知 mode 也明确报错，不转走其他 runtime。拒绝冲突 args/stdin、模型 slug、跨聊天 scope、恢复时删除目录，以及 read_only/未知 workspace。 |
| 旧模式兼容 | one_shot、managed_pty、Codex app_server 保留显式路径；失败不自动退回旧 CLI 或 native provider。 |

### 与原设计的差异及保留边界

- 厂商运行和审批由独立 registry 持有，复用 durable approval store、nonce、scope 和 WS resolver。没有把 Claude control ID 写成 Core Op，也没有强行扩展只面向 Core session 的 coordinator。
- 总期限为执行期限加一次审批期限余量；每个审批另有 TTL。不是对每次等待暂停并无限累加，总期限始终有界。
- 当前只实现 Unix 进程组；Windows 新模式在 preflight 拒绝。脱离组的后代及 Gateway SIGKILL 后清理不受保证，preflight 借用旧 supervisor 的直接子进程清理能力。
- Channel 有认证 peer 和独立上下文刷新，但尚无 Claude Channel 审批回路；只可选择 prompt 并拒绝交互权限，required 明确拒绝没有回路的调用。当前调用接口没有稳定上游 event ID，因此每次调用创建新 request ID，不用内容摘要错误合并后续相同文本；尚不保证 Channel 投递重试的去重。
- 文件身份使用长度/mtime，不能证明同长度、同时间戳的二进制替换；同一登录目录里的账号切换无法检测。settings/工作树/patch-only 不保证宿主文件或网络隔离，read_only 在新模式拒绝。
- 同一凭证能查询原 request ID 和回答审批；页面不会自动接续断线期间的全部增量，使用最终历史/运行查询恢复状态。
- 审计声称 decision_recorded，不声称 CLI 工具已执行。重启后旧审批不恢复；未完成记录显示 unknown_after_restart，拒绝自动重做。
- registry 持久记录没有自动磁盘清理，日志沿用已有留存机制；长期使用需管理已结束记录，不能删除活跃会话。
- 此环境 PATH 未发现真实 claude，只交付实验性显式 opt-in。expected_version 必须填写完整输出，设置它或 initialize 成功不能证明真实版本已认证。未下载 CLI、借用认证或触发付费调用。

### 验证记录

提交前复核修正了以下问题，并补充或扩展了对应回归测试：

- 关闭期间的入场竞争：等待 registry 锁的调用取得锁后再次检查关闭状态，防止关闭后新任务进入。
- 同会话收尾竞争：会话锁随调用结果保留到 rollout、聊天元数据及最终事件完成，避免下一轮提前读到旧状态。
- 绑定持久化失败：收到最终回复后保存失败也按执行错误结算，保留回复与工作目录，不再报成功。
- 会话元数据隔离：会话列表的 ID、数量和 token 总量只统计可见记录；所有带会话目标的 `sessions.*` 调用执行 Claude owner 检查。
- 配置编辑往返：编辑表单从 typed `agents.get` 加载完整 Claude 设置，保留精确版本、MCP 及其他策略字段，避免再次保存丢失配置。

本轮最终检查结果如下，未运行整个 workspace 的全量测试。

| 命令/验证 | 结果 |
| --- | --- |
| `cargo test -p savfox-gateway-server --lib` | 442 passed，0 failed，1 ignored。包含 19 项 Claude adapter 测试及 1 项 WS 集成测试；被忽略的静态资源测试另行运行。 |
| `cargo test -p savfox-gateway-server` | 单元、集成及文档测试合计 504 passed，0 failed，26 ignored；其中 24 项既有端到端/生命周期测试需要外部运行环境，未强行解除忽略。静态资源测试另行运行，另有 1 项文档示例维持忽略。 |
| `cargo test -p savfox-gateway-shared -p savfox-gateway-dioxus` | shared 30 passed，Dioxus 68 passed，均无失败；包含 Claude 配置的 JSON 往返验证。 |
| `pwsh -NoProfile -File scripts/build-web.ps1` | 成功构建并同步前端 dist/gateway static。 |
| `cargo test -p savfox-gateway-server --lib static_assets::tests::built_frontend_assets_are_served_with_spa_fallback -- --ignored` | 1 passed；验证 SPA 路由、JS/WASM 资源及 MIME。 |
| `cargo fmt --all -- --check`、`git diff --check` | 通过。stable formatter 对既有 unstable 配置提示不影响检查成功。 |
| `cargo clippy -p savfox-gateway-server -p savfox-gateway-shared -p savfox-gateway-dioxus --all-targets` | 命令成功；新增/修改代码未产生 Clippy 警告。既有文件仍有警告。 |
| 同上增加 `-- -D warnings` | 未通过：被既有 `crates/memory/src/storage.rs:315`、`gateway-shared/src/arkret.rs`、`utils/src/pty/pty.rs:26` 等警告阻塞。未修改无关代码或降低 workspace lint 门槛。 |

WS 集成测试使用两枚显示名称相同的 Operator token，证明非 owner 不能批准、读历史（包括 UUID 大小写变体）或获得列表中的 Claude 会话 ID/统计；断线后原 token 能查询 pending 并回答；排队任务按 run ID 取消后，原任务的审批仍活跃；重复提交不重跑、不重复用量/rollout；返回结果持有期间同会话锁仍不可获取，释放结果后才可获取。手工文件类型错误和未知 mode 明确失败，未触发另一 runtime。

合成协议 peer 及源码来源见 [fixture README](../crates/gateway-server/tests/fixtures/claude/README.md)。它是测试进程，不是真实 CLI 录制；参考官方 Python SDK commit b6e9d12fe1cc98dde988ab7b7713c1feeee50c6c 的协议实现，运行时没有 Python SDK 依赖。真实登录态、实际 CLI 权限/问答/resume/后台结束，以及各发布平台清理仍须实测，不能用上述离线结果认证任意 CLI 版本。
