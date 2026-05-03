# Crate 边界

这个文档说明新代码应该落在哪一层。

## 规则

### `savfox-core`

承载 transport 无关的共享 Agent 行为：
- 配置加载与编辑
- auth、session、rollout、tools、sandbox
- model/provider runtime 协调
- skills、memory、prompt 组装

不要把 gateway 专有 HTTP handler、Dioxus UI state 或 editor 专有 JSON-RPC framing 放进这里。

### Surface crates

以下 crate 应保持轻薄：
- `savfox-cli`
- `savfox-tui`
- `savfox-app-server`
- `savfox-mcp-server`
- `savfox-gateway-server`
- `savfox-gateway-dioxus`

它们可以处理 transport、生命周期和 UX，但不应复制核心策略逻辑。

### 协议 crate

- `savfox-protocol`：native 表面共享协议与数据模型
- `savfox-app-server-protocol`：app-server 专有 wire contract 与导出面
- `savfox-gateway-shared`：gateway browser/backend 共享 serde 类型

协议 crate 应该只承载数据契约，不承载运行时业务逻辑。

## 依赖方向

推荐方向：

```text
surface crates -> core / protocol / support crates
support crates -> protocol / lower-level support crates
core -> protocol / lower-level support crates
```

避免：
- `savfox-core` 依赖 gateway、TUI、app-server 或 Dioxus
- 协议 crate 依赖 surface crate
- `savfox-gateway-dioxus` 依赖 native-only runtime crate
- `savfox-channels` 依赖 TUI 或 app-server
