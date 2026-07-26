# Arkret 配对窗口交互改进报告

## 1. 背景

Arkret Agent 配置目前把协议输入、异步配对、普通配置保存和解除绑定放在同一个长表单中。实际配对在 Inkson 批准后只更新浏览器内存中的表单值，仍需用户滚动到底部点击 `Save` 才会持久化，容易产生“已经配对成功”的错误完成感。

本改进把 Arkret Agent 配对定义为一个完整事务：

```text
输入配对链接
  -> 解析链接
  -> 生成本地运行时密钥
  -> 请求 Inkson 批准
  -> 等待批准
  -> 保存频道配置
  -> 重载 Arkret 运行时
  -> 显示已配对，并单独展示真实运行态
```

只有保存成功后，界面才能进入已配对终态；是否 Connected 继续由真实监听器状态决定。

## 2. 现有问题与根因

### 2.1 Bootstrap JSON 暴露协议细节

- 配对链接解析后被格式化为多行 JSON 并回填到六行文本框。
- 用户需要的是配对链接、校验码和最终连接状态，而不是 CKP-0008 内部结构。
- 多行 JSON 显著增加弹窗高度并把主操作挤出视口。

### 2.2 配对码视觉层级不足

- 配对码使用普通字段提示样式，字号和对比度不足。
- 配对码没有分组、复制动作和明确的跨端核对指令。
- 等待批准状态与配对码分离，用户需要在信息之间来回寻找。

### 2.3 配对完成与保存边界不一致

- Inkson 批准后，前端只把 `authorizedEventRef` 写入内存表单。
- 通用 `Save` 位于长弹窗底部，用户容易遗漏。
- “Agent is active”出现在真正持久化之前，终态语义不准确。

### 2.4 Unbind 缺少上下文和确认

- `Unbind agent` 直接混在主配对流程中。
- 操作会撤销 KeyPackage、清理本地状态并清除绑定，但当前没有二次确认。
- “Unbind”是协议语言，不是面向用户的任务语言。

### 2.5 长弹窗操作不可见

- Channel 配置弹窗整体滚动，Header 和操作区都会离开视口。
- Save、Cancel、Test 等任务动作缺少稳定位置。

### 2.6 Channel 类型与实例身份混用

- `Add Channel` 曾使用 Channel 类型（例如 `arkret`）读取配置，因而会把某个现有实例的绑定状态恢复到新增表单。
- 配置列表曾以类型为 HashMap 键，同类型的后一个实例会覆盖前一个实例。
- Settings、Save、Test、Enable/Disable 和 Disconnect 的部分链路曾按类型操作，而不是按实例 ID 操作。
- 这会造成新增窗口显示旧实例为 Connected，也会让同类型多实例彼此串状态或误操作。

## 3. 目标交互

### 3.1 未配对状态

- 正常界面只显示单行 `Inkson pairing link`。
- 解析后的 Bootstrap 保存在隐藏表单状态中，不回显原始 JSON。
- 主动作使用 `Start pairing`。
- 配对开始后锁定重复提交，并依次显示解析、生成密钥、等待批准和保存状态。

### 3.2 等待批准状态

- 使用独立校验码卡片。
- 配对码采用等宽大号字体和四位分组显示。
- 提供复制动作，复制内容保持原始无空格值。
- 显示“在 Inkson 中核对相同代码”的明确说明。

### 3.3 完成和失败状态

- Inkson 批准后自动进入 `Finalizing and saving`。
- 复用 Channel 通用保存管线保存完整表单快照。
- 保存成功后刷新频道状态并进入已配对摘要。
- 保存失败时保留批准结果，显示 `Retry saving`，不重新发起 Inkson 批准。
- 只有本地保存成功后才能显示 `Agent paired and channel saved`。

### 3.4 已配对状态

- 隐藏配对链接、校验码和开始配对动作。
- 显示 Agent、Arkret Server 和 Paired 状态摘要；真实连接状态在实例卡片中独立显示。
- 长 DID 使用截断布局，保留完整复制能力。

### 3.5 断开状态

- 使用 `Disconnect agent…` 替代 `Unbind agent`。
- 在已连接摘要内进入确认状态，不与主配对动作并列。
- 确认内容明确说明撤销 KeyPackage、停止连接和清理本地配对状态。
- 确认按钮使用危险样式和动作化文案 `Disconnect agent`。
- 成功后清理前端配对数据并返回未配对状态。

## 4. 全站统一原则

1. 展示用户任务数据，不默认展示协议内部数据。
2. 一个任务只有一个完成边界；成功提示必须对应已持久化终态。
3. 长弹窗的 Header 和 Footer 固定可见，只有内容区滚动。
4. 每个任务区只保留一个主动作；测试、复制等使用次级动作。
5. 异步动作必须提供 idle、working、waiting、success、error 和 retry 状态。
6. 危险操作与常规操作分层，说明具体影响并要求明确确认。
7. 状态使用图标、文本和颜色共同表达，不只依赖颜色。
8. 调试数据和高级字段使用渐进披露，不能阻塞普通任务。
9. Channel 类型只用于选择配置模板；读取、修改、测试、启停、解绑和删除必须使用不可变的实例 ID。
10. `Add Channel` 永远创建新表单状态，不允许隐式恢复同类型已有实例。
11. `Paired` 只表示授权资料已持久化；只有真实监听器进入订阅/派发阶段才能显示 Connected。

## 5. 实施任务细则

### P0：完成语义和数据正确性

- [x] 提取 Channel 表单统一持久化函数，供普通 Save 和 Arkret 配对完成复用。
- [x] 为 Arkret 配对增加显式操作状态。
- [x] Inkson 批准后自动保存完整 Channel 配置并刷新运行时。
- [x] 保存失败时保留授权引用并提供 `Retry saving`。
- [x] 配对进行中禁止重复提交。
- [x] 成功文案只在持久化成功后出现。

### P0：多实例隔离

- [x] 新增流程不再调用 `channels.config.get`，不会恢复任何现有实例。
- [x] 新增实例自动选择未占用的默认名称和 ID，避免覆盖同名实例。
- [x] 配置列表保留并渲染全部同类型实例，不再按类型互相覆盖。
- [x] Settings 使用准确实例 ID 读取配置。
- [x] 编辑时保持原实例 ID；修改名称不会创建或覆盖另一实例。
- [x] Save、Test、Enable/Disable、Disconnect 和 Delete 都携带准确实例 ID。
- [x] `channels.status` 提供按实例 ID 的状态视图，Channel 卡片不再共享类型级 Connected。
- [x] 实例级 Login/Logout 只启动或停止目标实例，不影响同类型其他实例。
- [x] 后端保存时永久保留已有实例 ID；改名不会迁移 Arkret 运行时或加密状态归属。
- [x] Arkret 重绑校验只检查目标实例，允许不同实例绑定不同 Agent。
- [x] Arkret 保存后只重启目标实例。

### P1：Arkret 信息架构

- [x] 将 Bootstrap 输入改为单行配对链接。
- [x] 分离用户输入的配对链接与隐藏的已解析 Bootstrap JSON。
- [x] 增加大号、分组、可复制的配对码卡片。
- [x] 已绑定时隐藏配对表单，显示连接摘要。
- [x] 从普通流程移除无绑定状态下的 Unbind 占位内容。
- [x] 将 `Paired` 与真实运行态分离，展示 Starting、Listening、Retrying、Stopped 和最后错误。

### P1：危险操作

- [x] 将操作重命名为 `Disconnect agent…`。
- [x] 增加具体后果说明和二次确认。
- [x] 断开请求携带准确的已保存 Channel ID。
- [x] 成功后清理前端 Bootstrap、密钥引用、验证方法和授权引用。

### P1：Channel 弹窗

- [x] 固定 Channel 配置弹窗 Header。
- [x] 固定配置操作区，保证 Save/Test/Cancel 始终可见。
- [x] Arkret Agent 未配对时隐藏独立的通用 Save，避免双提交边界。

### P2：质量保障

- [x] 为配对码格式化、绑定状态和重试状态增加单元测试。
- [x] 更新已有 Arkret 可见性和保存测试。
- [x] 运行 `cargo fmt --all -- --check`。
- [x] 运行 `cargo test -p savfox-gateway-dioxus`。
- [x] 运行目标构建或 Clippy 检查。

## 6. 验收标准

1. 配对链接始终使用单行输入，解析后不显示多行 Bootstrap JSON。
2. 1365×768 和移动端视口中，主要操作无需寻找隐藏的底部 Save。
3. Inkson 批准后自动保存；刷新页面后仍显示已配对，并由运行时单独报告是否 Connected。
4. 模拟保存失败时不显示配对成功，并可直接重试保存。
5. 已绑定状态不显示新的配对入口。
6. Disconnect 未确认前不产生副作用，失败时保持已连接状态。
7. 长 Channel 配置弹窗滚动时，Header 和操作区保持可见。
8. 所有状态都具有文本含义，复制、等待、成功和危险操作可通过键盘使用。
9. 已有两个 Arkret 实例时，打开 `Add Channel → Arkret` 必须显示全新未配对表单。
10. 同类型多个实例分别显示、编辑、测试和启停；任何动作不得选择同类型的其他实例。
11. 已配对但监听器认证失败时显示 `Needs attention`，不得显示 Active 或 Connected。
12. Agent 配置只接受 canonical flat DTO、keyring runtime key、canonical
    millisecond timestamp 和 registry 中存在的显式 `requestedScope`；旧字段和旧
    action 不迁移。
13. `agent_requested_scope_commitment_invalid` 显示为
    `Re-provision required` 并停止重试。
14. 平台卡片显示实例计数；一个实例失败不得覆盖另一个 ready 实例。
15. 所有 Arkret outbound 必须携带精确 saved Channel ID，缺失时 fail closed。

## 7. 实施结果

- `cargo check -p savfox-gateway-dioxus`：通过。
- `cargo test -p savfox-gateway-dioxus`：50 项测试全部通过。
- `cargo test -p savfox-config channel_store`：10 项 Channel Store 测试全部通过。
- `cargo test -p savfox-channels --features arkret --all-targets`：195 项库测试与
  4 项 Arkret guard 测试全部通过。
- `cargo test -p savfox-gateway-server --features arkret --all-targets`：全部通过
  （412 项库测试及各集成测试目标）。
- `cargo clippy -p savfox-gateway-dioxus --all-targets --no-deps -- -D warnings`：通过。
- `cargo clippy -p savfox-channels --features arkret --lib --no-deps -- -D warnings`：通过。
- `cargo clippy -p savfox-gateway-server --features arkret --lib --no-deps -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- `scripts/build-web.ps1`：Web 客户端构建成功，并同步到 `gateway-dioxus/dist` 与 `gateway-server/static`。
- Dioxus CLI 仍报告既有的版本提示（`dx 0.8.0-alpha.0` 与 `dioxus 0.7.4`），但构建和打包成功。
