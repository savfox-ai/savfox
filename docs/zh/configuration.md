# 配置

Savfox 使用分层配置系统，支持配置档案、命令行覆盖和特性标志。

## 配置文件位置

```
~/.savfox/config.toml
```

Windows 上：`%USERPROFILE%\.savfox\config.toml`

## 配置区块

### 认证

```toml
[auth]
provider = "openai"        # 或 "chatgpt"、"ollama"、"lmstudio" 等
api_key = "sk-..."         # API 密钥（使用密钥认证时）
```

对于 ChatGPT 托管认证（推荐），使用 `savfox login` 而非手动设置 API 密钥。

### 模型选择

```toml
[model]
provider_id = "openai"
model = "gpt-4o"
```

### 沙箱策略

```toml
[sandbox]
mode = "workspace-write"   # "read-only"、"workspace-write"、"full-access"
```

### 网关服务器

```toml
[gateway]
port = 18881
host = "127.0.0.1"
token = "my-secret-token"  # 省略则自动生成
```

### 聊天桥接

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "..."

[gateway.bridges.telegram]
enabled = true
bot_token = "..."

[gateway.bridges.slack]
enabled = true
bot_token = "..."
signing_secret = "..."
```

### MCP 服务器

```toml
[mcp.servers.my-server]
command = "node"
args = ["server.js"]
```

### Skills

```toml
[skills]
enabled = true
# paths = ["./custom-skills/"]
```

### Shell 偏好

```toml
[shell]
# 终端/Shell 偏好设置
```

### Git 集成

```toml
[git]
# Git 相关设置
```

## 命令行覆盖

使用 `-c` 从命令行覆盖任意配置值：

```bash
savfox -c model.model=gpt-4o exec "你好"
savfox -c sandbox.mode=read-only exec "读取这个文件"
savfox -c gateway.port=9090 gateway
```

可以叠加多个覆盖：

```bash
savfox -c model.model=gpt-4o -c sandbox.mode=full-access exec "执行操作"
```

## 配置档案

使用命名档案在不同配置之间切换：

```bash
savfox --profile work exec "工作项目任务"
savfox --profile personal exec "个人项目任务"
savfox -p oss exec "使用 OSS 模型"
```

## 特性标志

特性标志控制实验性和开发中功能的访问权限。

### 阶段

- **under-development** — 尚未准备好供一般使用
- **experimental** — 可用但可能变化
- **stable** — 生产就绪
- **deprecated** — 将在未来版本中移除

### 管理特性标志

通过 CLI：

```bash
savfox features              # 列出所有标志及其状态
savfox features enable <F>   # 启用特性
savfox features disable <F>  # 禁用特性
```

或通过命令行选项：

```bash
savfox --enable my-feature exec "使用新特性"
savfox --disable some-feature exec "不使用该特性"
```

## 配置优先级

配置按以下顺序解析（后者覆盖前者）：

1. **系统默认** — 内置默认值
2. **用户配置** — `~/.savfox/config.toml`
3. **工作区配置** — 项目级 `.savfox/config.toml`
4. **配置档案** — 命名档案覆盖
5. **命令行覆盖** — `-c key=value` 选项
6. **云端要求** — 企业/托管设置（如适用）

## JSON Schema

导出配置的 JSON Schema 供编辑器集成：

```bash
savfox app-server generate-json-schema -o ./schema
```

这会生成一个 JSON Schema 文件，编辑器可以用它来提供 `config.toml` 的自动补全和验证。
