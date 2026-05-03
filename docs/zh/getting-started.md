# 快速开始

## 前置要求

- **Rust 1.94+**（从源码构建时需要）
- 支持的 LLM 提供商账号（OpenAI，或本地提供商如 Ollama / LM Studio）
- Git（用于会话 diff 和补丁应用）

## 安装

### 从源码构建

```bash
git clone https://github.com/savfox-ai/savfox.git
cd savfox
cargo install --path crates/savfox-cli
```

### 验证安装

```bash
savfox --version
```

## 认证

使用 Savfox 前，需要先进行身份认证：

```bash
savfox login
```

这会启动一个交互式流程来配置你的 API 凭证。Savfox 会将凭证安全地存储在系统密钥环中。

如果使用本地开源提供商：

```bash
savfox --oss login
```

退出登录：

```bash
savfox logout
```

## 第一次交互式会话

直接运行 `savfox`（不带子命令）即可启动交互式终端界面：

```bash
savfox
```

在 TUI 界面中，你可以：
- 输入消息与 AI 智能体对话
- 查看代码变更的 diff 预览
- 批准或拒绝命令和文件修改
- 切换模型和协作模式

详见 [交互模式](interactive-mode.md)。

## 第一次非交互式执行

使用 `exec` 子命令（别名 `e`）来执行一次性任务：

```bash
savfox exec "为 src/main.rs 添加错误处理"
```

智能体会处理你的请求，提出修改方案，并将结果输出到终端。使用 JSON 输出（适用于脚本集成）：

```bash
savfox exec --json "列出代码库中所有的 TODO 注释"
```

详见 [CLI 参考](cli-reference.md)。

## 常用技巧

- 使用 `--model` 或 `-m` 指定不同的 LLM 模型：
  ```bash
  savfox -m gpt-4o exec "解释这段代码"
  ```

- 使用 `--full-auto` 启用低干预自动执行模式：
  ```bash
  savfox --full-auto exec "重构认证模块"
  ```

- 恢复之前的会话：
  ```bash
  savfox resume        # 交互式会话选择器
  savfox resume --last # 恢复最近的会话
  ```

## 下一步

- [交互模式](interactive-mode.md) — 了解 TUI 界面功能
- [CLI 参考](cli-reference.md) — 所有命令和选项
- [配置](configuration.md) — 自定义 Savfox 行为
- [网关服务器](gateway.md) — 远程访问和聊天桥接
