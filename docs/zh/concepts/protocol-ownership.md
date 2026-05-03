# 协议所有权

Savfox 当前有三层协议承载，它们相关但不能混用。

## 所有权表

| Crate | 负责 | 不应负责 |
| ----- | ---- | -------- |
| `savfox-protocol` | 多个 native surface 共享的领域模型；tool/user-input/config/session 基元 | editor 专有 JSON-RPC 包装；browser 专有 view model |
| `savfox-app-server-protocol` | app-server JSON-RPC request/notification 以及 TS/schema 导出面 | 通用运行时业务逻辑；gateway browser 模型 |
| `savfox-gateway-shared` | gateway web UI 与 backend 共享 serde 类型 | TUI/editor 专有契约；core-only 内部模型 |

## 决策规则

### 放进 `savfox-protocol` 的情况

- 多个 native surface 需要同一个领域概念
- 该类型表达的是共享 agent/runtime 语义
- 数据应尽量保持 transport-neutral

### 放进 `savfox-app-server-protocol` 的情况

- 类型因为 app-server 与 IDE/editor 的 JSON-RPC 协议而存在
- 导出 tooling 必须包含它
- 它带有 app-server 的版本化约束

### 放进 `savfox-gateway-shared` 的情况

- browser frontend 与 gateway backend 直接共享它
- 它特定于 gateway 的 REST/WebSocket 或 web UI 行为
- wasm/native serde 兼容性是主要诉求
