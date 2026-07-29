# Channel 执行安全

Savfox 对 WebSocket、HTTP 和 Channel 入口使用同一个 Agent 安全策略解析器。
能力边界与交互方式相互独立：

- `permission_policy` 控制沙箱、工具、命名权限档案、细粒度审批类别和网络意图。
  一旦设置 `profile`，它就是权威来源；旧 `sandbox` 字段只在没有 profile 时使用。
- `execution_policy.mode` 控制越界请求的处理方式：
  `interactive`、`unattended` 或 `auto-review`。

只有能返回关联式决策的入口才会使用 `interactive`。`unattended` 不等待人工，
而是立即向 Core 提交拒绝。未配置 Reviewer 时，`auto-review` 会安全降级为
`unattended`。

## Agent 配置示例

```json
{
  "permission_policy": {
    "profile": ":workspace",
    "approval": "granular",
    "granular_approval": {
      "sandbox_approval": true,
      "rules": true
    }
  },
  "execution_policy": {
    "mode": "interactive"
  }
}
```

内置权限档案为 `:read-only`、`:workspace` 和 `:danger-full-access`。
未知档案或格式错误的策略按 fail-closed 处理。

## 关联式审批

每个审批包含服务端生成的 request ID、一次性 nonce、精确的 Core
Session/approval operation、Agent、Channel 实例/账号/Peer、逻辑 Session、策略
fingerprint、过期时间和脱敏摘要。Telegram 与 Discord 可显示结构化按钮；
纯文本客户端可以回复：

```text
approve:<request-id>
approve-session:<request-id>
allow-rule:<request-id>
deny:<request-id>
abort:<request-id>
```

兼容的 `+`/`-` 只在相同认证 Channel 范围内恰好存在一个 pending 时有效。
超时、过期、重启、重放、不同 Peer 或不同 Session 均不能批准该请求。

审批发起、审批读取与审批提交使用不同 token scope：
`operator_approvals_request` 只能创建请求，不能读取 nonce 或自行批准；
`operator_approvals_read` 可以读取 pending 与 nonce；
`operator_approvals_resolve` 只能提交已知 request ID 与 nonce。
旧的 `operator_approvals` 同时包含三者。

## 规则和模拟器

旧 Gateway approval policy 已停止写入。全局旧规则经过严格解析后一次性迁移到
`rules/default.rules`；per-node 规则不会被扩大成全局规则。新的永久授权只写入
Core execpolicy。

Agent 页面展示实际 enforcement backend、Core 规则和策略模拟器。模拟器调用
`security.policy.simulate`，使用真实的 Core 分层策略进行判断，但不会执行命令。
规则新增与撤销分别使用 `security.rules.add` 和 `security.rules.remove`。

## Windows

Restricted Token 沙箱默认启用。Elevated 沙箱尚未完成 setup 时，Gateway
退回 Restricted Token；WorkspaceWrite 没有可强制执行的 Windows 沙箱时，
有效策略退回 ReadOnly。Agent 页面会显示实际 backend 和降级原因。

Restricted Token 提供真实的文件系统与进程身份边界，但它自身还不等同于最新版
Codex 的 WFP 强网络边界。非提权兼容路径会移除代理凭据并设置阻断代理环境变量；
完成 elevated setup 后还可使用 offline identity/firewall 路径。Agent 的域名级
网络意图会进入策略 fingerprint，但在 managed proxy 就绪之前，Savfox 不宣称
已经实现按域名 allowlist 的强制执行。

当前 `granular_approval` 只实际强制 `sandbox_approval` 和 `rules`。
带作用域的 `RequestPermissions`、Guardian 自动复核和域名级临时授权会等到完整
运行时执行链就绪后再暴露配置。
