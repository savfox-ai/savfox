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

## 与 Gateway Agent 配置的区别

这里描述的是 **运行时子 agent / 线程角色模型**。  
它与 gateway 里的“可配置 agent 实例”是相关但不同的两层：

- 运行时角色：强调线程职责和派生行为
- gateway agent 配置：强调模型、prompt、trigger、渠道策略等长期配置
