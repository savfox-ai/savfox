# 沙箱与安全

Savfox 提供多层沙箱机制来控制 AI 智能体在系统上的操作权限，确保命令执行和文件操作的安全性。

## 沙箱模式

使用 `--sandbox` 设置沙箱模式：

```bash
savfox --sandbox read-only exec "分析这个代码库"
savfox --sandbox workspace-write exec "重构认证模块"
savfox --sandbox full-access exec "安装依赖并构建"
```

| 模式 | 说明 |
|------|------|
| `read-only` | 禁止文件系统写入 |
| `workspace-write` | 仅允许写入工作区目录（`.git` 和 `.savfox` 为只读） |
| `full-access` | 不限制文件系统访问 |

## 审批策略

控制智能体在执行操作前何时请求你的批准：

```bash
savfox --ask-for-approval unless-trusted exec "执行操作"
```

| 策略 | 说明 |
|------|------|
| `unless-trusted` | 除非命令在信任列表中，否则询问（默认） |
| `on-request` | 仅当智能体明确请求批准时询问 |
| `on-failure` | 仅当命令失败时询问 |
| `never` | 自动批准所有操作（建议配合沙箱限制使用） |

## 便捷模式

### 全自动模式

将宽松的审批策略与工作区写入沙箱相结合：

```bash
savfox --full-auto exec "重构数据库模块"
```

等同于 `--ask-for-approval on-request --sandbox workspace-write`。

### 跳过模式（危险）

跳过所有确认和沙箱限制。**仅在可信的隔离环境中使用：**

```bash
savfox --yolo exec "执行所需的一切操作"
```

这个选项名称本身就在提醒你其中的风险。

## 平台原生沙箱

Savfox 支持操作系统原生沙箱机制，提供更强的隔离能力：

### macOS — Seatbelt

使用 Apple 的 `sandbox-exec` 在操作系统级别强制执行沙箱：

```bash
savfox sandbox macos <command>
# 别名：
savfox sandbox seatbelt <command>
```

### Linux — Landlock + seccomp

使用 Linux 内核安全模块进行细粒度访问控制：

```bash
savfox sandbox linux <command>
# 别名：
savfox sandbox landlock <command>
```

### Windows — 受限令牌

使用 Windows 受限令牌限制进程权限：

```bash
savfox sandbox windows <command>
```

## 额外可写目录

使用 `workspace-write` 模式时，可以授予额外目录的写入权限：

```bash
savfox --sandbox workspace-write --add-dir /tmp/output exec "将报告生成到 /tmp/output"
```

## 最佳实践

1. **默认使用 `workspace-write`** — 防止意外修改项目外的文件，同时允许智能体正常工作。

2. **分析时使用 `read-only`** — 当你只需要智能体阅读和解释代码时，使用只读模式获得最大安全性。

3. **将 `never` 审批与沙箱结合** — 如果需要无人值守执行，将 `--ask-for-approval never` 与限制性沙箱模式搭配使用，而不是使用 `--yolo`。

4. **不信任的任务使用平台沙箱** — `sandbox` 子命令提供操作系统级别的隔离，比内置策略检查更强。

5. **在网关模式下审查审批** — 运行网关服务器时，使用 `savfox gateway approvals list` 审查来自远程客户端的待处理审批。
