# AI 营销工坊设计方案（客户端原生 Playground）

> v3（2026-10-12）：按 Codex 代码级评审修订。相对 v2 的关键变化：①补后端流任务注册表与身份取消链路（H1，原"复用 after_write 防线"不成立）；②补缓存未热初始化路径（M1）；③选择记忆扩展为独立数据结构而非仅加 slot（M2）；④流式 HTTP 走独立客户端、不继承 20s 总超时（M3）；⑤模型列表改用广场既有 kind 数据（M4）；⑥请求生命周期改为 invoke 等待终态 + 增量事件（M5）；⑦错误契约改为稳定 code + 本地化文案（M6）；⑧补上下文/并发预算与余额刷新时机（M7）；⑨修正影响面与守卫编号（M8/L3/L4）。

## 1. 目标与范围

在客户端新增「AI 营销工坊」Tab：用户不离开客户端即可完成营销内容生产——文本对话（P1）→ 图片生成/编辑（P2）→ 视频生成（P3）。原生实现，不内嵌 WebView。

一句话背景：Web 端已有 Playground（we2ai.com/playground），但 Web 需单独登录并手动填 API Key，客户端已有登录态与 Key 管理体系，原生集成成本可控、体验统一。

## 2. 事实依据

标注【已验证】= Codex 已对源码核实；标注【待联调】= 静态分析推定，P1 联调时以真实网关响应为准。

| 事实 | 来源 | 状态 |
|---|---|---|
| Web Playground 调用 chat（SSE）、`/v1/messages`、`/v1/images/generations`、`/v1/images/edits`、`/v1/videos`（含轮询）、`/v1/models`；参数 `temperature`/`top_p`/`max_tokens` 各带 enabled 开关，`stream:true` | 站点 chunk `PlaygroundView-xgoQHC48.js` 字符串分析（静态证据，服务端实际行为未实测） | 【待联调】 |
| `/v1/*` 请求用 `Authorization: Bearer ${apiKey}`（API Key），客户端管理 API 用登录 access token——两条鉴权链路不同 | 同上 + `api.rs:515` | 【待联调】（"session token 是否被 /v1/* 拒绝"未验证，方案不依赖该排他性：一律用 API Key） |
| 网关 `list_keys` API 按 `key: String` 契约返回 Key 完整明文；模型广场 `KeyCache`（Rust 内存）分页拉取后持有 active/quota_exhausted Key 明文；`apply` 经 `secret_for` 取用，明文不跨 IPC | `api.rs:728`、`keys.rs:641`、`commands_apply.rs:182`、`keys.rs:467`（仅 `masked_key` 出 IPC） | 【已验证】 |
| 缓存**不是登录即热**：登录命令不拉 Key，首次 `we2ai_list_keys`（广场页挂载）才填充；`invalidate()` 在创建/编辑/删除成功后清缓存 | `commands_auth.rs:278`、`ModelSquarePage.tsx:373`、`keys.rs:637`、`key_manage.rs:426` | 【已验证】 |
| `secret_for` 已是 `pub fn`，兄弟模块可直接调用；只按 ID+身份查找，**不校验 active** | `keys.rs:224` | 【已验证】 |
| 模型广场选择记忆 `KeySelectionFile` 为私有结构，slot 是 `region:user_id`（无用途维度）；`clear_selection_if` 只清广场 slot；列表返回 `selected_key_id`/`remembered_key_id` 均属广场；选择规则只查 ID 存在、不查 active | `keys.rs:509/522/554/258/501` | 【已验证】 |
| 广场模型数据来自 `/api/v1/desktop/keys/{key_id}/models`，含 `kind` 字段（B1 DTO）；前端 `modelKind.ts` 判定 `!kind \|\| kind === "text"`，且缺 kind 时放行 | `api.rs:612/842`、`modelKind.ts:8` | 【已验证】 |
| 现有共享 reqwest 客户端 20s 总超时、`response.text()` 整包读取；reqwest `stream` feature、tokio/futures 已启用；无 dashmap 依赖 | `api.rs:295/340`、`Cargo.toml:55/60` | 【已验证】 |
| `sanitize_message_for_display` 只处理 Codex 模型目录路径与品牌残留，**不是密钥脱敏器** | `apply.rs:543` | 【已验证】 |
| IPC 白名单与 Rust gate 按 `we2ai_` 前缀放行；Tauri capability 已含 `core:event:default`（listen/emit 可用，无需新增事件权限）；Rust `app.emit` 已有先例 | `ipcWhitelist.ts:28`、`mode.rs:249/381`、`capabilities/default.json:7`、`key_manage.rs:628` | 【已验证】 |
| 余额刷新目前触发条件：充值 Tab、身份变化、窗口聚焦；无 chat 完成触发 | `We2aiShell.tsx:561/615/642` | 【已验证】 |
| SecretStore（`com.we2ai.desktop.login`）模式已有（本方案未采用，见第 4 节） | `session.rs:609` | 【已验证】 |

## 3. 总体架构

```
┌─ 前端（新 Tab "studio"）────────────────────────┐
│  PlaygroundPage.tsx                              │
│  · 会话流渲染 / 参数面板 / 场景预设 / 停止按钮     │
│  · 只发命令、只听事件，不持有 Key 明文             │
└──────┬──────────────────────────▲───────────────┘
       │ invoke（等终态返回）       │ emit 事件（流式增量）
┌──────▼──────────────────────────┴───────────────┐
│  Rust（新增 playground.rs，key_manage 同层）       │
│  · 流任务注册表：request_id → { SessionIdentity,  │
│    abort 句柄 }；身份失效通知进入 select! 取消分支 │
│  · Key 明文：复用广场 KeyCache                    │
│    （keys.rs::secret_for 按 key_id 取用，         │
│     后端独立校验 active，不新增明文通道）           │
│  · 工坊选择记忆：独立 slot 数据结构（第 4 节）      │
│  · 独立流式 HTTP 客户端（第 5.3 节超时策略）        │
│  · SSE 逐行解析 → emit 增量 → invoke 返回终态      │
└──────┬──────────────────────────────────────────┘
       │ HTTPS（Bearer = 用户选中的已有 Key）
       ▼
  we2ai 网关 /v1/*（OpenAI 兼容）【端点契约 P1 联调验证】
```

## 4. Key 策略（用户已定：复用已有 Key + 用户可选）

问题：`/v1/*` 只认 API Key；管理列表 IPC 不回明文；让用户手填 Key 则明文必经 IPC，破坏边界。

**决策：复用广场 KeyCache 明文（`secret_for` 已是 pub，无需改可见性），选择器 UI + 工坊独立选择记忆。**

| 环节 | 行为 |
|---|---|
| 缓存预热 | 进入工坊 Tab 先调 `we2ai_list_keys`（该命令会拉取并填充 KeyCache）；加载完成前选择器/发送/模型列表全部禁用。处理 `KEY_LIST_SUPERSEDED`（忽略旧请求）与拉取失败（本地化错误 + 重试按钮，**不等同于 Key 被删除**） |
| Key 列表 | 复用既有 `we2ai_list_keys`（`KeyView` 含 status），选择器只列 `active`（`quota_exhausted` 展示但禁用，tooltip 指向充值） |
| 默认选中 | 默认 = 广场 `selected_key_id`；用户在工坊另选后记住工坊自己的选择 |
| 选择记忆 | **扩展 `KeySelectionFile` 数据结构而非仅加 slot**：`KeySelectionData` 增加工坊维度（slot 键改为 `region:user_id:用途` 或并列表），新增 `pub(super)` 读写接口；`clear_selection_if` 改为同时检查并清理广场与工坊两份记忆；`KeyListView` 增加工坊选中字段（`studioSelectedKeyId`，命名以实现为准），重启后前端由此恢复。**回退规则补强**：记忆的 Key 不在列表或非 active → 回退广场选中项；广场选中项也非 active → 不预选，置空并提示选择（现有选择规则只查 ID 存在不查 active，工坊侧必须加校验） |
| 调用时取明文 | `chat`/`models` 入参带 `key_id`，Rust 用 `secret_for(current_identity, key_id)` 取明文；**后端独立校验该 Key 在当前列表且 `active`**（不依赖前端选择器），无缓存/非 active → 稳定错误 code |
| Key 失效（401/403/已删除） | 稳定 code + 文案「所选 Key 已失效或已被删除，请重新选择」+ 刷新列表；不自动建 Key |
| 额度/限流 | 配额类（402/配额耗尽）→「Key 额度已用完」；429 →「请求过于频繁」（**两个语义分开，不混用**） |

不做的：不自动创建专用 Key；不让用户手填 Key（明文必经 IPC）。

## 5. P1：文本对话

### 5.1 IPC 面（`we2ai_` 前缀自动过既有 gate，无需追加两份白名单）

| 命令 | 输入 | 输出/行为 |
|---|---|---|
| `we2ai_playground_choose_key` | `{ key_id }` | 校验列表内且 active，记入工坊选择记忆；返回含 `studioSelectedKeyId` 的视图 |
| `we2ai_playground_models` | `{ key_id }` | **不调 `/v1/models`**：复用广场既有 key 维度模型数据（`/api/v1/desktop/keys/{key_id}/models`，带 `kind`），过滤 `kind === "text"`；无 kind 的模型不展示（`modelKind.ts` 缺省放行的口径在工坊侧收紧）。`/v1/models` 端点待联调验证后再评估是否补充 |
| `we2ai_playground_chat` | `{ request_id, key_id, model, messages, system?, temperature?, top_p?, max_tokens? }` | **invoke 阻塞至终态**：成功返回 `{ finish_reason, usage? }`；失败返回 `{ code, message }`；增量 delta 走事件（见下表）。`usage` 可能缺失（网关流式 usage 契约待联调），缺失时前端不显示 token 数，不视为错误 |
| `we2ai_playground_cancel` | `{ request_id }` | abort 该请求；校验任务归属（存在且属当前会话才取消）；幂等 |

**请求生命周期契约**（M5）：
- 前端先建立事件监听再 invoke（`listen` 返回 unlisten 前不发送）；
- Rust 校验输入、登记任务表成功后才真正发起 HTTP，失败直接以错误终态返回，不 emit；
- **每个请求恰好一个终态**：invoke resolve/reject 即终态（`cancelled` 以稳定 code 出现在错误终态）；终态后从任务表移除句柄，后续迟到的网关数据丢弃；
- 同一 `request_id` 重复 invoke → 立即错误；并发上限 1（同时只允许一个活跃流，前端发送按钮置灰即可保证，后端再兜底拒绝）；
- 新会话/切换场景预设/切换 Key → 前端先 cancel 再重建会话。
- 事件：`we2ai-playground-chunk` `{ request_id, delta }`；错误/完成不再单独 emit 事件（终态由 invoke 承载，避免双通道不一致）。

**上下文与并发预算**（M7）：
- Rust 侧硬上限：messages ≤ 50 条、序列化请求体 ≤ 512 KB、`max_tokens` ≤ 8192（下发前 clamp）；超限返回稳定 code，前端提示开启新会话；
- UI 明示「每轮会重新发送历史并计费」，输入区旁显示当前历史条数；
- 终态（成功/失败/取消）后触发一次余额刷新（`We2aiShell` 既有刷新函数挂 playground 终态回调）；**不承诺逐 chunk 实时扣费展示**。

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

- Key 选择器与广场同款展示（name + masked_key）；切换 Key 开新会话，切换前 cancel 进行中的流。
- 场景预设 = 内置 system prompt 模板（strings.ts 中英内置），点击开新会话。
- 会话历史存前端内存；工坊 Tab `forceMount`（对话非敏感，切走不丢）。
- 顶栏余额 chip 在每轮终态后刷新（见 5.1）。

### 5.3 Rust 实现要点

- **独立流式 HTTP 客户端**（M3）：不继承共享客户端 20s 总超时；新建 `reqwest::Client`（连接超时 10s；无总超时；`tokio::time::timeout` 包裹每次 chunk 等待实现 120s 空闲超时）。复用区域 `base_url()` 拼接与既有代理策略。**chat POST 不自动重放**（非幂等且计费），API Key 的 401 不接入登录 token 续期流程。
- **SSE 解析**：`stream` feature 已启用，逐行读 `data:`，`[DONE]` 终止，`choices[0].delta.content` 增量；不引入 dashmap，任务表用 `std::sync::Mutex<HashMap<...>>`（现有代码风格即如此）。
- **身份取消链路（H1，需新写）**：登出/切换会话身份的路径已有清缓存动作（`commands_auth.rs:403`），但**没有任务取消**。方案：playground 模块持有任务注册表（`request_id → (SessionIdentity, AbortHandle)`）；任务内 `tokio::select!` 监听取消分支 + 每次事件投递前复查 `manager.current_identity() == Some(注册身份)`，不一致 → 中止流并以身份失效错误终态返回。登出/切区域路径新增对 playground 注册表的清理调用（按完整 `SessionIdentity` = region + user_id + generation 匹配）。前端身份键只含区域+用户（`api.ts:105`），身份失效判定以后端为准，前端只消费终态。
- **错误契约（M6）**：所有错误返回 `{ code, message }`——code 是稳定枚举（`key_invalid` / `key_inactive` / `quota_exhausted` / `rate_limited` / `identity_changed` / `cancelled` / `network` / `payload_too_large` / `upstream_error`），message 是本地化文案；**未知上游响应不透传 body**（`sanitize_message_for_display` 不是密钥脱敏器，不能兜底），原始错误只进 log。测试用含 Key 的模拟错误断言明文不出现在 IPC 与日志。

## 6. P2：图片生成/编辑（概要）

- 端点：`/v1/images/generations`（prompt/model/size/n）、`/v1/images/edits`（multipart）。同一 Key 选择器复用，图片走一次性 JSON 响应（非流式），复用 P1 的独立 HTTP 客户端骨架。
- UI：工坊 Tab 内「对话 / 绘图」子模式；结果缩略图网格、另存。
- 另存文件需要 fs 权限：当前 capability 未列 fs，P2 设计时补（评审已确认 P1 无权限阻断）。

## 7. P3：视频生成（概要）

- 端点：`/v1/videos` 提交 → `/v1/videos/{id}` 轮询（5s 间隔、最长 15min）。
- 视频单价高：提交前弹确认（展示预估费用，价格来自广场 pricing 数据）。

## 8. 影响面与同步风险

| 文件 | 类型 | 冲突风险 |
|---|---|---|
| `src/we2ai/PlaygroundPage.tsx`（新增）、`playgroundPrompts.ts`（新增） | fork 独有 | 无 |
| `src/we2ai/{We2aiShell,strings,api}.ts(x)` | 既有 fork 文件内增量 | 低 |
| `src-tauri/src/we2ai/playground.rs`（新增）、`keys.rs`（选择记忆结构扩展 + clear 双份清理）、`commands_auth.rs`（登出路径挂 playground 任务清理，一处调用）、`{mod}.rs` | fork 独有文件内增量 | 低 |
| **`src-tauri/src/lib.rs`** | **上游共享文件**（命令注册 `generate_handler!`），合并时与上游改动同区 | 中：与功能 4/13/19 等既有触点相同，同步时人工确认注册段 |
| `scripts/we2ai/check-guards.sh` 新增 **4.17**（4.16 已被功能 22 占用） | fork 基础设施 | 无 |
| `Cargo.toml`/`Cargo.lock` | 不新增依赖（不用 dashmap，reqwest stream/tokio/futures 已有） | 无 |

不触碰上游其余文件。`lib.rs` 注册段为唯一共享集成点，与既有 fork 功能同级风险。

## 9. 守卫与测试

**守卫 4.17（机械字面量）**：
- `lib.rs` 注册 4 个 `we2ai::playground::we2ai_playground_*` 命令；
- `PlaygroundPage.tsx` 禁止出现 `navigator.clipboard`/`apiKey`/`secret` 字样（明文边界）；
- `keys.rs` 含工坊选择记忆字段/接口标识；
- `strings.ts` 含 `studioTitle`。

**测试**：
- 单测（Rust `#[cfg(test)]`）：SSE 行解析（正常流/`[DONE]`/半包/错误 JSON）；任务注册表（登记/取消/归属校验/终态移除）；错误 code 映射（含 Key 的模拟错误断言明文不出现在 IPC 与日志）；上下文预算 clamp；
- 集成（TS）：`PlaygroundPage` 渲染与场景预设；`We2aiShellPlayground` 白名单断言；缓存未热流程（加载前禁用发送、失败重试）；工坊选择记忆保存/重启读取/仅命中工坊记忆的删除清理；回退规则（记忆非 active → 广场选中 → 置空）；
- H1 验收覆盖（不能只测 UI 清空）：**登出时进行中流被后端取消、同账号重登、切区域、收到任何 chunk 前身份失效**——断言任务表清空、无继续计费、无旧身份事件；
- 回归：`pnpm typecheck`、`cargo check`、既有 `We2aiShell*` 测试。

**验收标准（P1）**：
1. 首次进入工坊（未开过广场）也能完成：列表加载 → 默认选中广场 Key → 发送；另选后重启客户端，工坊记住自己的选择；
2. 选模型发消息，流式逐字渲染，可停止（终态 `cancelled`），usage 缺失时不显示 token 数也不报错；
3. 断网/401/配额/429 各返回对应稳定 code 与本地化文案；401 引导重新选择 Key，且**不触发任何建 Key 调用**；
4. 上下文超限返回 `payload_too_large` 并提示新会话；每轮终态后余额 chip 刷新；
5. 登出/切区域时进行中的流被后端取消（含无 chunk 场景），重启后工坊记忆恢复；
6. `check-guards.sh` 全过。

## 10. 分期

| 期 | 内容 | 依赖 |
|---|---|---|
| P1 | 文本对话（本文 5 节全部）+ 联调验证第 2 节【待联调】项 | 无 |
| P2 | 图片生成/编辑（含 fs 权限设计） | P1 骨架 |
| P3 | 视频生成 | P2 子模式框架 |

每期独立发版；P1 完成后更新 `自定义开发功能列表.md`（功能 27）与 `docs/we2ai/实机验收清单.md`。

## 11. 已知限制与风险

| 风险 | 等级 | 处置 |
|---|---|---|
| 【待联调】项与真实网关不符（SSE 格式、usage 字段、/v1 端点鉴权细节） | 🟡 中 | P1 第一步即做网关联调探针（手动 curl + 临时命令），契约确认后再固化解析层；解析层单测与传输解耦 |
| 流任务注册表与登出/切区域清理是新写链路，漏挂清理点则旧流继续计费 | 🟡 中 | 清理调用收敛到单一函数，登出/切身份两处显式挂接 + 4.x 测试覆盖；事件投递前身份复查兜底 |
| 用户选中的 Key 被删/失效 → 401 | 🟢 低 | 稳定 code + 重新选择引导，不报原始错误、不自动建 Key |
| 选中 Key 额度耗尽/限流 | 🟢 低 | 配额与 429 两个 code 分开；选择器禁用 `quota_exhausted` |
| 明文复用扩大 `KeyCache` 明文消费面 | 🟢 低 | 仍限 Rust 进程内（消费方仅 `commands_apply` 与 playground），不出 IPC；守卫 4.17 禁前端 apiKey 字样 |
| 对话内容含敏感信息，仅存内存不持久化 | 🟢 低 | 写入用户说明；P2 评估可选本地历史开关 |
