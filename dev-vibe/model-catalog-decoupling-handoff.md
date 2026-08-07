# 模型目录去硬编码 — 完成记录

## 0. 最终状态

模型清单与模型能力的去硬编码工作已经完成，相关后续问题也已闭环；改动仍在工作区，尚未提交。

最终原则：

- 模型清单只来自活动 catalog：远程 `/models` 缓存、provider store、测试/配置注入。
- 模型能力只来自 `ModelInfo`，不再根据 OpenAI 或 Savfox 私有 slug 推断。
- 未命中 catalog 时只生成显式标记的保守 fallback，并写 warning 日志。
- `Feature::RemoteModels` 只控制网络刷新，不再屏蔽本地缓存和 provider store。

## 1. 已完成的实现

### Catalog 与能力解耦

- 删除内置 `model_presets.json` 以及 `local_models` 基线。
- `find_model_info_for_slug` 不再包含按 slug/prefix 的能力分支。
- `AuthMode::ApiKey` 可以刷新第三方 provider 的模型目录。
- provider store 的真实 `id` / `model_slug` 格式会被归一化为 `ModelInfo`，并保留 catalog 给出的 reasoning、shell、apply-patch、并行工具调用等能力。
- provider 连接写入 `models_fetched_at`；远程缓存与 provider stores 合并，不再互相遮蔽。
- 空 catalog 会给出明确错误和 TUI 空态，不会使用空模型名发请求。

### 本地、离线与 feature-off 行为

`Feature::RemoteModels` 关闭时仅禁止网络发现，仍然：

- 读取未过期的磁盘模型缓存；
- 读取 provider store；
- 通过 `get_remote_models` / `try_get_remote_models` 暴露已经加载的目录。

对应回归：

- `remote_models_feature_off_still_lists_local_provider_catalog`
- `remote_models_feature_off_still_uses_disk_cache_without_network`

### Fallback 可观测性

fallback 元数据继续设置 `ModelInfo::used_fallback_model_metadata = true`，`ModelsManager::get_model_info` 同时写 warning，避免能力降级静默发生。服务端若将来漏发能力字段，应修复 catalog 契约，不应恢复 slug 推断。

### Personality

本地、按私有模型前缀绑定的 personality 模板已经删除。Personality 功能仍然存在，但由 catalog 的 `ModelInfo::model_messages` 决定。测试均显式构造带 `model_messages` 的 catalog 条目，不再依赖私有模型名。

### 私有实验模型决策

本机真实远程缓存中没有 `savfox-1p*`、`exp-savfox*` 或 `exp-5.1*`。本次不恢复这些名称的特殊分支；若未来重新启用，provider 必须在 catalog 中发布完整能力。这是 catalog-only 规则的一部分，不再是待处理代码项。

## 2. 真实数据验证

本机 `~/.savfox/models_cache.json` 是实际远程 catalog 缓存，共 12 个模型：

- 12/12 包含 `shell_type`、`supports_parallel_tool_calls`、`truncation_policy`；
- 11/12 包含非空 `apply_patch_tool_type`；
- 条目还包含 `base_instructions`、`model_messages`、reasoning、context window 等字段。

真实 DeepSeek provider store 证实写入格式是 `id` / `model_slug`，且旧文件可能没有 `models_fetched_at`。读取端已兼容旧 store，新的连接写入 freshness。

本轮没有使用用户凭据重新请求线上 `/models`。现有真实缓存、真实 provider store 以及覆盖网络/缓存/store/offline 的回归测试已经验证客户端路径；带真实账号的登录和第三方线上 smoke test 仍属于发布前人工环境检查，不是未完成的代码修复。

## 3. Arkret 兼容清理

- `events_query` 已全部移除，调用使用 `events_read_outcome`。
- 新 scope 使用 `ak.self.events.read.scan` / `ak.self.events.read.frontier`。
- 已配对账号保存的 `ak.self.events.query.scan` / `ak.self.events.query.frontier` 会按新语义校验和授权，同时保持原字符串上行，避免破坏不可变 pairing commitment。
- 同时配置 query/read 两种拼写会被判定为重复 scope。
- 更早的 `ak.self.events.scan` 仍然拒绝。

对应测试 `query_scope_aliases_preserve_existing_pairing_commitments` 和 `query_and_read_spellings_are_duplicate_scopes` 已通过。

## 4. 验证期间发现并解决的问题

此前记录的“既有失败”均已重新定位或修复：

- Core 全量测试极慢：根因是每个临时 Savfox home 都尝试联网安装 system skills。测试专用 `SkillsManager::new_for_tests` 跳过网络安装，生产路径不变。
- Core 测试受宿主 `~/.agents/skills` 污染：测试只保留临时 Savfox home 下的 skills。
- Core 测试隐式依赖已删除的内置默认模型：测试 session 现在显式使用中性 `test-model`。
- Windows sandbox 断言与当前默认 restricted-token 行为不一致：已更新平台预期。
- 配置 schema fixture 过期：已重新生成 `crates/core/config.schema.json`。
- TUI personality 测试：显式注入 catalog 后通过；在非交互重定向环境运行，避免终端探测干扰。
- App-server OAuth 回调：兼容 URL 中编码后的 `localhost` 和 `127.0.0.1`。
- App-server personality、API SSE、gateway metadata 的既有失败均已精确复跑通过。
- 严格 Clippy 报告的 Rust 1.97 lint 已修复，包括两个 `useless_borrows_in_formatting`。

## 5. 最终验证

已通过：

```text
cargo test -p savfox-core --lib
# 1108 passed; 0 failed; 3 ignored; 19.48s

cargo test -p savfox-core --lib providers::manager::manager::tests -- --test-threads=1
# 14 passed

cargo test -p savfox-core --lib agent::role::tests -- --nocapture
# 3 passed

cargo test -p savfox-core --test all model_tools -- --exact
# 1 passed

cargo test -p savfox-channels --features arkret --lib query_ -- --nocapture
# 2 passed

cargo test -p savfox-tui --lib chat_screen::tests::user_turn_includes_personality_from_config -- --exact --nocapture
# 1 passed; 820 filtered out; 2.13s

cargo test -p savfox-tui --lib chat_screen::tests::reasoning_popup_uses_active_catalog_metadata_for_selected_slug -- --exact --nocapture
# 1 passed; 820 filtered out; 2.11s

cargo test -p savfox-app-server --test all turn_start_accepts_personality_override_v2 -- --exact
cargo test -p savfox-app-server --test all turn_start_change_personality_mid_session_v2 -- --exact
cargo test -p savfox-app-server --test all login_account_chatgpt_start_can_be_cancelled -- --exact
# personality filter final rerun: 3 passed

cargo test -p savfox-api-client --lib sse::chat::tests::emits_tool_calls_even_when_content_and_reasoning_present -- --exact --nocapture
cargo test -p savfox-gateway-server --lib chat_session::tests::persist_chat_session_metadata_updates_shared_entry_fields -- --exact

cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

严格 Clippy 只剩第三方 `proc-macro-error2 v2.0.1` 的 future-incompat 提示，不是 warning 失败。

## 6. Role 配置层迁移（已完成）

`crates/core/src/agent/role.rs` 已删除 `AgentProfile` 硬编码 match，迁移为嵌入式 TOML role 文件与 `ConfigLayerStack`：

- `default`、`worker`、`explorer`、`orchestrator` 的描述和配置来自 `crates/core/src/agent/roles/*.toml`；
- role 配置作为后置 `SessionFlags` layer 插入，能覆盖普通 session flags，但不能绕过更高优先级的 system/MDM 约束；
- 使用标准 `Config::load_config_with_layer_stack` 重建配置；
- role 未设置 model、provider、reasoning、instructions、sandbox 等字段时，保留调用方当前运行时选择；
- explorer 的 `medium` reasoning effort 来自 `explorer.toml`，不再来自 Rust match；
- orchestrator 的 base instructions 已从独立 Markdown 模板迁入 `orchestrator.toml`；
- 原本未进入最终配置的 `ConfigToml.instructions` 现在正确映射为 base instructions。

角色文件解析、schema 描述、继承模型以及 orchestrator instructions 均有单元测试覆盖。

## 7. Catalog-only 测试审计（已完成）

受本次改动影响的工具、personality、truncation、reasoning 与 picker 测试已经改为显式构造 `ModelInfo` 能力，不再通过 slug 获得能力。

审计还发现并修复了 TUI 的一个生产路径：reasoning 弹窗此前调用 `provider_model_info`，会为已选择模型重新制造 fallback 元数据。现在 `ModelsManager::try_get_catalog_model_info` 只返回活动内存 catalog 中的条目，未命中时沿用 picker preset，不生成 fallback；相关测试显式注入 default/xhigh reasoning 能力。

保留的模型名判断仅用于模型迁移文案、rate-limit 产品提示或普通字符串展示，不参与 shell、apply-patch、personality、reasoning、并行调用或 truncation 能力推断。

后续约束仍然是：如果模型需要某项能力，fixture 必须显式提供对应字段，不能重新写成“某个 slug 应当自动拥有某种能力”。
