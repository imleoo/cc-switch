# AI 营销工坊设计方案（客户端原生 Playground）

> v4（2026-10-14）：P2 图片、P3 视频落地，三类模板按机械制造与出口企业重做。相对 v3：①图片/视频接口按官方文档定稿（第 6、7 节），Gemini 图片用 `x-goog-api-key`；②视频费用预估改为文案提示——媒体模型价格服务端一律省略；③新增第 12 节行业模板，借鉴 video-miniAPP 的行业画像与五镜分镜；④新增守卫 4.19；⑤联调清单见第 13 节。
>
> v3（2026-10-12）：按 Codex 代码级评审修订。相对 v2 的关键变化：①补后端流任务注册表与身份取消链路（H1，原"复用 after_write 防线"不成立）；②补缓存未热初始化路径（M1）；③选择记忆扩展为独立数据结构而非仅加 slot（M2）；④流式 HTTP 走独立客户端、不继承 20s 总超时（M3）；⑤模型列表改用广场既有 kind 数据（M4）；⑥请求生命周期改为 invoke 等待终态 + 增量事件（M5）；⑦错误契约改为稳定 code + 本地化文案（M6）；⑧补上下文/并发预算与余额刷新时机（M7）；⑨修正影响面与守卫编号（M8/L3/L4）。

## 1. 目标与范围

在客户端新增「AI 营销工坊」Tab：用户不离开客户端即可完成营销内容生产——文本对话（P1）→ 图片生成/编辑（P2）→ 视频生成（P3）。原生实现，不内嵌 WebView。目标用户是机械制造与出口企业的外贸、营销人员，模板按这个行业设计（第 12 节）。

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
| 图片/视频接口形态（端点、请求字段、Gemini 用 `x-goog-api-key`、Grok 视频 `duration/aspect_ratio/resolution/image`、Seedance `content[]/ratio/generate_audio`、Seedance 状态 `queued→running→succeeded/failed`） | we2ai.com/docs（2026-10-14 读取） | 【文档，字段未实测】 |
| 网关路由存在：`/v1/images/{generations,edits}`、`/v1/videos`、`/v1/videos/{id}`、`/content`、`/v1beta/models/{m}:generateContent` 未授权均返回 401，不存在的路由返回 404 | 2026-10-14 未授权探测 `api.we2ai.com` | 【已验证】 |
| **Grok 图片实测**（`grok-imagine-image-2.0`）：`POST /v1/images/generations` 带 `response_format:"b64_json"` 返回 `data[].b64_json` + `mime_type:"image/jpeg"`（字节头 `ffd8ff`）；`aspect_ratio` 生效（1:1→1024×1024，16:9→1280×720，4:3→1152×864，3:4→864×1152，9:16→720×1280），`size` 被忽略；`n=2` 返回 2 张；非法画幅返回 422（不列可选值）。同一模型可直接走 `POST /v1/images/edits`（multipart：`model/prompt/response_format/aspect_ratio/image`），`grok-imagine-edit` 也可走生成接口，两者可互换。单张约 0.04 美元（生成）/ 0.06–0.07 美元（编辑） | 2026-10-14 带 Key 实测 `api.we2ai.com` | 【已验证】 |
| **GPT 图片实测**（`gpt-image-2`）：生成 1024×1024 与 `n=2` 1536×1024 均返回 `data[].b64_json`（PNG）；`POST /v1/images/edits`（multipart）带参考图成功。响应另带 `usage`、`size`、`quality` 等字段，解析不依赖 | 2026-10-14 实测 | 【已验证】 |
| **Gemini 图片实测**（`gemini-3.1-flash-image`）：`x-goog-api-key` 被接受；返回 `candidates[].content.parts[].inlineData`；1K 为 PNG，**2K 为 JPEG**（16:9→2752×1536），3:4→896×1200；`inlineData` 参考图成功。响应体最大约 5 MB | 2026-10-14 实测 | 【已验证】 |
| **Grok 视频实测**（`grok-imagine-video-1.5`，5 秒 480p，带首帧图）：创建返回 `{request_id}`；轮询进行中是 **HTTP 202** `{status:"pending",progress}`，完成是 **200** `{status:"done",progress:100,usage,video:{url}}`（**不是 `succeeded`**）；失败是 **200** `{status:"failed",error:{code,message}}`；`/content` 返回 `video/mp4`（`ftypisom`），不重定向，完整下载 4.8 MB 约 3 秒。取值核对：时长 5·10、画幅 16:9·9:16·1:1、分辨率 480p·720p 均被接受 | 2026-10-14 实测 | 【已验证】 |
| **Seedance 视频实测**（`doubao-seedance-2.0-mini`，5 秒 480p，带 512×512 首帧图）：创建返回 `{id}`；轮询 `running`→`succeeded`（均 200），完成体含 `content.video_url`、`usage`，约 115 秒；`/content` 为 `video/mp4`，完整下载 2.6 MB 约 1 秒。取值核对：时长 5·10·15、五种画幅、480p·720p 合法，**1080p 被拒** | 2026-10-14 实测 | 【已验证】 |
| **Seedance 首帧图宽或高 < 300 像素被拒**（`400 InvalidParameter`，`param:image_url`，信息含 `expected the width to be at least 300px`）；`doubao-seedance-2.0-fast` 对 256 像素的图未拒绝（校验不同）；`doubao-seedance-2.0` 与 `doubao-seedance-2.5` 当前对所有请求返回 `502 Upstream request failed`（`fast`、`mini` 正常），详见 `网关问题-doubao-seedance-502.md` | 2026-10-14 实测 | 【已验证】 |
| 网关**没有取消视频任务的接口**：`DELETE /v1/videos/{id}` 返回 404；非法视频参数在创建时即被 400/422 拒绝且不产生任务（Grok 对损坏首帧图则创建成功、随后异步 `failed`，不计费） | 2026-10-14 实测 | 【已验证】 |
| 媒体模型价格服务端一律省略 `price`，客户端拿不到图片/视频单价 | `B1定价契约.md` v6/v7 | 【已验证】 |
| 视频下载需带 Key，图片临时 URL 可能指向网关以外主机 | 文档：`GET /v1/videos/{id}/content` 带 `Authorization` | 【文档】 |
| `assetProtocol` 已启用但 `scope` 为空；CSP 没有 `media-src`（回落 `default-src 'self'`，视频无法加载 asset 协议） | `tauri.conf.json:30-35` | 【已验证】 |
| `dialog:default`、`opener:default` 已授权；`open_path` 不在 `opener:default` 内，所以打开/定位文件改由 Rust 侧调用，不新增前端权限 | `capabilities/default.json`、`tauri-plugin-opener 2.5.3` | 【已验证】 |

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
│ ┌──────────────┐ ┌ 模式：[文本] [图片] [视频] ────┐ │
│ │ Key 选择器     │ │  对话流（用户/助手气泡，Markdown）│ │
│ │ (masked_key,  │ │  底部输入框 + 发送/停止          │ │
│ │  默认=广场选中)│ │  流式打字中显示 ▊ 光标           │ │
│ ├──────────────┤ ├────────────────────────────────┤ │
│ │ 场景预设（文本）│ │ 模型 / 输出语言 / 新会话        │ │
│ │ ·询盘回复     │ │ 参数面板（可折叠）：            │ │
│ │ ·产品详情页   │ │ temperature / top_p / max_tokens│ │
│ │ ·开发信       │ └────────────────────────────────┘ │
│ │ ·展会邀请     │ 图片 / 视频模式：模板 → 产品信息    │
│ │ ·参数转卖点   │ → 提示词 → 参数 → 结果（第 6、7 节）│
│ │ ·社媒帖子     │                                    │
│ │ ·多语言本地化  │                                    │
│ └──────────────┘                                    │
└──────────────────────────────────────────────────────┘
```

- Key 选择器与广场同款展示（name + masked_key）；切换 Key 开新会话，切换前 cancel 进行中的流。
- 场景预设 = 内置 system prompt 模板（`playgroundPrompts.ts` 中英内置），点击开新会话；system 由「场景指令 + 统一硬约束 + 输出语言」三段拼成（第 12 节）。图片/视频模式的模板在各自面板内选择。
- 会话历史存前端内存；工坊 Tab `forceMount`（对话非敏感，切走不丢）。
- 顶栏余额 chip 在每轮终态后刷新（见 5.1）。

### 5.3 Rust 实现要点

- **独立流式 HTTP 客户端**（M3）：不继承共享客户端 20s 总超时；新建 `reqwest::Client`（连接超时 10s；无总超时；`tokio::time::timeout` 包裹每次 chunk 等待实现 120s 空闲超时）。复用区域 `base_url()` 拼接与既有代理策略。**chat POST 不自动重放**（非幂等且计费），API Key 的 401 不接入登录 token 续期流程。
- **SSE 解析**：`stream` feature 已启用，逐行读 `data:`，`[DONE]` 终止，`choices[0].delta.content` 增量；不引入 dashmap，任务表用 `std::sync::Mutex<HashMap<...>>`（现有代码风格即如此）。
- **身份取消链路（H1，需新写）**：登出/切换会话身份的路径已有清缓存动作（`commands_auth.rs:403`），但**没有任务取消**。方案：playground 模块持有任务注册表（`request_id → (SessionIdentity, AbortHandle)`）；任务内 `tokio::select!` 监听取消分支 + 每次事件投递前复查 `manager.current_identity() == Some(注册身份)`，不一致 → 中止流并以身份失效错误终态返回。登出/切区域路径新增对 playground 注册表的清理调用（按完整 `SessionIdentity` = region + user_id + generation 匹配）。前端身份键只含区域+用户（`api.ts:105`），身份失效判定以后端为准，前端只消费终态。
- **错误契约（M6）**：所有错误返回 `{ code, message }`——code 是稳定枚举（`key_invalid` / `key_inactive` / `quota_exhausted` / `rate_limited` / `identity_changed` / `cancelled` / `network` / `payload_too_large` / `upstream_error`），message 是本地化文案；**未知上游响应不透传 body**（`sanitize_message_for_display` 不是密钥脱敏器，不能兜底），原始错误只进 log。测试用含 Key 的模拟错误断言明文不出现在 IPC 与日志。

## 6. P2：图片生成 / 编辑（已实现）

### 6.1 模型族与请求体

按模型名分族（服务端 `kind` 只说明是图片，请求体形态只能靠名称区分）；未识别的图片模型按 OpenAI 兼容形态处理。

| 族 | 判定 | 端点 | 请求体要点 | 响应取图 | 参考图 | 一次最多 |
|---|---|---|---|---|---|---|
| OpenAI（已联调） | 其余 | `POST /v1/images/generations`；带参考图时 `POST /v1/images/edits`（multipart：`image/prompt/model/n/size`） | `model/prompt/n/size`，尺寸 1024x1024、1536x1024、1024x1536（前两个已测） | `data[].b64_json` 或 `data[].url` | 支持 | 4（2 已测） |
| Grok（**默认优先**，we2ai 上最便宜） | `grok*`、含 `grok-imagine` | `POST /v1/images/generations`；带参考图时 `POST /v1/images/edits`（multipart，**同一个模型**） | `model/prompt/n/response_format:"b64_json"/aspect_ratio`（1:1·16:9·9:16·4:3·3:4），不下发 `size`（会被忽略） | `data[].b64_json`（JPEG） | 支持（实测） | 4（2 已实测） |
| Gemini（已联调） | 含 `gemini` | `POST /v1beta/models/{model}:generateContent`，头 `x-goog-api-key`（同时带 Bearer） | `contents[].parts`（text + 可选 `inlineData`）、`responseModalities:["TEXT","IMAGE"]`、`imageConfig.aspectRatio / imageSize(1K·2K·4K)` | `candidates[].content.parts[].inlineData.data` | 支持 | 1 |

能力表（可选尺寸、画幅、张数、是否支持参考图）由 Rust `MediaCapabilities` 下发，前端只渲染不推断；Rust 对不在表内的取值直接拒绝。

### 6.2 IPC

| 命令 | 输入 | 输出 / 行为 |
|---|---|---|
| `we2ai_playground_media_models` | `{ keyId, kind: "image" \| "video" }` | 复用广场 `kind` 数据过滤；返回 `id/provider/family/capabilities`；视频只返回能识别请求体的模型族；**Grok 族稳定排在最前**，前端默认选中第一项 |
| `we2ai_playground_image` | `{ requestId, keyId, model, prompt, n?, size?, aspectRatio?, imageSize?, reference? }` | invoke 阻塞至终态，返回 `images[{ path, mime, bytes, dataUrl }]`；图片已落盘 |
| `we2ai_playground_media_save_as` | `{ path }` | 弹系统保存对话框并复制；用户取消返回 `null` |
| `we2ai_playground_media_open` | `{ path, reveal }` | 系统程序打开，或在文件夹中显示 |

### 6.3 约束

| 项 | 规则 |
|---|---|
| 提示词 | ≤ 4000 字符；空白拒绝 |
| 参考图 | ≤ 8 MB；PNG/JPEG/WebP；声明的 mime 必须与字节头一致 |
| 响应校验 | 一律按字节头判定 PNG/JPEG/WebP，不信任上游 `Content-Type`；单张 ≤ 32 MB；响应体 ≤ 96 MB |
| 临时 URL | 仅 https；**不带 Authorization**（URL 可能在网关以外的主机） |
| 超时与重放 | 总超时 300 秒，受取消 / 身份监督；不自动重放（计费） |
| 落盘 | `<data_root>/studio/`，文件名 `时间戳-请求id-序号.扩展名`；保存时顺带清理 30 天前的文件 |
| 路径校验 | 另存 / 打开只接受工坊目录内真实存在的文件（canonicalize 后比较前缀，拒绝 `..` 与符号链接逃逸） |
| 并发 | 与文本对话共用任务表，同时只允许一个活跃任务；登出 `cancel_all` 同样中断 |

新增错误 code（稳定契约，文案本地化）：`PLAYGROUND_REJECTED`（400/422：参数不受支持或内容审核）、`PLAYGROUND_MEDIA_FAILED`、`PLAYGROUND_MEDIA_TIMEOUT`、`PLAYGROUND_MEDIA_INVALID`、`PLAYGROUND_PATH_INVALID`、`PLAYGROUND_SAVE_FAILED`；413 复用 `PLAYGROUND_PAYLOAD_TOO_LARGE`。上游错误体仍不读、不写日志。

### 6.4 UI（图片模式）

模型 → 场景模板 → 产品信息字段 → 参考图（可选）→ 可编辑提示词（含「AI 优化提示词」「恢复模板提示词」）→ 尺寸 / 画幅 / 分辨率 / 张数 → 生成 / 停止 → 结果网格（另存为、在文件夹中显示）。

- 提示词随模板与字段自动拼装；用户手改后不再被覆盖，点「恢复模板提示词」才回到模板。
- 换模板 / 换模型会清掉参数覆盖项，回到「模板构图 ∩ 模型能力表」的默认值。
- 面板以 `key={keyId}` 挂载，换 Key 整体重建；卸载时取消在途请求。

## 7. P3：视频生成（已实现）

### 7.1 模型族与请求体

| 族 | 判定 | 创建请求体 | 创建响应 id | 时长 / 画幅 / 分辨率（能力表） | 音频 | 提示词语言与上限 |
|---|---|---|---|---|---|---|
| Grok（**默认优先**） | `grok*`、含 `grok-imagine` | `model/prompt/duration/aspect_ratio/resolution`，参考图为 `image:{url,type:"image_url"}`（data URL） | `request_id` | 5·10 秒 / 16:9·9:16·1:1 / 480p·720p（均已核对） | 无 | 英文；**≤ 4096 字节**（建单前拒绝，来自 video-miniAPP 的线上教训） |
| Seedance | 含 `seedance` | `model/content[]`（text + `image_url` 且 `role:"first_frame"`）`/duration/ratio/resolution/watermark:false/generate_audio` | `id` | 5·10·15 秒 / 16:9·9:16·1:1·4:3·3:4 / 480p·720p（**mini 不支持 1080p**，其他型号未测，保守不开放） | 可选 | 中文；**首帧图宽高均 ≥ 300 像素**（Rust 前置校验并告知当前尺寸） |

其余视频模型不展示：没有通用请求体，猜错会白白计费。取值范围已于 2026-10-14 逐项核对（见第 2 节）；`doubao-seedance-2.0` 与 `doubao-seedance-2.5` 当前在网关上对所有请求返回 502，选到它们会得到“网关服务异常”（详情与排查建议见 `网关问题-doubao-seedance-502.md`）。

### 7.2 流程

```
前端 invoke we2ai_playground_video ──▶ 校验 + 登记任务（并发 1）
   ▲ event we2ai-playground-progress          │
   │ { requestId, stage, elapsedSecs, progress }▼
   │            POST /v1/videos（只提交一次，不重放）→ 取任务 id（安全字符校验）
   │            每 5 秒 GET /v1/videos/{id}，最长 15 分钟；连续网络失败容忍 3 次
   │            成功 → GET /v1/videos/{id}/content（带 Key，≤ 200 MB，字节头须为 MP4/WebM）
   └──────────  落盘 <data_root>/studio/ → 返回 { path, mime, bytes }
```

| 状态值（不区分大小写） | 处理 |
|---|---|
| `succeeded`（Seedance）、`done`（Grok，实测）、`success` `completed` `complete` | 下载 |
| `failed`（两族实测：Grok 为 HTTP 200 + `error{code,message}`）、`failure` `error` `cancelled` `canceled` `expired` `rejected` | `PLAYGROUND_MEDIA_FAILED`（不透传上游原因） |
| 其他（Grok 的 `pending`（**HTTP 202**）、Seedance 的 `running`，以及 `queued` 和未知值） | 继续轮询；超过 15 分钟返回 `PLAYGROUND_MEDIA_TIMEOUT` |

轮询按“2xx 即成功响应”处理（Grok 进行中是 202）；401/402/403/429 立即终止；偶发网络错误不放弃一个已计费的任务。

### 7.3 取消与费用

| 项 | 行为 |
|---|---|
| 取消 | 只放弃本地等待。**网关没有取消接口**（`DELETE /v1/videos/{id}` 实测 404），已提交的任务会在上游跑完，成功即计费；界面在生成中明示这一点 |
| 费用 | 价格服务端省略，**不显示金额**。界面提示「按分辨率和时长计费，生成成功才扣费」。每次终态后刷新余额 |
| 进程重启 | 任务 id 不持久化，重启后无法续接；本期不做，列入风险 |

### 7.4 预览与交付

- 应用内预览：`tauri.conf.json` 的 `assetProtocol.scope` 仅开放 `$HOME/.we2ai/studio/**`，CSP 增加 `media-src 'self' asset: http://asset.localhost`；守卫 4.19 禁止放宽范围。
- 预览失败（解码器、平台差异）回退为「用系统播放器打开」与「在文件夹中显示」。
- 另存为走系统保存对话框，路径限定在工坊目录内。

### 7.5 UI（视频模式）

模型 → 场景模板 → 产品信息字段 → 首帧参考图（可选）→ 可编辑提示词（Grok 显示字节计数）→ 时长 / 画幅 / 分辨率 / 音频 → 生成 / 停止（显示阶段、百分比、已用时）→ 成片预览与操作。时长不足 10 秒时分镜压缩为三镜头（第 12 节）。

## 8. 影响面与同步风险

| 文件 | 类型 | 冲突风险 |
|---|---|---|
| `src/we2ai/PlaygroundPage.tsx`（新增）、`playgroundPrompts.ts`（新增） | fork 独有 | 无 |
| `src/we2ai/{We2aiShell,strings,api}.ts(x)` | 既有 fork 文件内增量 | 低 |
| `src-tauri/src/we2ai/playground.rs`（新增）、`keys.rs`（选择记忆结构扩展 + clear 双份清理）、`commands_auth.rs`（登出路径挂 playground 任务清理，一处调用）、`{mod}.rs` | fork 独有文件内增量 | 低 |
| **`src-tauri/src/lib.rs`** | **上游共享文件**（命令注册 `generate_handler!`），合并时与上游改动同区 | 中：与功能 4/13/19 等既有触点相同，同步时人工确认注册段 |
| `scripts/we2ai/check-guards.sh` P1 新增 **4.18**、P2/P3 新增 **4.19**（4.17 已被功能 25 占用） | fork 基础设施 | 无 |
| `Cargo.toml`/`Cargo.lock` | 不新增依赖（不用 dashmap，reqwest stream/tokio/futures 已有；multipart 手工构造，不开 reqwest `multipart` feature） | 无 |
| `src-tauri/src/we2ai/playground/media.rs`（新增）、`playground.rs`（一行 `pub mod media;`）、`lib.rs`（5 个命令注册） | fork 独有文件内增量；`lib.rs` 注册段同上 | 中（同 lib.rs） |
| **`src-tauri/tauri.conf.json`** | **上游共享文件**：CSP 增加 `media-src`，`assetProtocol.scope` 由空数组改为工坊目录 | 中：上游改 CSP 或 `security` 段会冲突，保留 fork 侧两处 |
| `src/we2ai/Studio{Image,Video}Panel.tsx`、`StudioMediaParts.tsx`、`studioErrors.ts`、`studioMediaCommon.ts`、`studioMediaStrings.ts`、`studioMediaTemplates.ts`（新增） | fork 独有 | 无 |
| `src/we2ai/PlaygroundPage.tsx`、`api.ts`、`playgroundPrompts.ts` | 既有 fork 文件内增量（模式切换、媒体类型与封装、预设重写） | 低 |

不触碰上游其余文件。`lib.rs` 注册段为唯一共享集成点，与既有 fork 功能同级风险。

## 9. 守卫与测试

**守卫 4.18（P1 机械字面量）**：
- `lib.rs` 注册 4 个 `we2ai::playground::we2ai_playground_*` 命令；
- 增量事件名两侧一致；`playground.rs` 不出现 `response.text()`；`we2ai_logout` 函数体内调用 `playground.cancel_all()`；
- `keys.rs` 含工坊选择记忆字段/接口标识；
- `strings.ts` 含 `studioTitle`。

**守卫 4.19（P2/P3 机械字面量）**：
- `lib.rs` 注册 5 个媒体命令，`playground.rs` 含 `pub mod media;`；
- 进度事件名 Rust / TS 两侧一致；
- `media.rs` 不出现 `response.text()`；`download_image_url` 不含 `bearer_auth` / `authorization`；
- 另存为与打开都经 `resolve_studio_path`；
- `assetProtocol.scope` 恰为 `["$HOME/.we2ai/studio/**"]`，CSP 含 `media-src`；
- Grok 提示词字节上限、通用字符上限、参考图大小上限在 Rust / TS 两侧一致；
- 图片/视频前端不出现 `Bearer`、`Authorization`、`navigator.clipboard`；
- 模板里的「禁止编造」与「画面不出现文字」硬约束字面量存在（文本 / 图片 / 视频 / AI 编译）。

守卫已做破坏性验证：逐条改坏对应位置，确认会报错，再还原。

**P2/P3 测试**：
- Rust（`cargo test --lib we2ai::playground::media`，22 个）：分族、能力校验、三族图片请求体、multipart 编辑、解析（b64 / url / Gemini inlineData）、魔数识别、两族视频请求体与 4096 字节拒绝、任务 id 与状态解析、路径限制与清理；wiremock 端到端：图片成功 / 非图片字节 / 400·401·402·429·500 映射且不回显 body / 临时 URL 下载不带鉴权头 / Gemini 带 `x-goog-api-key` / 视频提交只发一次·轮询·下载 / 失败·超时·取消 / 下载内容非视频 / 身份失效；
- 前端（`tests/we2ai/`）：`studioMediaTemplates.test.ts`（57：所有模板×语言×时长无残留占位、时间轴连续、Grok 提示词在 150 字符长字段下仍 ≤ 4096 字节、硬约束存在、输出语言规则）、`StudioImagePanel.test.tsx`（17）、`StudioVideoPanel.test.tsx`（11）、`PlaygroundPage.test.tsx` 新增模式切换 / 换 Key 重建 / 输出语言用例。

**P1 测试**：
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

| 期 | 内容 | 状态 |
|---|---|---|
| P1 | 文本对话（本文 5 节全部）+ 联调验证第 2 节【待联调】项 | 已实现并发布 |
| P2 | 图片生成 / 编辑、三族请求体、落盘与另存（第 6 节） | 已实现；联调清单见第 13 节 |
| P3 | 视频生成、进度、预览（第 7 节） | 已实现；联调清单见第 13 节 |
| 行业模板 | 文本 / 图片 / 视频模板按机械制造与出口重做（第 12 节） | 已实现 |

每期独立发版；功能 27 已登记于 `自定义开发功能列表.md`，P2/P3 手动验收项已追加到 `docs/we2ai/实机验收清单.md`。

## 11. 已知限制与风险

| 风险 | 等级 | 处置 |
|---|---|---|
| 【待联调】项与真实网关不符（SSE 格式、usage 字段、/v1 端点鉴权细节） | 🟡 中 | P1 第一步即做网关联调探针（手动 curl + 临时命令），契约确认后再固化解析层；解析层单测与传输解耦 |
| 流任务注册表与登出/切区域清理是新写链路，漏挂清理点则旧流继续计费 | 🟡 中 | 清理调用收敛到单一函数，登出/切身份两处显式挂接 + 4.x 测试覆盖；事件投递前身份复查兜底 |
| 用户选中的 Key 被删/失效 → 401 | 🟢 低 | 稳定 code + 重新选择引导，不报原始错误、不自动建 Key |
| 选中 Key 额度耗尽/限流 | 🟢 低 | 配额与 429 两个 code 分开；选择器禁用 `quota_exhausted` |
| 明文复用扩大 `KeyCache` 明文消费面 | 🟢 低 | 仍限 Rust 进程内（消费方仅 `commands_apply` 与 playground），不出 IPC；守卫 4.18 禁止 `playground.rs` 读上游错误体，4.19 禁止图片/视频前端出现 Bearer / Authorization / 剪贴板（不做宽泛的 apiKey 禁词扫描：证明不了信息流） |
| 对话内容含敏感信息，仅存内存不持久化 | 🟢 低 | 写入用户说明；P2 评估可选本地历史开关 |
| 个别型号的取值范围与已联调型号不同（如 Seedance 其他型号的 1080p），或网关侧故障（`doubao-seedance-2.0` / `2.5` 当前 502） | 🟡 中 | 同族共用能力表，取值取联调型号的交集；不合法时创建即被拒且不计费，界面提示“请求被网关拒绝”；未测型号见第 13 节第 13 项 |
| 视频取消不能阻止上游继续计费（网关无取消接口，实测） | 🟡 中 | 界面明示；网关提供取消接口后再接入 |
| 视频任务 id 不持久化，客户端重启后丢失已提交的任务 | 🟡 中 | 本期不做；后续可把任务 id 落盘并在启动时询问是否续接 |
| 应用内视频预览依赖资源协议与平台解码器，Windows / Linux 未实机验证 | 🟢 低 | 预览失败回退到系统播放器；列入实机验收清单 |
| 资源协议范围写死 `$HOME/.we2ai/studio/**`，数据目录被覆盖时预览失效 | 🟢 低 | 数据目录在 WE2AI 模式固定为 `~/.we2ai`（守卫保证）；预览失败有回退 |
| 含真人面孔的参考图被网关拒绝（Seedance 要求走 asset） | 🟢 低 | 界面提示；模板本身不生成人脸 |
| 模板被当成「事实来源」：用户填错参数会被原样写进文案 | 🟡 中 | 提示词硬约束只使用输入、不补写；缺失用占位；文本输出里数值单位原样不换算 |

## 12. 行业模板（机械制造与出口）

### 12.1 借鉴映射（video-miniAPP → 本方案）

| video-miniAPP | 本方案落点 | 取舍 |
|---|---|---|
| `shopIndustryProfile`（主体称谓、五段镜头结构、负面清单、人物出镜指引） | `StudioVideoTemplate.shots` + 全局约束 + 负面清单 | 采用；行业换成机械制造与出口 |
| `shopUniversalGlobalParams`（竖屏、禁字幕水印、音频优先级） | `VIDEO_GLOBAL` / `IMAGE_GLOBAL_EN`：写实纪实、画面无任何文字 / Logo / 认证标志 | 采用；画幅由所选比例决定，不写死竖屏 |
| `shopUniversalShotConstraints`（五镜固定运镜） | 每个分镜自带运镜（缓推、平移、固定机位、拉远定格） | 采用 |
| `negativePrompt`（禁止编造图片中没有的内容） | `VIDEO_NEGATIVE`、文本 `CONSTRAINTS`：不得编造参数、认证、客户、价格 | 采用并加强（外贸场景的合规风险更高） |
| `videoPromptAssistSystemPrompt`（`[]` 锁定业务关键词、限长、只输出一段） | `buildCompileSystem`（可选「AI 优化提示词」，用同 Key 下的文本模型） | 采用；限长改为视频 450 汉字 / 220 词，图片 130 词 |
| `ownerGuidance`（人物照该怎么出镜） | 人物只用背影、侧影、手部，并佩戴防护装备 | 不同：工业场景合规，且 Seedance 对真人首帧要求走 asset |
| 口播字数校验（每秒 2-6 字） | 不采用 | 本方案不生成口播脚本；音频由模型生成（Seedance `generate_audio`） |
| 草稿续接、积分扣退、内容审核 | 不采用 | 属于小程序后端，不在客户端范围 |

### 12.2 文本模板（`playgroundPrompts.ts`）

| id | 场景 | 产出 |
|---|---|---|
| `inquiry-reply` | 询盘回复 | 逐条回应 + 3 个待确认问题 + 下一步，150 词内，2 个备选主题行 |
| `product-copy` | 产品详情页（B2B 平台） | 标题 ≤ 120 字符、3-5 条卖点、参数表、应用、包装发运、3 条问答 |
| `cold-email` | 开发信 | 2 个主题行 + 120 词正文 + 5 天后跟进信 |
| `exhibition-invite` | 展会邀请 | 邀请邮件 + LinkedIn 短帖 + WhatsApp/微信短信，展会名·日期·展位号逐字一致 |
| `selling-points` | 参数转卖点 | 「参数原值 → 对生产的影响 → 买家收益」5 条 |
| `social-post` | 社媒帖子 | LinkedIn、Facebook、YouTube 描述 |
| `localization` | 多语言本地化 | 行业用语、计量与电压、贸易术语；不追加输出语言（目标语言由用户指定） |

system 拼装：**场景指令 + 统一硬约束 + 输出语言**。输出语言可选 9 种，默认英语（英 / 西 / 俄 / 阿 / 法 / 葡 / 德 / 日 / 中）。统一硬约束 5 条：不编造认证·参数·产能·客户·价格·交期·起订量·专利·获奖；缺失用 `[待补充：…]`；参数数值单位原样；不用绝对化用语；不承诺未确认的保修与交付条款。

### 12.3 图片模板（`studioMediaTemplates.ts`）

| id | 场景 | 默认构图 |
|---|---|---|
| `hero-product` | 设备主图（浅灰棚拍） | 1:1 |
| `factory-floor` | 工厂车间实景 | 16:9 |
| `process-closeup` | 工艺细节特写（微距） | 4:3 |
| `application-scene` | 客户现场应用 | 16:9 |
| `packing-shipping` | 出口包装与装柜 | 4:3 |
| `exhibition-poster` | 展会海报底图（上三分之一留白，不渲染文字） | 3:4 |

图片提示词一律英文。全局约束：画面内无文字 / 数字 / Logo / 认证标志，无可辨认人脸，不增加描述中没有的部件；带参考图时追加「保持产品外形、颜色、结构，只改背景、光线、角度」。构图偏好会落到所选模型的能力表上（不在表内取第一个，表为空则不下发）。

### 12.4 视频模板

| id | 场景 | 五镜头结构 |
|---|---|---|
| `factory-strength` | 工厂实力展示 | 厂区航拍 → 车间产线 → 核心加工特写 → 质检 → 装柜定格 |
| `product-demo` | 设备功能演示 | 外观环绕 → 关键部件微距 → 运行演示 → 工件取出展示 → 整机定格 |
| `process-craft` | 工艺品质 | 原材料 → 加工细节 → 装配 → 试运行检测 → 成品排列 |
| `shipping-delivery` | 装柜发运 | 成品仓库 → 包装 → 叉车装柜 → 封柜 → 港口全景 |
| `exhibition-invite` | 展会邀请 | 展品聚光 → 展厅氛围 → 洽谈（手部）→ 现场演示 → 展台全景 |

- 时长 ≥ 10 秒为五镜头，5 秒压缩为三镜头（开场、核心、收尾）；每镜时间区间按总时长等分取整，最后一镜结束等于总时长（单测保证）。
- 提示词结构：风格行 → 产品锁定行（`[产品名]` 方括号锁定业务关键词，可含型号 / 外观 / 特征）→ 分镜 → 全局约束 → 负面清单 → 参考图条款。
- 语言：Seedance 中文，Grok 英文；Grok 受 4096 字节限制，单测验证字段写到 150 字符时仍不超限。

## 13. 联调清单

2026-10-14 已用真实 Key 对四类模型各跑一遍（脚本在会话临时目录，Key 文件已删除）。结论回写第 2 节；下表是逐项状态。

| # | 项 | 结论 | 状态 |
|---|---|---|---|
| 1 | 路由存在 | 全部 401（不存在的路由 404） | 已验证 |
| 2 | OpenAI 图片：尺寸、`n` | 1024×1024、1536×1024、`n=2` 通过；1024×1536 未测（与 1536×1024 对称） | 已验证 |
| 3 | `/v1/images/edits` | Grok、GPT 均 multipart 通过，响应同为 `data[].b64_json`；Gemini 走 `inlineData` | 已验证 |
| 4 | Grok 图片 | 认 `aspect_ratio` 五种，忽略 `size`，`n=2` 通过；`n=4` 未测 | 已验证 |
| 5 | Gemini 鉴权 | `x-goog-api-key` 通过；1K PNG、2K JPEG；4K 未测 | 已验证 |
| 6 | Grok 视频 | 202 pending → 200 `done`；失败为 200 `failed`+`error`；取值全部核对 | 已验证 |
| 7 | Seedance 视频 | `running` → `succeeded`；取值核对，1080p 被拒；首帧图须 ≥ 300 像素 | 已验证（mini） |
| 8 | `/content` | `video/mp4`，不重定向，完整下载正常 | 已验证 |
| 9 | 取消任务接口 | 不存在（`DELETE` 404） | 已验证 |
| 10 | 含真人面孔的首帧被拒 | 未测（需真人照片） | 待验证 |
| 11 | 应用内视频预览（三平台） | 需要启动应用 | 待验证 |
| 12 | 终态后余额刷新 | 需要启动应用 | 待验证 |
| 13 | 其余型号：`doubao-seedance-2.0` 与 `2.5`（当前 502，已出报告）、`-fast`，Grok 的 `grok-imagine-video`、`grok-video-1.5` 等，GPT 的 `gpt-image-1/1.5/2.5-*`，Gemini 的其他图片型号 | 同族共用请求体与能力表；个别型号取值范围可能不同，不合法时创建即被拒（400/422）且不计费，界面给出“请求被网关拒绝” | 未逐个测 |

探测中意外产生的任务（均已记录）：`doubao-seedance-2.0-fast` 对 256 像素的图未拒绝，创建了 1 条 5 秒 480p 视频任务（成功则计费）；Grok 接受损坏的首帧图，创建了 5 条任务，均异步 `failed`，未成功。
