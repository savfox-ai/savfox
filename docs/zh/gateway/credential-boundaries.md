# Gateway 凭据边界

## 远程模型发现

`models.list` 和 `models.test` 可以使用调用者指定的 `base_url` 发现模型。
自定义地址只会收到本次请求显式提供的 `api_key`，不会收到 gateway 保存的
OpenAI API key、ChatGPT OAuth token 或 ChatGPT 账户 ID。

修改持久 provider 与模型配置的操作（`models.add`、`models.update`、
`models.delete`、`models.deleteAccount`、`models.setdefault`、`models.import`、
`models.aliases.set`）要求 `operator_admin`。普通写权限不能通过覆盖导入已有账户
或改变默认模型来重定向已保存凭据。`models.list` 继续使用读权限，`models.test`
保留其写权限要求。

仅访问 `api.openai.com` 的 HTTPS 443 端口、根路径或 `/v1` 路径时，才会隐式
使用本机 OpenAI 凭据。URL 内嵌用户名密码、查询参数、片段、其他端口或路径
均禁用此回退。使用 ChatGPT 登录时，这类可信 OpenAI 发现请求会被转换为
管理员配置的 ChatGPT Codex 地址。最终目标仍受 SSRF 策略和 DNS 固定解析约束。

## WebChat 会话引导

`POST /webchat/session` 要求有效 gateway bearer token，且包含 `chat` scope
（粗粒度 `operator` 角色也包含此权限）。缺少或无效凭据返回 `401`；有效但
没有聊天权限的 token 返回 `403`。

返回的会话 token 授予该 WebChat 会话的 agent 执行能力；`/webchat/send` 与
`/webchat/poll/{session_id}` 继续使用此会话 token。创建会话不会建立独立受限的
公共 agent，执行仍遵循 gateway 的配置策略。

嵌入应用须在可信后端验证自己的用户并创建会话。不要将 gateway bearer token
放入浏览器配置。加载 `/webchat/widget.js` 前，仅提供已授权用户的会话凭据：

```javascript
window._WEBCHAT_Config = {
  sessionId: session.session_id,
  sessionToken: session.token
};
```

未提供预创建会话时，widget 显示登录提示并禁用输入，不再匿名创建会话。
会话 token 在内存会话存续期间仍是有效能力凭据；撤销用于引导的 gateway token
不会单独撤销已经签发的会话凭据。

## Provider 账户文件

Gateway 的 provider 文件操作仅接受 ASCII 字母、数字、连字符和下划线构成的
账户 ID，并排除 Windows 保留设备名。文件加载、保存、删除和导入目标共用校验。
路径、盘符、UNC 路径、点号与备用数据流会在访问文件前被拒绝。已有自定义 ID
不符合此格式时，需要先重命名再通过 gateway 使用。

此规则保护远程账户参数。管理员仍须控制本地 provider 目录及其链接；它不是针对
能够并发替换本地文件或目录链接的用户的文件系统沙箱。
