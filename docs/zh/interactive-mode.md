# 交互模式

直接运行 `savfox`（不带子命令）会启动基于 ratatui 的终端交互界面，与 AI 智能体进行实时对话。

## 启动

```bash
savfox                    # 开始新会话
savfox resume             # 选择之前的会话继续
savfox resume --last      # 恢复最近一次会话
savfox fork               # 从之前的会话分叉
```

## 界面概览

TUI 界面提供：

- **聊天输入框** — 输入消息发送给智能体
- **Markdown 渲染** — 智能体回复支持语法高亮
- **Diff 展示** — 以统一 diff 格式展示代码变更
- **审批弹窗** — 接受或拒绝命令和文件修改
- **状态指示器** — 加载动画和进度反馈

## 会话管理

### 恢复会话

从之前中断的对话继续：

```bash
savfox resume             # 显示交互式会话选择器
savfox resume --last      # 恢复最近的会话
savfox resume <SESSION_ID>
```

### 分叉会话

从之前的会话分叉，探索不同的方向：

```bash
savfox fork               # 选择一个会话进行分叉
```

### 归档

会话存储在本地，可以通过 TUI 界面进行归档或列表浏览。

## 审批流程

当智能体需要执行命令或修改文件时，Savfox 会弹出审批对话框：

- **命令执行** — 在运行前审查命令内容
- **文件变更** — 在应用前审查 diff 内容

你可以逐个批准或拒绝操作。审批策略可以全局配置：

```bash
savfox --ask-for-approval unless-trusted   # 默认：除非命令在信任列表中，否则询问
savfox --ask-for-approval never            # 自动批准所有操作（建议配合沙箱使用）
savfox --ask-for-approval on-failure       # 仅在失败时询问
```

## 模型和协作模式选择

在 TUI 界面中，你可以：

- 在可用的 LLM 模型之间切换
- 选择不同的智能体协作模式
- 通过覆盖层界面进行配置

也可以从命令行设置模型：

```bash
savfox -m gpt-4o
savfox --oss           # 使用本地模型（Ollama、LM Studio）
```

## 斜杠命令

交互模式支持斜杠命令来快速执行操作。在聊天输入框中输入 `/` 即可查看可用命令。

## 快捷键

TUI 使用标准终端按键绑定，导航和输入遵循常规终端操作习惯。

## 使用技巧

- 使用 `Ctrl+C` 中断正在执行的智能体操作
- 会话自动保存 — 你可以随时恢复之前的会话
- 使用 `-i` 附加图片到消息中：
  ```bash
  savfox -i screenshot.png
  ```
