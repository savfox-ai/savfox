# CLI 命令详解

本文档是 `savfox` CLI 的较详细命令参考，适合作为 `docs/zh/cli-reference.md` 的补充说明。

## 顶层用法

```bash
savfox [OPTIONS] [PROMPT]
savfox [OPTIONS] <COMMAND> [ARGS]
```

如果不带子命令，Savfox 会启动交互式 TUI。可选的 `PROMPT` 会作为首条消息。

## 常见全局参数

| 参数 | 说明 |
|------|------|
| `-c <KEY=VALUE>` | 覆盖配置，支持重复指定 |
| `-m, --model <MODEL>` | 指定模型 |
| `-p, --profile <NAME>` | 使用配置 profile |
| `--oss` | 仅使用开源模型提供方 |
| `--sandbox <MODE>` | 指定 sandbox 模式 |
| `--ask-for-approval <POLICY>` | 指定审批策略 |
| `--full-auto` | 尽量减少交互确认 |
| `--search` | 启用搜索模式 |
| `-C, --cwd <DIR>` | 覆盖工作目录 |
| `-i, --images <PATHS>` | 附加图片 |
| `--add-dir <DIR>` | 添加额外目录上下文 |
| `--enable <FEATURE>` | 开启 feature flag |
| `--disable <FEATURE>` | 关闭 feature flag |

## 常见子命令

### `savfox`

不带子命令时进入交互模式：

```bash
savfox "Explain async/await in Rust"
```

### `savfox exec`

非交互执行任务：

```bash
savfox exec "Add error handling to the parser"
savfox exec --json "List all TODO comments"
```

常见选项：

- `--json`
- `--quiet`

### `savfox review`

执行非交互代码审查：

```bash
savfox review
savfox review --target branch:feature
```

### `savfox resume`

恢复之前的交互会话：

```bash
savfox resume
savfox resume --last
savfox resume <SESSION_ID>
```

### `savfox fork`

基于旧会话分叉新会话：

```bash
savfox fork
savfox fork --last
savfox fork <SESSION_ID>
```

### `savfox gateway`

启动或管理 gateway：

```bash
savfox gateway
savfox gateway --port 8080 --token abc
```

常见子命令包括：

- `start`
- `stop`
- `restart`
- `status`
- `logs`
- `models`
- `approvals`
- `devices`
- `channels`
- `nodes`
- `install`
- `uninstall`

### `savfox login` / `logout`

管理登录状态和 API 凭据。

### 其他常见子命令

根据版本和构建特性不同，还可能包含：

- `app-server`
- `mcp`
- `sessions`
- `skills`
- `daemon`
- `cron`
- `doctor`
- `sandbox`

## 使用建议

- 日常开发：优先使用交互模式或 `exec`
- 自动化 / 脚本：优先使用 `exec --json`
- 远程接入：使用 `gateway`
- 需要恢复上下文：使用 `resume` / `fork`

更精细的参数和最新行为请优先以 `savfox --help`、具体子命令的 `--help` 以及当前代码实现为准。
