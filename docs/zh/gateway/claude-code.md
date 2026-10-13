# 纯 Rust 调用本地 Claude Code

实验性 `claude_stream` 模式通过双向 JSONL 驱动本机 Claude Code。Savfox 持有进程、桥接审批和问答，并按明确的 Claude session ID 恢复聊天；无需 Python 或 TypeScript 桥接。模型请求、登录态和工具由 Claude CLI 管理。旧 `one_shot`、`managed_pty` 和 Codex `app_server` 保留显式模式，失败后不会自动切换模式或重发任务。

## 配置

先通过本机 Claude CLI 登录，并检查 `claude --version`。Agents 页面选择 Claude、Claude Stream (Rust)，在 Claude settings 的 `expected_version` 中填写**完整版本输出**，并验证该安装。缺失或版本变化会拒绝执行。版本匹配、参数检查和初始化成功不能代替真实调用验收。

`<savfox_home>/agents/claude-local.json` 示例：

```json
{
  "id": "claude-local",
  "name": "Local Claude",
  "kind": "terminal",
  "system_prompt": "遵循仓库说明，并解释所做修改。",
  "terminal": {
    "runtime": "claude",
    "profile": "claude",
    "enabled": true,
    "command": "claude",
    "mode": "claude_stream",
    "session_scope": "per_session",
    "savfox_approval_bridge": "required",
    "terminal_execution": "native_terminal",
    "timeout_secs": 300,
    "workspace": { "mode": "shared", "cleanup_policy": "per_session" },
    "claude": {
      "expected_version": "替换为完整的 claude --version 输出",
      "model": null,
      "settings_policy": "controlled",
      "allowed_tools": [],
      "initialize_timeout_secs": 20,
      "approval_timeout_secs": 120,
      "shutdown_timeout_secs": 5
    }
  }
}
```

`model` 留空使用 CLI 默认模型；不会隐式传入 Savfox 的 provider/model slug。需要选模型时填写 Claude 模型名。`args` 必须为空，`stdin` 必须缺省，协议参数由适配器生成；`io_protocol` 只能缺省或为 `jsonl`。

`controlled` 关闭 settings 来源和 hooks，使用严格 MCP 配置，默认不配置服务器。可用 `mcp_config` 配置包含 `mcpServers` 对象的 JSON **字符串**；`user` 加载正常的用户配置。两种策略都不是文件或网络沙箱，也不保证关闭所有项目记忆、skills 或扩展。`allowed_tools` 是 CLI 自动批准规则，不是工具可见性白名单，相应调用可能不经过审批桥接。

Claude 使用自身登录态；Savfox 不复制认证文件，也不隐式注入 provider API key。可通过 `terminal.env` 显式设置 key、代理或配置目录。环境身份参与摘要，绑定文件不保存 key 原文；同一登录目录内的账号切换目前无法检测。

## 聊天、审批与停止

`chat.send` 应携带客户端 `request_id` 和 UUID v7 `session_id`。原生 CLI 执行需要 `OperatorWrite`；默认 `required` 桥接还要求同一凭证的客户端具有 `OperatorApprovals`，完整 `Operator` token 同时具有这些权限。事件、运行查询、Claude 历史和审批按凭证隔离，相同显示名称的不同 token 不会共享身份。

Approvals 页面提供允许一次、拒绝、停止任务及 `AskUserQuestion` 问答。工具原始输入留在服务端，页面不能替换参数。`exec.approval.resolve` 必须携带原 request ID、nonce 和所属凭证；回答用索引键，例如 `"answers": {"0": "所选选项"}`，适配器转换为 CLI 原始问题文本对应的答案。过期、取消和重启后的审批不会授权另一执行。记录决策不表示已经证明 CLI 收到或工具执行。MCP elicitation 明确不支持。

`prompt` 在没有授权客户端时明确拒绝权限请求；新模式拒绝 `disabled`。Channel 调用需要认证后的 peer 范围；没有 WS 审批客户端时仅能使用 `prompt`，拒绝交互权限，并单独刷新可信的本轮上下文。Claude 的 Channel 审批通知/回传尚未接入。

聊天停止按钮按 session 调用 `chat.abort`；API 可按 `run_id` 停止。找不到明确目标时不会停止其他 Core 会话。全局 Core 停止必须显式 `all=true` 且具有 `OperatorAdmin`。Gateway 正常关闭会取消 Claude 执行，浏览器断开本身不会取消。

使用相同凭证重连后，可通过 `chat.run.get` 查询原客户端 request ID：

```json
{"method":"chat.run.get","params":{"request_id":"原始客户端 request ID"}}
```

返回状态、回复、诊断 stderr、厂商用量和活跃审批 ID/nonce。流式队列有界，丢失增量后可用最终回复或查询校准。用量缺失不会生成虚假的 `TokenUsage`；兼容 input 总量包含 cache creation/read，cached input 是其中 cache-read 的子集。重复请求不再发 prompt、写 rollout 或累计会话用量。

Gateway 重启后，未完成记录显示 `unknown_after_restart`，拒绝重用原 request ID 自动执行。先检查此前效果，再明确使用新 request ID 继续。查询按凭证和客户端 request ID 关联。

## 恢复与边界

`per_session` 保留明确的 Claude transcript ID，恢复轮只发送当前请求与附件。第一轮最多导入 20 条 Savfox 历史，并使用 CLI 系统指令参数。`per_turn` 每次独立且关闭 transcript 持久化；拒绝 `per_agent` 和 `manual`。身份、cwd、可执行文件/版本、环境、设置、模型配置或初始指令变化时要求新聊天。transcript 丢失不会自动新建重放。

恢复要求保留 workspace/session，拒绝 per-turn、delete-on-success/always 清理组合。协议错误、取消和清理不确定时保留目录。有限的 local agent/workflow 会在 result 后继续跟踪，无期限后台 shell 不当作有限任务。总期限包含一次审批期限余量，各审批有独立过期时间，不会无限延长执行。

当前进程组实现仅支持 Unix，Windows 新模式会在启动前拒绝，等待 Job Object/npm shim 验证。Unix 进程组不能保证收回已脱离组的后代；工作树和审批不是沙箱。

自动化测试使用合成 shell 协议进程，覆盖文本、用量、恢复、审批、问答、停止、帧上限、stderr 排空、后台后续轮、重复请求，以及 WebSocket 凭证隔离与重连。**这些测试不认证任何真实 Claude CLI 版本。** 部署前还需实测登录态、真实审批、resume、后台结束和平台进程清理。

新模式拒绝 `read_only` 和未知 workspace 模式；`shared`、`worktree`、`patch_only` 描述工作目录和补丁处理，不构成操作系统访问限制。
