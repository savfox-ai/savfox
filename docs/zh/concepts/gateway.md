# Gateway 架构

Savfox gateway server 是一个常驻、多协议接入层。它把 Savfox 核心能力通过 WebSocket、REST 和聊天平台 webhook 暴露出来，并负责会话、渠道和后台服务管理。

## 高层组件图

```text
                    +--------------------+
 WebSocket (/ws) -->|                    |--> ThreadManager (Savfox core)
 REST (/api/*)   -->|   Gateway Server   |--> SessionStore
 Webhooks (/wh*) -->|     (Salvo)        |--> CronService
 OpenAI API (/v1)->|                    |--> Auth / Token scopes
                    +--------------------+
                            |
                 +----------+----------+
                 |                     |
          Channel Runtime         Web UI (SPA)
```

## 核心职责

gateway 负责：

- 统一接入外部客户端和聊天平台
- 持久化和查找 session
- 调用 core agent 引擎
- 托管渠道凭据和运行时配置
- 提供 WebSocket / REST / Web UI
- 承担 cron、审批、配对等后台服务

## GatewayChannel

`GatewayChannel` 是 gateway 内部的重要枢纽，负责串联：

- `ThreadManager`
- 配置服务
- 认证管理
- WebSocket session 管理
- 运行时渠道密钥
- HTTP client

## 认证模型

gateway 使用 bearer token 和 scope 做权限控制。常见 scope 包括：

- `Operator`
- `Viewer`
- `Chat`
- `OperatorRead`
- `OperatorWrite`
- `OperatorAdmin`
- `OperatorApprovals`
- `OperatorPairing`

同时支持原始 token 和基于 challenge 的 HMAC-SHA256 握手，以降低重放风险。

## WebSocket 生命周期

1. 客户端连接 `/ws`
2. gateway 下发 `ConnectChallenge`
3. 客户端发送 `Connect`
4. 鉴权成功后返回 `Connected`
5. 进入双向消息循环
6. 断开连接时清理相关状态

## JSON-RPC 分发

带有 `"jsonrpc"` 字段的消息会进入 WS-RPC 分发器，再由各 handler 负责具体方法执行。

这些方法覆盖：

- agent 管理
- sessions
- routing
- config
- 日志
- approvals
- 渠道管理

## 渠道运行时

聊天平台消息不会直接绕过 gateway 到 core，而是先经过 gateway runtime：

1. 解析平台消息
2. 做 dedupe
3. 解析 target agent
4. 做 trigger 决策
5. 复用或创建 session
6. 在需要时调用 agent

## Web UI

gateway 还负责托管前端静态资源，并向浏览器端提供：

- 配置界面
- sessions / agents 管理
- gateway 状态查看
- 渠道配置入口

## 设计目标

gateway 的总体目标是：

- 提供稳定的长生命周期服务层
- 让多端接入共享统一会话和策略
- 把平台差异收敛到渠道层
- 把 trigger、session、agent 配置统一收敛到 runtime
