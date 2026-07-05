# 多 Agent 系统

Savfox 支持多 agent 架构。主 agent 可以把子任务分派给不同角色的子 agent，每个 agent 都有独立的线程、配置和上下文。

## Agent 角色

角色定义位于 `crates/core/src/agent/role.rs`。每个角色会通过 `AgentProfile` 覆盖部分默认配置。

| 角色 | 说明 |
|------|------|
| `default` | 继承父 agent 配置 |
| `explorer` | 面向代码库问答的快速 agent |
| `worker` | 执行实现任务的 agent |
| `orchestrator` | 仅协调的 agent（规划中） |

### AgentProfile 常见字段

| 字段 | 说明 |
|------|------|
| `base_instructions` | 覆盖系统提示词 |
| `model` | 覆盖模型 |
| `reasoning_effort` | 覆盖推理强度 |
| `read_only` | 是否强制只读 |
| `description` | 对外展示的角色描述 |

## Explorer

`explorer` 主要用于代码库问题探索：

- 使用更快的模型配置
- 通常采用中等推理强度
- 适合并行运行多个独立问题
- 更偏向“给出可信结论”，而不是直接实现改动

## Worker

`worker` 主要用于执行工作：

- 实现功能
- 修复缺陷
- 拆分大重构任务
- 通常需要明确文件所有权，减少并发冲突

## 子 Agent 启动流程

子 agent 由 `ThreadManager` 拉起。大致流程如下：

1. 父 agent 请求创建新线程
2. `AgentRole::apply_to_config()` 根据角色修改子线程配置
3. 子线程以独立上下文启动

常见覆盖包括：

- 替换模型
- 替换基础指令
- 调整推理强度
- 强制只读 sandbox

## 深度限制

为了避免无限递归创建 agent，系统使用 `MAX_THREAD_SPAWN_DEPTH` 控制最大派生深度。

## 生命周期状态

agent 状态通常体现为：

```text
Idle --> Thinking --> Executing --> Idle
                 \--> Error
```

这些状态可通过协议事件或 gateway 侧接口观察。

## 控制接口

每个 agent 线程都通过统一控制接口进行管理，包括：

- 启动/停止
- 中断当前任务
- 注入新输入
- 查询状态

## Terminal Agent Runtime

Gateway agent 也可以通过 `terminal_delegate` 把一次任务交给本地终端 CLI。
这不是临时兼容层，而是一等运行时路径，适合接入 Codex、Claude 或自定义本地
agent。它们通常有自己的登录态、配额提示、上下文管理、插件和交互式行为。

当前 terminal runtime 保留 one-shot 流程：Savfox 启动配置中的命令，把 prompt
渲染到参数或 stdin，捕获 stdout/stderr，把 user/assistant 交换写入 session
rollout，然后把捕获结果作为回复返回。`agent.terminal.launch` 仍然用于人工可控
的交互式场景，例如登录、使用 TUI、或直接接管复杂 agent 会话。

one-shot 路径现在统一经过 Terminal Supervisor。Supervisor 会校验解析后的 cwd、
启动进程、写入 stdin、限量读取 stdout/stderr、杀掉超时进程，并把 `spawn` 失败、
`invalid cwd`、`timeout`、非零退出码、输出读取失败归一到 terminal metadata 和
用户可读错误里。stdout 和 stderr 每路最多保留 1 MiB；被截断的输出会在日志和返回
metadata 中带上 truncation marker。

Terminal agent 执行的是本地原生命令。Savfox 能记录进程 metadata、日志、health
结果和 stream event，但命令运行后，vendor CLI 自己的审批、插件动作、文件写入和
网络请求不会再由 Savfox 逐项拦截。这个 runtime 应用于可信 CLI，并且要明确选择
cwd。下面的权限和 workspace 字段会把这个边界显式写进配置，但当前 one-shot
runtime 仍然启动的是 native terminal 进程。one-shot 后端现在已执行 `shared`、
`worktree` 和 `patch_only` workspace mode；`read_only` 当前会作为声明被接受，
但 runtime 会明确返回平台能力不足，直到后续 sandbox 切片接入。
workspace 隔离能降低多个 agent 误写同一 repo 的风险，但不能单独替代 native
terminal 的权限边界。

`terminal_delegate` 支持以下可选的前向兼容字段：

| 字段 | 说明 |
|------|------|
| `profile` | 终端 agent 类型，例如 `codex`、`claude`、`custom` |
| `mode` | 运行模式，例如 `one_shot`、`managed_pty`、`interactive_launch` |
| `session_scope` | 终端状态按 turn、session、agent 或 manual 作用域隔离 |
| `io_protocol` | 输出解释方式，例如 plain text、JSONL、profile parser、sentinel |
| `terminal_execution` | 命令权限边界声明，例如 `native_terminal`、`managed_workspace`、`disabled` |
| `savfox_approval_bridge` | 是否由 Savfox 桥接 vendor CLI 审批，例如 `disabled`、`prompt`、`required` |
| `health_check_command` / `health_check_args` | 仅供 `agent.terminal.health` 使用的可选检查命令/参数 |
| `workspace` | workspace 声明块，包含 `mode`、`base`、`cleanup_policy` |

这些字段省略时，runtime 会按 `profile = "custom"`、`mode = "one_shot"`、
`session_scope = "per_session"`、`io_protocol = "plain_text"`、
`terminal_execution = "native_terminal"`、`savfox_approval_bridge = "disabled"`、
`workspace.mode = "shared"` 处理。`workspace.base` 可省略，默认使用解析后的
terminal cwd；`workspace.cleanup_policy` 默认是 `per_session`。只包含
`enabled`、`command`、`args`、`stdin`、`cwd`、`env`、`timeout_secs` 的旧配置
无需迁移即可继续运行。shared wire type 会保留未知字符串值，方便新 runtime 增加
策略时旧 UI 仍能 roundtrip。

`workspace` 配置块：

| 字段 | 取值 / 说明 |
|------|-------------|
| `mode` | `shared` 直接使用配置 cwd；`worktree` 为每个 terminal session 创建 detached git worktree；`patch_only` 在隔离 worktree 中运行，并在 `log_dir` 写入 `workspace.patch` 和 `workspace-diff-summary.txt`；`read_only` 当前会明确返回能力不足错误 |
| `base` | 可选 base path 或模板，用于创建/解析 managed workspace |
| `cleanup_policy` | `per_session`/`manual` 保留隔离 workspace；`per_turn`/`delete_on_success` 在成功后删除；`delete_always` 失败后也删除 |

`worktree` 和 `patch_only` 的恢复入口是 `metadata.json`：先查看
`workspace_path`、`workspace_cleanup_status`、`workspace_patch_path` 和
`workspace_diff_summary_path`。如果 cleanup 失败，可以在记录的 `workspace_base`
git root 下手动执行 `git worktree remove --force <workspace_path>`。如果
`patch_only` 生成了 patch，先审查 `workspace-diff-summary.txt`，再把
`workspace.patch` 应用回主工作树。

`savfox_approval_bridge = "disabled"` 表示 vendor CLI 保持自己的审批提示，Savfox 不会
逐项检查 CLI 内部的工具、文件或网络动作。`prompt` 和 `required` 是为后续 bridge
实现预留的声明值；在 runtime 明确报告 bridge 支持前，不要把它们当作沙箱保障。
同样，`terminal_execution` 在这个兼容切片里是策略声明；如果 runtime 还没有实现
策略强制，禁用 terminal delegation 仍应使用 `enabled = false`。

每次 terminal 调用都会在
`{savfox_home}/terminal-agents/<agent_id>/sessions/<session_id>/` 下创建独立目录，
包含 `home`、`workspace`、`logs` 和 `metadata.json`。命令模板可以使用：

| 模板 | 值 |
|------|----|
| `{{session_id}}` | 当前 Savfox session id |
| `{{agent_home}}` | terminal agent 的隔离 home/config 目录 |
| `{{workspace_dir}}` | 为 terminal runtime 预留的 session 工作目录 |
| `{{log_dir}}` | stdout/stderr 日志目录 |
| `{{conversation_context}}` | 当前 Savfox session 中最近的 user/assistant turns |
| `{{attachment_manifest}}` | 本轮附件清单，包含文件名、MIME、大小和本地路径 |
| `{{terminal_input_json}}` | 结构化输入包 JSON，包含 session、agent、当前请求、历史和附件 |

`{{prompt}}` 现在渲染为完整 terminal 输入包：system prompt、session 标识、最近
对话、当前用户请求、附件 manifest 和结构化 JSON 都会包含进去。需要裸用户文本时
使用 `{{user_prompt}}`。Sessions UI 传入的图片附件会写入
`logs/attachments/`，并通过 attachment manifest 把本地文件路径暴露给 Codex、
Claude 或自定义 CLI。当前不会把大 base64 图片直接塞进命令参数。

`metadata.json` 会记录 profile、mode、session scope、I/O protocol、解析后的路径、
命令、cwd、workspace mode/base/path、cleanup status、patch path、diff summary path、
进程 id、状态、开始/完成时间、退出码、错误，以及成功持久化后的 Savfox rollout
路径。传入 terminal runtime 的 session id 必须是 UUID v7。

通过 WS-RPC session 调用 terminal agent 时，它会沿用现有 agent stream topic，
并带上 terminal 专用标记：`started`、`delta`、`log`、`status`、`message`、
`completed`、`error`。one-shot 进程运行期间，stdout/stderr 会被边读边广播：
stdout delta 用作 assistant text stream，stderr delta 作为 terminal log。complete
payload 仍会携带解析后的 terminal event 列表，便于客户端构建更细的 timeline。
Sessions UI 会使用实时 stdout delta 更新当前 terminal-agent 回复，并在 complete 时
用最终解析结果收敛。

输出解析方面，`io_protocol = "plain_text"` 会把 stdout 当作回复、stderr 当作日志。
`io_protocol = "jsonl"` 支持按行 JSON 事件，`event`/`type`/`kind` 可为
`message`、`status`、`log`、`error`、`completed`；未知 JSONL 行会保留为原始 stdout
日志。`io_protocol = "sentinel"` 支持 `::savfox-message ...`、
`::savfox-status ...`、`::savfox-error ...`、`::savfox-complete` 这类标记。
`io_protocol = "profile"` 当前会回落到 plain text parser，直到具体 profile
提供更细的 parser。

运维或用户可以调用 `agent.terminal.health` 检查配置的 CLI 是否可用、版本命令是否
能运行，以及当前 cwd 和 terminal runtime 根目录是否可用。agent 配置可以通过
`health_check_command` 和 `health_check_args` 覆盖 health probe；显式 RPC 参数仍
优先。`agent.terminal.metrics` 会返回 spawn count、duration、timeout count、
exit reason 总数等运行指标。`agent.terminal.cleanup` 可以清理
`{savfox_home}/terminal-agents` 下的 terminal session 目录，并支持 `dry_run`
预览。Agents UI 在交互式启动按钮旁提供了 Health Check 入口。创建/编辑 UI 也
提供 Savfox 普通模型 agent、Codex、Claude、自定义 CLI 四类一级模板。

Managed PTY 已通过公开 WS-RPC 提供长驻终端进程能力：`agent.terminal.pty.start`、
`write`、`read`、`resize`、`close`、`list` 和 `close_idle`。start 可以显式传入
command，也可以解析 agent 配置中的 `terminal_delegate.interactive_command` /
`command`；write 支持 text、line、newline、interrupt、control sequence 和
manual complete；read 可以按 sequence 增量读取 transcript，也可以带 timeout 等待
指定文本。会启动/写入/关闭本地进程的 PTY 方法需要 Admin scope，`read` 和 `list`
只需要 Read scope。

Codex one-shot terminal agent 示例：

```json
{
  "terminal_delegate": {
    "enabled": true,
    "profile": "codex",
    "mode": "one_shot",
    "session_scope": "per_session",
    "io_protocol": "plain_text",
    "terminal_execution": "native_terminal",
    "savfox_approval_bridge": "disabled",
    "workspace": {
      "mode": "shared",
      "cleanup_policy": "per_session"
    },
    "command": "codex",
    "args": ["exec", "{{prompt}}"],
    "interactive_command": "codex"
  }
}
```

Claude one-shot terminal agent 示例：

```json
{
  "terminal_delegate": {
    "enabled": true,
    "profile": "claude",
    "mode": "one_shot",
    "session_scope": "per_session",
    "io_protocol": "plain_text",
    "terminal_execution": "native_terminal",
    "savfox_approval_bridge": "disabled",
    "workspace": {
      "mode": "shared",
      "cleanup_policy": "per_session"
    },
    "command": "claude",
    "args": ["-p", "{{prompt}}"],
    "interactive_command": "claude"
  }
}
```

这些目录为 one-shot 和 managed PTY 终端 session 提供明确的上下文边界，同时保留
当前 one-shot 命令行为。

### Managed PTY 平台状态

当前 Managed PTY 是 gateway 管理的 session registry 加 process-backed 后端，已支持
公开 WS-RPC start/write/read/resize/close、transcript 读取、resize/kill trait 调用、
idle/explicit close，以及 fake REPL 测试中的 sentinel/manual complete。它还不是完整
的原生 terminal emulator。

| 平台 | 当前后端 | 原生 PTY hook | 状态 |
|---|---|---|---|
| Windows | process-backed stdio 后端 | 预留 ConPTY hook | WS-RPC 可用；非原生 PTY |
| macOS | process-backed stdio 后端 | 预留 Unix pty hook | WS-RPC 可用；非原生 PTY |
| Linux | process-backed stdio 后端 | 预留 Unix pty hook | WS-RPC 可用；非原生 PTY |

在 runtime 明确报告原生 PTY backend 和 approval bridge 支持前，客户端不应假设
Codex 或 Claude 的完整交互协议已经可用。

## 与 Gateway Agent 配置的区别

这里描述的是 **运行时子 agent / 线程角色模型**。  
它与 gateway 里的“可配置 agent 实例”是相关但不同的两层：

- 运行时角色：强调线程职责和派生行为
- gateway agent 配置：强调模型、prompt、trigger、渠道策略等长期配置
