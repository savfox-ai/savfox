# Cokret 加解密支持任务列表

## 目标判断

当前 Savfox Cokret 集成已经能走明文 `ck.content.text`、DID-proof 登录、事件签名、grant 校验和 applet idempotency；但完整端到端加解密还未完成。完整 E2EE 需要引入 Cokret crypto session / MLS state / key backup / realm encryption policy 的持久状态，并在 account 与 applet 两条消息路径里同时接入。

## 并行关系

- T1 和 T2 可以并行：分别处理 account-mode 与 applet-mode 入站 encrypted carrier 的分类。
- T3 依赖 T1/T2：gateway 只有拿到明确 skip reason 后才能做观测和 applet rejected reason。
- T4A 依赖 T1，T4B 依赖 T2，但 T4A/T4B 可并行执行。
- T5 依赖 T1-T4 全部完成。
- E1-E3 是完整 E2EE 的基础链路，必须先于 E4/E5。
- E4 和 E5 在 E1-E3 完成后可并行：一个做入站解密，一个做出站加密。
- E6/E7/E8 可在 E4/E5 之后并行推进。

## 本轮可闭环任务

- [x] T1 account-mode：识别 `encrypted_content` / `encrypted_payload` / `ck.content.encrypted`，返回明确 `EncryptedContent` skip reason。
- [x] T2 applet-mode：识别 `encrypted_content` / `encrypted_payload` / `ck.content.encrypted`，返回明确 `AppletDispatchSkip::EncryptedContent`。
- [x] T3 gateway 观测：account encrypted skip 记录 warning；applet transaction rejected 返回 `EncryptedContent` 并记录 warning。
- [x] T4A account-mode 测试：覆盖 encrypted content block、spec `encrypted_content` carrier、delta skip 汇总。
- [x] T4B applet-mode 测试：覆盖 encrypted content block、spec `encrypted_content` carrier。
- [x] T5 验证：运行 targeted fmt/check/test/clippy。

## 完整 E2EE 后续任务与工作量

- [x] E1 依赖与 feature 图确认（S）：启用 Cokret `mls`、`device-runtime`；当前 account/appet 入口暂不需要 `sync-runtime` / `timeline-runtime`，避免额外运行时体积。
- [x] E2 持久 crypto store（M-L）：为每个 Cokret account/applet 绑定本地 `FileCokretCryptoStore`，持久化 SDK feature report、MLS store backup JSON、realm policy、bootstrap plan、key-backup 状态和 unable-to-decrypt records。
- [x] E3 realm/session bootstrap（L）：account subscribe 同步 realm encryption policy；account/applet 入站密文都会生成 MLS recovery/bootstrap plan，并在缺 session 时 fail closed。
- [x] E4 入站解密（L）：`encrypted_content` / `encrypted_payload` / `ck.content.encrypted` 反序列化为 SDK `EncryptedPayload`，存在本地 group state 时尝试 MLS 解密，失败写 unable-to-decrypt。
- [x] E5 出站加密（L）：account/applet 发送前按 realm `content_encryption_floor` / `encryption_profile` 加密；需要 E2EE 但缺 group state 时阻止明文提交。
- [x] E6 applet E2EE 策略（M）：applet 支持 `deviceId` 配置、describe 暴露 E2EE 能力，明文 fallback 仅限 realm policy 允许。
- [x] E7 key backup / recovery（M-L）：接入 SDK recovery planning 与本地 key-backup 状态，重启后保留恢复需求；远端 RRK/secret-share 拉取需 Cokret 服务端密钥备份事件/接口对接后扩展。
- [x] E8 inbound applet HTTP Message Signature（M）：在 bearer + DID 来源校验之外，接入 SDK HTTP signature / content-digest 验证，绑定交易 idempotency anchor。
- [x] E9 集成测试与 KAT（L）：覆盖 encrypted carrier、policy sync、missing session fail-closed、trusted HTTP signature、tampered body reject、配置 trusted key 校验和 targeted compile/clippy。
