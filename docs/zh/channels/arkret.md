# Arkret Agent 频道

Windows 上的 Arkret runtime 私钥与加密状态 wrapping key 使用 SDK 已有的当前用户
DPAPI 保护加密仓库，不再占用 Windows Credential Manager 条目。`keyring` key reference
继续表示平台保护的存储位置，私钥不进入 channel JSON。旧 Windows 凭据库条目不导入，
需要重新配对以授权新的 runtime key。macOS、Linux 继续使用各自的原生 keyring。

配对核对码为八位数字，显示为两组四位，保留前导零。批准前应确认 Inkson 与 Savfox 显示相同核对码。权限用途和授权期限在主视图显示，授权引用、运行密钥标识与完整错误链位于可展开的技术详情中。

审批状态查询失败后，**Check approval** 恢复同一请求的查询；过期与否以 Station 的正式状态为准。手动停止等待不会撤销已批准请求或丢弃同一运行密钥，不必重发已消费的配对链接。

提交审批和轮询审批状态时，校验的是尚未获批的配对配置，不要求提前提供
`authorizedEventRef`。该引用在 Inkson 批准运行时密钥后返回；保存后的频道启动时
仍必须具备该引用。配对错误会显示具体的校验失败原因。

配对链接解析、运行时密钥审批提交和审批状态轮询均发送 Station 要求的精确版本
`Arkret-Operation` 请求头。如果配对提示 `operation_selector_required`，请先使用
包含此支持的代码重新构建并重启网关，再重试配对。

Agent 模式下粘贴 Inkson pairing link 后即可点击 **Start pairing**。Controller Account ID
和 Runtime key DID URL 是内部字段，由链接解析响应中的 `runtime_identity` 自动提供，
随后 Savfox 生成本地密钥并请求 Inkson 审批。若服务仍只返回旧的六字段 bootstrap，
需更新配对服务；界面会明确报告身份信息缺失，不要求手填身份，也不会让按钮一直禁用。
已有保存的完整绑定仍可使用。

Savfox 的 Arkret Agent 运行时处理已授权的消息订阅、回复和加密在线状态心跳。
Agent Signal 不携带设备 ID；当前运行时的原始 Ed25519 公钥摘要标识序列端点，
运行时密钥同时签署 Signal proof。每个 Realm 的 MLS nonce 与 payload sequence 都在
HTTP 提交前持久化，因此不确定的提交可以跳号，但不会复用 nonce 或倒退序列。

配对 bootstrap 的 `agent_id` 承载稳定的 `ak:did_core:*` 身份；运行时密钥 DID URL
是 Agent/controller 身份流程另行提供的完整 `did:<method>:...#<fragment>`。Savfox
通过共享 DID method adapter 校验该 DID controller 能投影到 bootstrap 的 `agent_id`，
不会把它拼成 `agent_id#device_id`，也不会把本地或会话设备标识当成 Agent MLS actor。

审批完成后，Agent MLS endpoint 精确绑定
`(agent_id, verificationMethod, authorizedEventRef)`。SessionGrant 以及所有
KeyPackage、claim、Welcome、receipt 和 consume 操作都必须保持该绑定；主体、运行时
密钥或授权 Event 任一不一致即拒绝，不回退到 human-device 身份。

新配对会申请 `ak.self.signal.command.send.v1`。Station 仍须验证 Agent 分类、生命周期、
唯一 controller、当前 runtime 与 session 授权，以及精确匹配的 MLS leaf；接收端仅按
经过验证的当前原始公钥摘要接受并投影在线状态。Agent 密钥不会合成设备 ID。

新配对请求的候选权限由共享 Arkret SDK 操作注册表与能力最低集合生成，包含
标准 committed-event stream 订阅、扫描及安全消息操作。服务权限使用精确的带版本操作 ID，例如
`ak.self.committed_event.read.scan.v1`；`ak.event.read` 等内容动作保持原名。普通在线聊天默认
不申请延迟发布 lease。

编辑已配对频道的交付模式等设置时，隐藏的旧核对码与密钥引用由服务器保留，不要求重新配对。选择 `interactive_chat` 接收聊天回复；`task_delivery` 用于任务交付。原权限数组保持不变；缺失、旧无版本名或 `query` 别名均明确拒绝，
不会自动升级。新候选也只是申请，Station 仍须独立检查不可变 provision、当前 key
与 session 三层上限。实际 session grant 必须保留配置所需的全部服务操作，并且不得
超出申请范围。没有资源授权时，授权方可以移除内容动作；消费已接受的原生 Sidecar
请求时，运行时刷新并核对 committed-event 读取操作、精确 Agent AccountId 与 audience。
资源 grant 和 action policy 仍独立约束写入与回复。缺失服务操作或超额授予均拒绝。最后成功 session 的缓存仅供诊断，不代表
当前 key/provision 授权，也不能用于给已有 Agent 增加权限。

保存交付模式后，已有对话的后续入站消息也使用新模式。两个模式使用独立的本地执行会话，
仅共享已验证的远端对话历史。切换为聊天不会公开或续用旧任务的私有执行记录；切回任务模式
会继续原任务会话。

按服务返回的原因恢复：不可变 provision 缺项需新建 Agent；key 缺项需在 provision
上限内重新授权；session 缺项需在两层上限内刷新 session。这些权限检查通过不表示
独立的 Agent 身份与运行时迁移已经完成。
无效的旧绑定不会被报告成解绑成功。若旧身份或权限阻止安全撤销，Savfox 保留本地
状态并要求 controller 侧恢复；只有真实解绑确认后才清除旧权限，并允许同一空频道
槽位生成新的配对候选。

清理旧配对的 KeyPackage 池时，仅读取密钥包清单并核对所属 Agent，旧文件中的过期
消息 ID 格式不会阻止新运行时启动。远端确认撤销后才写入本地退休标记；其他状态
和其他 Agent 的密钥保持原样。

自有 Agent 私聊必须先完成已验证的 MLS Welcome 入群，运行时才能解密消息或发布
加密在线状态。Savfox 验证治理闭包与 controller/Agent 成员密钥归属，将入群状态
持久化并回读后才签署接收端持久回执，并立即提交 KeyPackage consume。Direct Conversation binding 要等该 consume 完成，运行时不能反过来等待 binding 才确认 Welcome。内容加密方案由 accepted 治理绑定决定；
exporter AEAD 回复在提交前保留并持久化计数器，重启后也不会复用计数器。

Standard MLS 消息以完整 sender ActorId（包含 Station）作为 AAD 身份，不使用 device/runtime 验签方法的选择器。signed RealmGenesis purpose 识别私聊，普通消息无需提及即可触发。解密失败或尚无 binding 的新触发保留在持久 inbox，历史恢复仍验证正式 accepted stream。回复在加密和签名前携带 exact participant binding；重新入群使用同组 fresh verified Welcome 推进 epoch，不重置群组或复活过期领取。

待处理的接收项不阻断后续投递，但累计 ACK 和持久队列 cursor 不跨过首个未完成项。Welcome 分配先从本机可用库存扣除对应一次性 KeyPackage，因此过期领取也会触发补充。替换初始库存项时保留旧记录；缺失的公开记录只能根据已验证 claim 恢复，并要求本机已持有逐字匹配的私钥包。

## 回复模式、确认与故障恢复

`interactive_chat` 将模型回复正文发回原远端对话；`task_delivery` 将完整模型执行输出保留在
本地私有会话，只通过任务交付路径发布公开检查点。看不到普通模型正文不表示任务模式没有执行。
交付模式是 Savfox 的产品设置，不是 Arkret 权限、MLS 状态或新的协议 profile；切换模式不扩权，
也不会重投已经确认的历史消息。

普通消息经过“已认证独立 stream → 耐久 inbox → 可信路由预检 → coordinator 接纳 →
模型执行 → 回复提交”处理。路由预检必须包含 saved config、account、Realm、Strand、SDK
`stream_ref`、原请求 Event ID 和 sender；不能从展示标题或父 Realm 猜 private stream。
缺字段或 binding store 不可读时，必须在 coordinator 接纳前返回失败，保留 inbox 重试机会。
worker 复用同一验证。内存 coordinator 接纳不是模型完成，也不是另一份耐久执行队列；当前
路径不能据此承诺进程崩溃后的模型执行或回复“恰好一次”。检查模型会话与实际 accepted 回复，
不能只以 inbound/dispatched 计数判断完整链路成功。

本地 inbox 完成位点与 Arkret to-device 累计 ACK 是不同边界；后者遵循 v1 `client-sync` §10.1，
不能越过未耐久处理的 Welcome/DeviceMessage。不要把二者混成一个 receipt 或游标。

网关 WebSocket 先完成 typed `connectChallenge`/`connect` 认证，之后可直接发送顶层带 `jsonrpc`
的 JSON-RPC 请求，不需要 `type` 包装。JSON 字段顺序不影响分类；即使 `params` 很大或含非 ASCII
文本、`jsonrpc` 在最后也必须正常处理。嵌套对象或字符串里的同名内容不能选择 RPC 路由；1 MiB
帧上限、正式参数验证和权限检查仍保留。

| 现象 | 检查与恢复 |
| --- | --- |
| 重新配对后仍显示旧 session 错误或旧连接 | 运行状态只属于保存配对中的精确 account，退休账号不能让当前频道变成正常或异常。连接测试成功仅证明网络可达；仍须分别检查当前 listener 和实际已接受的回复。 |
| Inkson 提示 Encryption state moved while sending | 并发 MLS 迁移可能拒绝旧加密上下文。已验证当前状态就绪后，将恢复的明文草稿重新创作为新消息；不重投已拒绝的旧密文，也不重置群组。提交结果未知时则继续 pending，由原字节恢复路径确认。 |
| 路由报 `missing field streamRef`，尚未进入模型 | 保留不可读文件及诊断。不可凭空迁移其缺失坐标；由操作者归档明确废弃的旧开发数据后，按新入站原件建立路由。不要清 dedupe 来重放旧指令。 |
| 启动提示 keyring entry not found，channel 未监听 | 核对启动时的 Windows 用户、受保护存储命名空间和实际 `keyRef`。批准记录不包含私钥，重启或重新保存配置不会补出它。 |
| 原 runtime 私钥确实遗失 | 在 Inkson 对原 Agent 使用 Replace runtime，生成未用于该 Agent 的新 raw key，并完成 controller 批准。保持原权限上限、身份和生命周期；不要新建同名 Agent 冒充原身份。 |
| gateway 已发回复，页面停在 Verifying sender identity | 检查接收端 exact committed Agent 签名证据解析及 MLS leaf authorization。单条回复也应主动解析并更新待验消息，不依赖后续 Account 帧或刷新；不能先展示未验证正文。 |

等权替换沿用原 Direct 的 group 与 binding，通过标准 Remove/Add/Welcome 收敛新 runtime endpoint，
不会替换其它 human device。它不恢复已经遗失的旧私钥或 MLS 私态，也不保证解开旧 wrapping key
保护的数据。保留身份、聊天历史、wrapped crypto 与恢复材料，不通过清库、重置 epoch 或重新
Genesis 消除 pending。仅在 controller 明确授权解绑且远端确认后执行原解绑清理。

### Applet 出站身份

Applet 模式必须配置由已接受 provision 结果保留的完整 `bot_account_id`，其值为
Arkret `AccountId` 对象，包含 `principal_id` 和 `station_id`。旧字符串配置
`botActorId` / `bot_actor_id` 不再接受；服务 DID 或目标服务器地址不能补出账户 Station。

同时明确配置可解析的 `service_did`、与其对应的 `serviceId`、`trust_domain` 和服务
`verification_method`；`keyRef` 必须对应该服务签名密钥。出站运行时还需要已有的
`namespaces` 以及 `managed_actor_authoring`，后者包含 `principal_endpoint` 与
`key_encryption_key_hex`，应使用实际安装的身份托管配置。消息 `actor_id` 保留完整
Bot Account，`executed_by` 为 Applet 服务。普通发送使用本地保留的授权上下文。
