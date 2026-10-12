# AI 营销工坊设计方案（客户端原生 Playground）

## 1. 目标与范围

在客户端新增「AI 营销工坊」Tab：用户不离开客户端即可完成营销内容生产——文本对话（P1）→ 图片生成/编辑（P2）→ 视频生成（P3）。原生实现，不内嵌 WebView。

一句话背景：Web 端已有 Playground（we2ai.com/playground），但 Web 需单独登录并手动填 API Key，客户端已有登录态与 Key 管理体系，原生集成成本可控、体验统一。

## 2. 事实依据（已核实，非推测）

| 事实 | 来源 |
|---|---|
| Web Playground 支持 chat（SSE 流式）、`/v1/messages`、`/v1/images/generations`、`/v1/images/edits`、`/v1/videos`（含 `/v1/videos/{id}` 轮询）、`/v1/models` | 站点 chunk `PlaygroundView-xgoQHC48.js` 字符串分析 |
| `/v1/*` 端点只认 API Key：`Authorization: Bearer ${apiKey}`，不认登录 session token | 同上（`t.apiKey`） |
| 请求参数：`temperature`/`top_p`/`max_tokens` 各带 enabled 开关，`stream:true` | 同上 |
| 客户端网关基址 `baseUrl`（如 `https://api.we2ai.com`，按区域）不含 `/v1`，拼接规则已有封装 | `we2ai_gateway_info`、`codeSamples.ts:84` |
| Key 创建返回一次性明文 `plaintext`；管理页列表不含明文（明文边界设计） | `key_manage.rs:431 create_key`、功能 21 |
| **网关 `list_keys` API 返回 Key 完整明文**，模型广场 `KeyCache`（Rust 内存）已持有全部 Key 明文；`apply` 写工具配置即经 `secret_for(identity, key_id)` 取用，明文不跨 IPC | `keys.rs:89`、`keys.rs:224 secret_for`、`keys.rs:467`（仅 `masked_key` 出 IPC） |
| 模型广场已有 Key 选择记忆（`KeySelectionFile`，按身份 slot 持久化），删除 Key 时同步清理 | `keys.rs:533`、`clear_selection_if` |
| SecretStore 持久化模式已有先例（本方案最终未采用，见第 4 节） | 功能 25（`com.we2ai.desktop.login`） |

## 3. 总体架构

```
┌─ 前端（新 Tab "studio"）────────────────────────┐
│  PlaygroundPage.tsx                              │
│  · 会话流渲染 / 参数面板 / 场景预设 / 停止按钮     │
│  · 只发命令、只听事件，不持有 Key 明文             │
└──────┬──────────────────────────▲───────────────┘
       │ invoke                    │ emit 事件（流式 chunk）
┌──────▼──────────────────────────┴───────────────┐
│  Rust（新增 playground.rs，key_manage 同层）       │
│  · Key 明文：复用模型广场 KeyCache                │
│    （keys.rs::secret_for，按 key_id 取用，        │
│     不新增明文通道，不建专用 Key）                 │
│  · 工坊选择记忆：KeySelectionFile 工坊 slot        │
│  · reqwest 流式 POST baseUrl/v1/chat/completions │
│  · SSE 逐行解析 → emit → abort 句柄（取消）        │
└──────┬──────────────────────────────────────────┘
       │ HTTPS（Bearer = 用户选中的已有 Key）
       ▼
  we2ai 网关 /v1/*（OpenAI 兼容）
```

## 4. Key 策略（核心决策，用户已定：复用已有 Key + 用户可选）

问题：`/v1/*` 只认 Key；管理页明文不可回取；让用户手填 Key 则明文必经 IPC，破坏边界。

**决策：直接复用模型广场的 Key 明文缓存（`keys.rs::KeyCache`）——网关 `list_keys` 本就返回完整明文，Rust 已为 `apply` 持有全部明文，工坊走同一条内部取用路径（`secret_for`），明文不新增任何跨 IPC 通道。**

| 环节 | 行为 |
|---|---|
| Key 列表 | 前端复用模型广场既有列表命令（`KeyView`：id/name/masked_key/status），工坊选择器只列 `active`（`quota_exhausted` 展示但禁用，tooltip 指向充值） |
| 默认选中 | 默认 = 模型广场当前选中 Key（`selected_key_id`）；用户在工坊另选后记住工坊自己的选择 |
| 选择记忆 | `KeySelectionFile` 增加工坊 slot（同一持久化模式、同一清理逻辑：Key 删除时 `clear_selection_if` 一并清工坊记忆）；默认值回退语义与广场一致（记忆的 Key 不在列表里 → 回退广场选中项） |
| 调用时取明文 | `we2ai_playground_chat` 入参带 `key_id`，Rust 用 `secret_for(current_identity, key_id)` 取明文发起请求；身份不一致或无缓存 → 本地化错误，不出现明文 |
| Key 失效（401/403/已删除） | 文案「所选 Key 已失效或已被删除，请重新选择」+ 刷新列表；不自动建 Key |
| 额度耗尽 | 429/配额类错误映射为「Key 额度已用完」，tooltip 指向充值 Tab |

不做的：不自动创建专用 Key（首版方案，已废弃——SecretStore 降级链路、重建防堆积等复杂度不再需要）；不让用户手填 Key（明文必经 IPC）。

## 5. P1：文本对话

### 5.1 IPC 面（全部走既有 WE2AI gate + 白名单）

| 命令 | 输入 | 输出/行为 |
|---|---|---|
| `we2ai_playground_choose_key` | `{ key_id }` | 记入工坊选择记忆（`KeySelectionFile` 工坊 slot）；校验该 Key 在当前身份列表内且 `active` |
| `we2ai_playground_models` | — | GET `baseUrl/v1/models`（Bearer = 工坊选中 Key 的明文，Rust `secret_for` 取用）→ 过滤 chat 模型列表 |
| `we2ai_playground_chat` | `{ request_id, key_id, model, messages, system?, temperature?, top_p?, max_tokens? }` | 立即返回 `ack`；明文由 Rust 按 `key_id` 从 `KeyCache` 取（第 4 节）；流式结果走事件；参数语义与 Web 对齐（undefined = 不下发该字段） |
| `we2ai_playground_cancel` | `{ request_id }` | abort 对应流；幂等 |

Key 列表本身复用模型广场既有命令（不新增列表命令）；`we2ai_playground_choose_key` 只管工坊自己的记忆 slot。

| 事件（Rust → 前端） | 载荷 |
|---|---|
| `we2ai-playground-chunk` | `{ request_id, delta }`（content 增量） |
| `we2ai-playground-done` | `{ request_id, finish_reason, usage? }` |
| `we2ai-playground-error` | `{ request_id, message }`（脱敏后，同 `sanitize_message_for_display` 原则） |

`request_id` 由前端生成 UUID，事件全部携带，允许多请求并存与精确取消。

### 5.2 UI（PlaygroundPage.tsx，neo-brutalist，沿用 we2ai-theme.css）

```
┌ Tab: 模型广场 | AI 营销工坊 | Key 管理 | 充值 | 设置 ┐
│ ┌──────────────┐ ┌──────────────────────────────┐ │
│ │ Key 选择器     │ │  对话流（用户/助手气泡，Markdown）│ │
│ │ (masked_key,  │ │  底部输入框 + 发送/停止          │ │
│ │  默认=广场选中)│ │  流式打字中显示 ▊ 光标           │ │
│ ├──────────────┤ └──────────────────────────────┘ │
│ │ 场景预设卡片   │ ┌ 参数面板（可折叠）─────────────┐ │
│ │ ·产品文案     │ │ 模型选择 / temperature / top_p │ │
│ │ ·社媒帖子     │ │ / max_tokens（各带启用开关）    │ │
│ │ ·营销邮件     │ └────────────────────────────────┘ │
│ │ ·卖点提炼     │                                    │
│ │ ·多语言本地化  │                                    │
│ └──────────────┘                                    │
└──────────────────────────────────────────────────────┘
```

- Key 选择器：与模型广场同款展示（name + masked_key），复用既有列表命令；切换 Key 开新会话（避免跨 Key 计费混淆），切换前有进行中的流则先取消。
- 场景预设 = 内置 system prompt 模板（strings.ts 中英内置，不做配置面），点击即以该 system 开新会话。
- 会话历史存前端内存；工坊 Tab `forceMount`（对话非敏感，切走不丢；与 Key 管理 Tab 相反）。
- 顶栏余额 chip（功能 20）实时反映扣费；`done` 事件的 `usage` 在气泡尾部展示 token 数。
- 历史消息随会话发送（多轮），提供「新会话」清空。

### 5.3 Rust SSE 解析要点

- `stream:true`，逐行读 `data:` 行，`[DONE]` 终止；`choices[0].delta.content` 增量。
- 网关返回非 200：读 body 一次，按 HTTP 状态映射本地化错误（401 → Key 失效流程；429 → 限流文案）。
- 超时：连接超时 10s；流空闲超时 120s（防止挂死）。
- 取消：`tokio::select!` 监听取消信号；`CancelHandle` 用 `DashMap<request_id, AbortHandle>`。

## 6. P2：图片生成/编辑（概要）

- 端点：`/v1/images/generations`（prompt/model/size/n）、`/v1/images/edits`（multipart，原图 + mask + prompt）。
- UI：工坊 Tab 内切换「对话 / 绘图」子模式；结果缩略图网格，点击看大图、另存（Tauri dialog + fs）。
- 同一 Key 选择器复用（绘图子模式共用选中 Key），无新增 Key 逻辑。图片走一次性 JSON 响应（非流式），复用 `we2ai_playground_chat` 的请求骨架。

## 7. P3：视频生成（概要）

- 端点：`/v1/videos` 提交 → `/v1/videos/{id}` 轮询（任务模型，非流式）。
- UI：子模式「视频」；提交后轮询间隔 5s、最长等待 15min；完成后下载 mp4 并另存。
- 视频单价高：提交前弹确认（展示预估费用，价格来自模型广场 pricing 数据）。

## 8. 影响面与同步风险

| 文件 | 类型 | 冲突风险 |
|---|---|---|
| `src/we2ai/PlaygroundPage.tsx`（新增）、`playgroundPrompts.ts`（新增） | fork 独有 | 无 |
| `src/we2ai/{We2aiShell,strings,api}.ts(x)` | 既有 fork 文件内增量 | 低：Shell 加一个 TabsTrigger/TabContent；strings/api 追加段落 |
| `src-tauri/src/we2ai/keys.rs`（`KeySelectionFile` 增工坊 slot、`secret_for` 暴露给 playground 模块） | 既有 fork 文件内增量 | 低：上游不触碰 keys.rs；改动为追加 slot 与一处调用 |
| `src-tauri/src/we2ai/playground.rs`（新增）、`{mod,lib}.rs` 注册 | Rust 侧同上 | 低 |
| `src/we2ai/ipcWhitelist.ts` + Rust gate（`mode.rs`） | 追加命令/事件 | 低 |
| `scripts/we2ai/check-guards.sh` 新增 4.16 | fork 基础设施 | 无 |

无上游文件触碰，属 fork 独有功能带，与功能 9–21 同级。

## 9. 守卫与测试

**守卫 4.16（机械字面量）**：
- `lib.rs` 注册 4 个 `we2ai::playground::we2ai_playground_*` 命令；
- 事件名 `we2ai-playground-{chunk,done,error}` Rust/TS 两侧一致；
- `PlaygroundPage.tsx` 禁止出现 `navigator.clipboard`/`apiKey`/`secret` 字样（明文边界：明文只经 Rust `secret_for`）；
- `keys.rs` 的 `KeySelectionFile` 工坊 slot 字段存在（选择记忆不被同步静默还原）；
- `strings.ts` 含 `studioTitle`。

**测试**：
- 单测：SSE 行解析（正常流/`[DONE]`/半包/错误 JSON，Rust `#[cfg(test)]`）；
- 集成：`PlaygroundPage` 渲染与场景预设、`We2aiShellPlayground` 白名单断言（全部 invoke 落白名单）、401 → 重建 Key 流程（mock）；
- 回归：`pnpm typecheck`、`cargo check`、既有 `We2aiShell*` 测试。

**验收标准（P1）**：
1. 进入 Tab，Key 选择器默认选中模型广场当前选中的 Key；另选后重启客户端，工坊记住自己的选择；
2. 选模型发消息，流式逐字渲染，可停止，token 用量展示；
3. 断网/401/额度耗尽均有本地化提示，401 引导重新选择 Key；额度耗尽的 Key 在选择器中禁用；
4. 切走 Tab 再切回，会话不丢；切换 Key / 登出 / 切区域后明文缓存清空（既有防线），进行中的流被取消且不串身份；
5. `check-guards.sh` 全过。

## 10. 分期

| 期 | 内容 | 依赖 |
|---|---|---|
| P1 | 文本对话（本文 5 节全部） | 无 |
| P2 | 图片生成/编辑 | P1 的 Key 供给 + 请求骨架复用 |
| P3 | 视频生成 | P2 的子模式框架 |

每期独立发版；P1 完成后更新 `自定义开发功能列表.md`（功能 27）与 `docs/we2ai/实机验收清单.md`。

## 11. 已知限制与风险

| 风险 | 等级 | 处置 |
|---|---|---|
| 用户选中的 Key 被删/失效 → 调用 401 | 🟢 低 | 401/403 映射为「Key 已失效，请重新选择」+ 刷新列表，不报原始错误 |
| 选中 Key 额度耗尽 → 429/配额错误 | 🟢 低 | 映射「额度已用完」文案；选择器中对 `quota_exhausted` Key 禁用 |
| 流式长连接期间用户登出/切区域 → 事件串身份 | 🟢 低 | 事件与缓存均按 SessionIdentity 校验（复用 `after_write` 同款防线），身份变更即中断流并清 UI；`KeyCache` 本身已有身份不一致即清空的防线（`secret_for`） |
| 明文复用扩大了 `KeyCache` 明文的消费面 | 🟢 低 | 仍限于 Rust 进程内（`secret_for` 只被 `commands_apply` 与 playground 调用），不出 IPC；守卫 4.16 禁止前端出现 apiKey 字样 |
| 模型 `/v1/models` 列表含非 chat 模型 | 🟢 低 | P1 按 endpoint 类型过滤；与模型广场 kind 判定（`modelKind.ts`）口径对齐 |
| 对话内容含用户业务敏感信息，仅存内存不持久化 | 🟢 低 | 已知限制写用户说明；P2 评估可选本地历史开关 |
