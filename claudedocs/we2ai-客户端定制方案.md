# WE2AI 桌面客户端定制方案 v27（终版）

日期：2026-09-23　基线：cc-switch we2ai 分支 `3f805895`（4.20.4，上游 3.20.4；与评审初期的 `85777fa0` 代码无差异）、SubPanel `60e00724b`（P1 切分支时以实际提交号为准并复核行号）　后端：SubPanel

当前版本 v27（终版）。评审历程：Codex 23 轮对抗评审（第 23 轮 PASS）、Fable 5 过程评审两轮、Sonnet 5 复核一轮、Fable 5 终验两次（第二次 PASS）。各版本改动见文末附录。

## 0. 已定决策

| # | 决策 | 结论 |
|---|---|---|
| 1 | 供应商范围 | 只有 we2ai.com，用户不可添加其他供应商 |
| 2 | 登录方式 | 邮箱密码（含 2FA）+ 国内版手机验证码；OAuth 全部不做 |
| 3 | 会话保持 | refresh_token 存系统钥匙串，静默续期，默认最长 30 天免登录 |
| 4 | 目标工具 | Claude Code、Codex、WorkBuddy；检测安装状态，未装给下载链接 |
| 5 | 指定模型 | 客户端本地写工具配置并激活，SubPanel 不存"哪台机器用哪个模型" |
| 6 | Key 选择 | 用户从已有 Key 中选，客户端不创建 Key |
| 7 | 其余模块 | 前端隐藏、Rust 启动白名单禁用、IPC 默认拒绝白名单拦截（第 6.2 节），代码保留 |
| 8 | 品牌 | productName、identifier、scheme、数据目录、图标、托盘、关于页全改为 WE2AI |
| 9 | 上游同步 | 继续整体 merge，冲突人工核对 |
| 10 | 后端改动 | **SubPanel 需要 7 处改动**（第 3.2 节），v1 的"后端零改动"不成立 |

## 1. 用户旅程

```mermaid
flowchart LR
  A[首次打开] --> B[登录页 选区域]
  B --> C{Key 数量}
  C -->|1 个| D[自动选中]
  C -->|多个| E[顶栏 Key 下拉 记住上次]
  D --> F[模型广场 仅显示该 Key 可用模型]
  E --> F
  F --> G[卡片按钮 仅显示该 Key+模型 支持的工具]
  G --> H[确认弹窗 显示将写入的文件与字段]
  H --> I[we2ai_apply_model 原子写入 + 顶栏状态刷新]
```

| 减少选择的规则 | 做法 |
|---|---|
| 不问槽位 | Claude Code 四个模型变量写同一模型（现有生成器即如此，`src-tauri/src/provider.rs:771-800`）；「高级」折叠可分别改 |
| 不问协议 | 工具按钮由服务端返回的 Key 级能力决定（第 3.2 节），客户端不做任何协议推断 |
| 不问供应商名 | Claude Code、Codex 各一条固定 id 的供应商记录原地更新；WorkBuddy 见第 4.3 节 |

## 2. 架构

```mermaid
flowchart TB
  subgraph 前端
    L[LoginPage] --> S[session store]
    S --> K[KeySelector]
    K --> M[ModelSquarePage]
    M --> T[ToolStatusBar]
  end
  subgraph Rust src-tauri/src/we2ai
    C0[mode.rs 数据根 + 启动白名单 + IPC gate]
    C1[session.rs keyring + 单飞 refresh]
    C2[api.rs envelope 解包 + 分页]
    C3[captcha.rs 验证码窗口导航拦截]
    C4[apply.rs we2ai_apply_model]
    C5[workbuddy.rs]
    C6[detect.rs]
  end
  S --> C1
  L --> C3
  M --> C2
  M --> C4
  C4 -->|we2ai_apply 锁内调用 switch，switch 自行加锁| P[现有 provider 管道]
  C4 --> C5
  T --> C6
  C2 --> API[(SubPanel)]
  P --> F1[~/.claude/settings.json]
  P --> F2[~/.codex/config.toml]
  C5 --> F3[WorkBuddy models.json]
```

## 3. SubPanel 接口

### 3.1 通用约定

| 项 | 规则 | 证据 |
|---|---|---|
| 响应 | 成功 `{code:0, message, data}`。失败有**四种形态**，客户端都要解码：① 业务错误 `{code:<数字>, message, reason:"<错误码>"}`（`response.ErrorFrom`）；② 中间件错误 `{code:"<错误码>", message}`，`code` 是字符串、没有 `reason`（JWT 中间件用的就是这种，如过期返回 `code:"TOKEN_EXPIRED"`）；③ 通用错误 `{code:<数字，等于 HTTP 状态码>, message}`，没有 `reason`（`response.BadRequest`、`Forbidden` 等，如请求绑定失败）；④ 认证路由限流 `{error, message}` + HTTP 429，没有 `code`（登录、刷新等认证路由的限流中间件，`backend/internal/middleware/rate_limiter.go:169-180`，Redis 故障时同样返回此形态）。受保护路由限流走形态 ②：`{code:"RATE_LIMITED", message}` + 429（`backend/internal/server/middleware/panel_rate_limit.go:162`）。任一 429 均保留凭证并退避重试。错误码取值：有 `reason` 取 `reason`；否则 `code` 为字符串时取 `code`；都没有时按 HTTP 状态码处理（401 按未知 401 终止会话，其余按普通请求失败） | `backend/internal/pkg/response/response.go:14-38,59,80`、`backend/internal/server/middleware/middleware.go:63-66`、`backend/internal/server/middleware/jwt_auth.go:60-66` |
| 分页 | `data = {items, total, page, page_size, pages}`，默认 20 条；客户端循环到 `page == pages` | `backend/internal/handler/api_key_handler.go:104-147` |
| 请求头 | `Authorization: Bearer <access_token>`；`User-Agent: WE2AI-Desktop` 常量，跨版本、跨系统不变；版本与系统放 `X-We2ai-Client: desktop/<version>/<os>` | session binding 以 UA+IP 为指纹，UA 变化会撤销整个 refresh 家族（`backend/internal/service/session_binding.go:15`、`backend/internal/service/auth_service.go:1870`） |

### 3.2 SubPanel 需要新增或修改的 7 处

| # | 改动 | 原因 | 验收 |
|---|---|---|---|
| B1 | 新增 `GET /api/v1/desktop/keys/:id/models`，计算顺序：① Key 归属当前用户（否则 404），**不按状态过滤**，模型集与准入状态分别计算；② 取 Key 分组可调度账号的模型集；③ 套分组模型白名单（`backend/internal/server/middleware/group_model_allowlist.go:32`）；④ 复合分组按模型解析目标平台（`backend/internal/server/routes/gateway.go:606`）；⑤ 逐 `模型 × 端点` 计算 `tools: ["claude_code","codex","workbuddy"]`（协议支持）；⑥ 另返回 Key 级准入结果 `callable: bool` 与 `blocked_reason`。`callable` 定义为**网关 API Key 中间件的完整准入结果**：把中间件（`backend/internal/server/middleware/api_key_auth.go`）中请求无关的准入判定抽成 service 层函数，网关与 B1 共用；`blocked_reason` 取该函数返回的**统一业务错误码**（如 `API_KEY_QUOTA_EXHAUSTED`），与端点无关；网关在不同协议端点上会把同一原因映射成不同外部格式（如 OpenAI 格式的 `insufficient_quota`，`backend/internal/server/middleware/middleware.go:83`），B1 不暴露这层映射。不在 B1 里另行枚举原因。已知会拒绝的条件包括：Key 状态与到期、配额耗尽、IP 白名单与黑名单、分组停用、分组绑定权限、无有效订阅、订阅限额、余额不足（`api_key_auth.go:132,160,197,227,237,261,437`）。`ip_not_allowed` 按本次 B1 请求的来源 IP 判定，桌面端与工具通常同机同出口，列为近似判断。客户端在 `callable=false` 时工具按钮置灰并显示原因，不阻止用户查看模型。可选能力字段 `supports_tool_call / supports_images / reasoning_efforts` 无数据则省略 | 现有 `/api/v1/models` 按用户全部分组取并集，不接收 Key、不套白名单（`backend/internal/handler/usage_handler.go:746-781`、`backend/internal/service/model_routing_service.go:73`）；`provider` 是目录元数据，不是入站协议 | 负例全部覆盖：跨分组 Key、白名单外模型、复合分组同组不同模型、OpenAI 分组关闭 messages dispatch、禁用或过期 Key、他人 Key 返回 404；分组停用、配额耗尽、IP 不在白名单或命中黑名单、无分组绑定权限、无有效订阅、订阅超限、余额不足时，`callable=false` 且 `blocked_reason` 等于共用准入函数返回的业务错误码；同时用真实网关请求验证该 Key 确被拒绝 |
| B2 | 手机 `send-sms-code`、`phone-login` 的 DTO 与 handler 改为接收完整 `CaptchaProof`（turnstile_token / tencent ticket+randstr / aliyun param），调用 `VerifyCaptcha` | 现在只调 `VerifyTurnstile`，腾讯、阿里启用后手机流程必失败（`backend/internal/handler/auth_handler.go:795-800,869-883`） | 三家验证码分别开启时，手机发码与登录均通过 |
| B3 | 前端新增路由 `/desktop/captcha?n=<nonce>&p=<provider>`：复用现有验证码组件；成功后用 `window.location.assign()` 做**完整页面导航**（不用 `router.push`）到 `https://<本站>/desktop/captcha/done#n=<nonce>&p=<provider>&<proof>`。proof 按 provider 结构化：turnstile `t=`；tencent `ticket=&randstr=`；aliyun `param=` | 桌面端无法渲染验证码组件；腾讯校验需要两个独立字段（`backend/internal/service/auth_service.go:93`）；SPA 内路由切换不触发 WebView2 `NavigationStarting` | 三家验证码分别开启，桌面端发短信与登录端到端通过 |
| B4 | `/auth/logout`：用传入 refresh 查出家族 id（先查有效 token，查不到再查 B5 的已消费映射），调用已有的 `RevokeSessionFamily`（`backend/internal/service/auth_service.go:1912`）撤销整个家族：用一个 Redis Lua 脚本原子完成"写家族墓碑（TTL = refresh 有效期）+ 删除家族全部成员 token + 删除家族索引"，取代现在先取成员再删除的两步（`backend/internal/repository/refresh_token_cache.go:69`）；响应增加 `revoked: bool`，仅当查到家族且脚本执行成功时为 true | 现在只删单个 token 且吞掉错误（`backend/internal/handler/auth_handler.go:726-735`、`backend/internal/service/auth_service.go:1898`）；`DEL` 删 0 个键也算成功（`backend/internal/repository/refresh_token_cache.go:68-70`） | 撤销有效 token 返回 true 且同家族所有 token 失效；用已轮转旧 token 登出也能经已消费映射撤销家族并返回 true；映射过期（即后继 token 也已过期）后返回 false；旧 token 在到期前 1 秒轮转且响应丢失，稍后凭旧 token 登出仍返回 true；网页端行为不变 |
| B5 | `/auth/refresh` 的消费与签发合并为**同一个** Lua 脚本：Go 先非破坏性读取旧 token 数据、完成用户状态与会话绑定校验、生成新 token 对；再执行脚本原子完成"旧 token 仍存在 → 删除旧 token → 家族墓碑不存在 → 写入新 token + 加入家族索引 + 加入用户索引"，同一脚本内再写入已消费映射 `consumed:<旧 token 哈希> → family_id`，TTL 等于后继 refresh 的完整有效期（`backend/internal/service/auth_service.go:1778`），保证响应丢失后只要后继 token 还有效，就能凭旧 token 找回家族撤销；不用旧 token 的剩余有效期，否则临期轮转时映射几乎立即过期。任一条件不满足则整体拒绝、不产生任何写入。**后端模式：** Go 预检阶段（脚本执行前）就检查后端模式与用户角色，非管理员直接返回 403 `BACKEND_MODE_ACTIVE`（B7）、不消费旧 token；现在是轮转之后才检查（`backend/internal/handler/auth_handler.go:694-703`），会让客户端丢失新 token。**复用检测：**提交的 refresh 命中已消费映射（说明旧 token 被重复使用），视为泄露，直接对该家族执行 B4 的撤销脚本。取代现在"读取→删除→签发"分步执行和分别存 token 与索引的做法（`backend/internal/service/auth_service.go:1790,1880`）。消费与签发之间没有空档：登出在脚本之前执行，用旧 token 查到家族、写墓碑，脚本随后被拒；在脚本之后执行，用旧 token 经已消费映射查到家族并撤销。后者覆盖"脚本已提交、响应丢失或客户端崩溃、客户端只剩旧 token"的情况 | 现在两个并发 refresh 可读到同一旧 token，各自签发后继 token（`backend/internal/service/auth_service.go:1823,1880`）；登出只能撤销其中之一 | 同一 refresh 并发两次只有一次成功，失败的那次触发复用检测、家族被撤销；脚本提交后丢弃响应，再用旧 token 登出，断言家族内无存活 refresh、旧 access token 返回 401；并发刷新两次后登出，断言家族内无存活 token；用测试钩子把登出固定在"Go 校验之后、脚本执行之前"执行，断言脚本被拒且无后继 token 可刷新；后端模式开启时非管理员 refresh 返回 403 且旧 token 仍存在 |
| B6 | 抽出共用的墓碑校验函数，在**两条** JWT 鉴权路径中调用：普通用户中间件（`backend/internal/server/middleware/jwt_auth.go:60` 之后）和管理员中间件（`backend/internal/server/middleware/admin_auth.go:165`，`/admin` 路由不经过普通中间件，`backend/internal/server/routes/admin.go:23`）。按 claims 的 `sid` 检查家族墓碑，存在即返回 401 `TOKEN_REVOKED`；无 `sid` 的旧 token 按现状处理 | 只撤销 refresh 家族时，已签发的 access token 到期前仍可调用返回明文 Key 的 `/keys`（`backend/internal/handler/dto/types.go:60`），默认有效期回退为 24 小时（`backend/internal/config/config.go:2438`） | 登出后用旧 access token 请求 `/api/v1/keys` 返回 401；管理员登出后旧 JWT 请求 `/api/v1/admin/*` 返回 401；未登出会话不受影响；中间件每请求多一次 Redis `EXISTS` |
| B7 | 后端模式的所有拒绝点返回统一错误码 `BACKEND_MODE_ACTIVE`：用户路由守卫 `BackendModeUserGuard`（`backend/internal/server/middleware/backend_mode_guard.go:14`，挂在 `backend/internal/server/routes/user.go:20`）、认证路由守卫 `BackendModeAuthGuard`（`backend_mode_guard.go:80`，`phone-login`、`send-sms-code` 不在其放行清单 `backend_mode_guard.go:30`，会在这里被拒）、B5 的 refresh 预检、邮箱登录与 2FA handler 内的现有检查 | 现在守卫只返回一条 403 提示文字，客户端无法可靠识别；已登录的非管理员在后端模式开启后调 `/keys` 等接口会一直收到 403，直到 access token 过期 | 登录后开启后端模式，非管理员下一次请求即收到 `BACKEND_MODE_ACTIVE`；后端模式下非管理员分别走邮箱登录、2FA、手机发码、手机登录，均收到 `BACKEND_MODE_ACTIVE`；管理员邮箱登录、2FA、refresh 与受保护接口不受影响。后端模式下手机入口按路径整体关闭（守卫此时尚不知道用户角色），**管理员也不能用手机登录**，需改用邮箱，客户端在手机登录页收到 `BACKEND_MODE_ACTIVE` 时提示"请使用邮箱登录" |

工具与入站端点的对应关系固定：Claude Code → `/v1/messages`，Codex → `/v1/responses`，WorkBuddy → `/v1/chat/completions`。

B1 对每个 `模型 × 端点` 的判定**直接调用网关处理该端点时使用的同一组判定函数**（入站协议、目标平台、账号出站端点、messages dispatch 放行规则如 `allowOpenAICompatibleMessagesDispatch`，`backend/internal/handler/openai_gateway_handler.go:334`），不在方案或 B1 中另写平台对照表。若判定逻辑散落在 handler 内无法复用，先抽成 service 层纯函数，网关与 B1 共用。

B1 契约负例在前述基础上追加：Grok、Kimi、智谱、DeepSeek、MiniMax、OpenCode Go 分组无需 dispatch 开关即出现 `claude_code`；generic 分组按账号实际协议走 Gemini 兼容链的 `/v1/chat/completions`；每个返回的工具能力至少用真实请求打一次对应端点验证。

### 3.3 客户端使用的接口

| 用途 | 方法 路径 | 请求 | 响应 data |
|---|---|---|---|
| 公开设置 | GET /api/v1/settings/public | — | 验证码开关与 site key |
| 邮箱登录 | POST /api/v1/auth/login | email, password, 验证码字段 | TokenPair 或 `{requires_2fa, temp_token}` |
| 2FA | POST /api/v1/auth/login/2fa | temp_token, **totp_code**（6 位） | TokenPair |
| 发短信 | POST /api/v1/auth/send-sms-code | phone + CaptchaProof（B2 后） | — |
| 手机登录 | POST /api/v1/auth/phone-login | phone, code + CaptchaProof（B2 后） | TokenPair |
| 续期 | POST /api/v1/auth/refresh | refresh_token | 新 TokenPair，旧 refresh 原子失效（B5 后） |
| 登出 | POST /api/v1/auth/logout | **refresh_token**（不带则服务端不撤销） | `{revoked}`，撤销整个家族（B4 后） |
| Key 列表 | GET /api/v1/keys?page=&page_size=100 | — | 分页，展示 `status` 为 `active` 与 `quota_exhausted` 的 Key（`backend/internal/service/api_key.go:9`），后者工具按钮置灰并显示配额耗尽；B1 对其返回 `callable=false`、`blocked_reason=API_KEY_QUOTA_EXHAUSTED`（`backend/internal/server/middleware/api_key_auth.go:308`） |
| Key 模型 | GET /api/v1/desktop/keys/:id/models | — | B1 |
| 日用量 | GET /api/v1/**user**/api-keys/:id/usage/daily | — | 第二期 |

区域与地址（一个安装包，登录页切换，记住上次）：

| 区域 | 地址 | 2026-09-23 实测 |
|---|---|---|
| 国际版 | `https://api.we2ai.com` | 在线，验证码均关闭 |
| 国内版正式 | `https://api.wtgo.com.cn` | 未上线 |
| 国内版测试 | `https://jiwu.wtgo.com.cn` | 在线，仅开发构建可选 |

工具写入的网关地址：Claude Code 用根地址，Codex、WorkBuddy 用 `/v1`。

## 4. 工具写入

### 4.1 统一入口 `we2ai_apply_model`

单一 Rust 命令，前端不再分别调 `add_provider` 和 `switch_provider`。

**不在外层拿 switch lock。** `ProviderService::switch` 内部会获取同一把按应用划分的非重入锁（`services/provider/mod.rs:5739`、`proxy/switch_lock.rs:18`），外层再拿会死锁。WE2AI 用自己的 `we2ai_apply` 互斥锁串行化自身调用，再调用现有 `switch`，由它自己拿锁。除下方"热切换拒绝"一行外，不改上游 provider 服务文件。`switch` 内部用 `futures::executor::block_on` 获取 tokio 锁（`services/provider/mod.rs:5743-5749`），上游命令都在 `spawn_blocking` 中调用它（`commands/provider.rs:119-125`），`we2ai_apply_model` 同样在 `spawn_blocking` 中执行，不直接在 async 上下文调用。

WE2AI 进程内没有其他切换入口：托盘的供应商切换菜单隐藏，本地代理接管在启动白名单中禁用（第 6.2 节）。

**跨进程：** live 文件（`~/.claude/settings.json`、`~/.codex/*`、WorkBuddy `models.json`）属于用户，CC Switch、工具本身或用户都可能同时改写。这些写入者之间没有共同遵守的文件锁，WE2AI 无法建立跨进程协调，因此**只做尽力检测，不承诺杜绝覆盖**：比对与替换之间的毫秒级窗口无法消除，这是已知限制，写进用户文档。策略是"写后记代次，恢复前比对"：

| 时点 | 动作 |
|---|---|
| 快照 | 记录每个文件的内容哈希 H0 |
| 写入后 | `switch` 返回后立即回读文件计算 H1，这样现有写入器的规范化、MCP 重投影都已计入，不改上游写入层。回读前的瞬间若有外部写入会被误记为 H1，归入本节已知限制 |
| 失败恢复前 | 当前哈希 = H1 才恢复（= H0 说明未写入，无需恢复）；否则判定被外部改写，不自动恢复，报告"检测到其他程序修改，未回滚"，并给出快照文件路径供用户手动恢复 |
| `switch` 中途失败（无 H1） | 上游管道可能已改写部分文件后才报错（如 Codex 先改模型目录、再在配置写入处失败，`codex_config.rs:2720`），此时拿不到 H1。规则：当前哈希 = H0 的文件跳过；≠ H0 的文件视为本次 `switch` 的部分写入，按快照写回。`switch` 执行期间若恰有外部程序改写同一文件，会被误判为部分写入而覆盖，归入本节已知限制（窗口为 `switch` 执行时长，毫秒级） |
| 应用启动与每次 apply 前 | 检测 CC Switch 进程是否在运行，在运行则在顶栏提示"CC Switch 也在管理这些工具，可能互相覆盖"。检测键：macOS 按 bundle id `com.ccswitch.desktop`；Windows 按进程名 `cc-switch.exe`；Linux 按可执行名 `cc-switch`。用户自行改名的便携版检测不到，列为已知限制 |

**热切换拒绝（改上游一行）：** `switch` 在检测到接管（数据库有代理备份或 live 有接管标记，`services/provider/mod.rs:5751-5761`）后会走热切换，而热切换用**本进程**的代理配置生成地址写入 live（`services/proxy.rs:1948,3074`）。WE2AI 若进入该分支，会把自己的代理地址写进 CC Switch 正在接管的文件，端口不同时工具将指向未运行的代理。因此在该判定之后加一行：WE2AI 模式下 `is_app_taken_over || live_taken_over` 为真即返回 `TakeoverConflict` 错误，不进入热切换、不写 live、不写 `proxy_live_backup`。这是 WE2AI 对 `services/provider/mod.rs` 的唯一改动，登记到第 7 节触点。它同时消除前置检查与 `switch` 内部判定之间的竞态：竞态发生时 `switch` 直接失败，不会产生任何 live 写入。

**前置条件：** ① apply 前校验 WE2AI 数据库中该应用只有固定 id 一条供应商、current 指向它或为空；② 用现有 `detect_takeover_in_live_configs` 同款判定检查目标 live 文件不处于代理接管状态（Claude `env` 含 `PROXY_MANAGED`、Codex 含本地代理路由）。任一不满足则拒绝并提示；接管状态的提示为"CC Switch 正在代理接管此工具，请先在 CC Switch 中关闭接管"。第 ② 条防止现有 `switch` 走热切换分支（`services/provider/mod.rs:5759-5797`）只改数据库、不写 live 却返回成功。"更新公共配置片段"分支由供应商 `meta.common_config_enabled == true` 门控（`services/provider/mod.rs:6229-6236`），与接管无关，由第 4.2 节"固定 id 供应商不启用公共配置"保证不进入。第 ① 条保证现有 `switch` 不会进入"回填旧供应商行"分支（`services/provider/mod.rs:5844`），快照清单无需覆盖旧供应商行。启动导入已禁用，正常使用下前置条件恒成立。

```mermaid
sequenceDiagram
  participant UI
  participant A as we2ai_apply_model
  participant P as provider 管道
  participant F as live 文件
  UI->>A: tool, key_id, model
  A->>A: 获取 we2ai_apply 锁（不是 switch lock）
  A->>A: 按事务快照清单全量快照
  A->>P: upsert 固定 id 供应商（save_provider 同 id 走 UPDATE）
  A->>P: 调用现有 switch（内部自行加 switch lock）
  alt switch 返回成功
    A->>F: 回读 live 托管字段
    A->>A: 模型、网关地址、认证字段任一与期望不一致，或出现接管标记，按失败处理
    A->>P: 收敛数据库行为仅托管字段
    A-->>UI: 成功 + 回读到的最终模型
  else 失败
    A->>F: 按清单逐项恢复
    A->>A: 回读核对与快照一致
    A-->>UI: 错误 + 未改动（核对不一致则报告具体残留项）
  end
```

事务快照清单：

| 对象 | 内容 | 依据 |
|---|---|---|
| 数据库 | 固定 id 供应商行（含不存在状态）、该应用 DB current 标记。上游 DAO 只有 `set_current_provider`、没有清空函数（`database/dao/providers.rs:389-410`），WE2AI 在自有模块内执行清空 current 的 SQL 用于回滚"行存在、current 为空"的状态 | `database/dao/providers.rs:180-238` |
| 数据库 | 该应用的代理备份记录 `proxy_live_backup`（含不存在状态）。热切换会把供应商配置（含 Key）写进这里（`services/proxy.rs:2933,3074`），且"存在备份"本身会让以后的 `switch` 一律走热切换（`services/provider/mod.rs:5751-5756`） | |
| 本地设置 | 该应用的本地 current 供应商 | `services/provider/mod.rs:5963` |
| Claude Code | apply 开始时调用上游 `get_claude_settings_path()` 解析出的实际文件并固定该路径：`settings.json` 不存在而旧版 `claude.json` 存在时，上游写的是 `claude.json`（`config.rs:243-255`、`services/provider/live.rs:1312-1317`）。快照、H0/H1、权限收紧、回滚、回读门都针对这个固定路径 | |
| Codex | WE2AI 自行读取 `auth.json`、`config.toml`、模型目录、托管 marker 的原字节与"是否存在"（文件清单参照 `CodexLiveStateSnapshot`，`codex_config.rs:251-290`；该结构字段为模块私有，不复用其恢复函数）；恢复由 WE2AI 用 `atomic_write_private` 写回，原先不存在的文件删除 | |
| WorkBuddy | `models.json`、托管状态记录 | 第 4.3 节 |

`switch` 返回成功不等于成功。回读 live 后以下四项全部满足才向用户报成功：① 模型等于期望值；② 网关地址等于 WE2AI 网关（Claude `env.ANTHROPIC_BASE_URL`、Codex `[model_providers.we2ai].base_url`），不是本地代理地址；③ 认证字段等于所选 Key（Claude `env.ANTHROPIC_AUTH_TOKEN`、Codex provider 级 `experimental_bearer_token`），不是 `PROXY_MANAGED`；④ 接管判定函数返回未接管。任一不满足且接管判定为已接管时，**只恢复 WE2AI 自有的数据库（供应商行、current 标记、`proxy_live_backup`）与本地设置，不恢复工具 live 文件**：此时 live 已被 CC Switch 的代理路由接管，按快照恢复会拔掉正在运行的代理。有了上面的热切换拒绝，这条路径只作为兜底（例如 `switch` 之后、回读之前 CC Switch 才开启接管）。提示"检测到代理接管，未生效"。不满足但未接管时，按正常失败走完整恢复清单。只核对模型不够：热切换会保留目标模型、把地址改成本地代理（`services/proxy.rs:694,8765` 的测试即断言此结果）。这覆盖前置条件检查与 `switch` 内部再判定之间 CC Switch 恰好开启接管的窗口（`services/provider/mod.rs:5778-5797`）。

验收：在 upsert 后、switch 中、live 写入后三个阶段分别注入失败，回读上表全部对象与快照一致。同一工具并发发起两次 apply，第二次排队完成且不挂起。故障注入前让另一进程改写 live 文件，WE2AI 不覆盖该文件并报告冲突（验收覆盖"比对前外部写入"，不覆盖不可消除的比对后窗口）。无外部写入时，在 Codex 规范化与 MCP 重投影之后注入失败，WE2AI 正常回滚且不误报冲突。数据库预置第二条供应商时 apply 被拒绝。CC Switch 开启代理接管后，WE2AI 的 apply 被拒绝且 live 文件不变。用测试钩子在前置检查之后、`switch` 之前开启接管，`switch` 返回 `TakeoverConflict`，apply 返回冲突而不是成功。验收使用 CC Switch 与 WE2AI 配置不同代理端口，逐字节核对 live 中 CC Switch 的原代理地址未变，并经该地址发起一次实际工具请求成功；且 CC Switch 写入的代理地址与 `PROXY_MANAGED` 占位认证仍在 live 文件中，WE2AI 数据库中该应用无 `proxy_live_backup` 记录；随后 CC Switch 解除接管，WE2AI 再次 apply 成功写入直连配置。

### 4.2 Claude Code 与 Codex

**保留用户已有配置。** 现有管道把供应商配置整份写成 live 文件（Claude `services/provider/live.rs:1312`、Codex `codex_config.rs:1125`），固定 id 反复切换也不会触发回填分支。因此 apply 每次都先读当前 live 文件作为基底，只覆盖托管字段，再把合并结果作为供应商配置交给管道：

| 工具 | 托管字段（WE2AI 覆盖） | 其余内容 |
|---|---|---|
| Claude Code | `env.ANTHROPIC_BASE_URL`、`env.ANTHROPIC_AUTH_TOKEN`、`env.ANTHROPIC_MODEL`、`env.ANTHROPIC_DEFAULT_{SONNET,OPUS,HAIKU}_MODEL`；并删除会与之冲突的 `env.ANTHROPIC_API_KEY` | hooks、permissions、其他 env、MCP 等原样保留 |
| Codex | 顶层 `model_provider`、`model`；整张 `[model_providers.we2ai]` 表 | 其他顶层键、其他 provider 表、`[mcp_servers]`、profiles 等原样保留；用 `toml_edit` 保持格式与注释 |

live 文件不存在时以空对象或空文档为基底。

**凭据文件保护（Unix）：** `~/.claude/settings.json`、`~/.codex/config.toml`、WorkBuddy `models.json` 写入后都含明文 Key。写入这些文件的路径很多：上游 Claude 与 Codex 写入器、Codex MCP 重投影（`mcp/codex.rs:458` 整份重写 `config.toml`）、WorkBuddy 写入器，它们都用"临时文件 + 原子替换"，临时文件按进程 umask 创建、写完 Key 之后才调整权限（`config.rs:425`、`localwrite.rs:110,135`）。逐个改写入调用无法穷尽，也会持续增加上游触点。因此**以目录权限为主防线**：

| 措施 | 规则 |
|---|---|
| 目录收紧（主防线） | apply 开始、快照之前，对三个**实际解析**的配置目录（Claude 用上游 `get_claude_config_dir()`、Codex 用上游 Codex 目录函数，二者都支持设置覆盖，`config.rs:37-42`、`codex_config.rs:461-467`；WorkBuddy 按第 4.3 节解析）依次执行：⓪ 目录不存在（首次使用）时以 0700 创建（含缺失的父级路径）；① 解析符号链接到真实目录；② 核对属主为当前用户；③ chmod 0700；④ 重新 stat 确认模式为 0700。其他本机用户因此无法进入目录，目录内任何临时文件、任何写入路径的中间状态都不可读。对同一用户运行的 Claude Code、Codex、WorkBuddy 没有影响 |
| 失败即停止 | 三个目录中任一目录的上述任一步失败（属主不是当前用户、无权 chmod、chmod 后模式不符），apply 在快照与任何写入之前停止，提示"配置目录 <路径> 不属于当前用户或无法设为仅本人可访问，已停止写入"。目录原本供多人共享或工具由其他用户运行的情况同样拒绝，不为其放宽 |
| 文件收紧（纵深） | 同一时点把已存在的 Claude 实际配置文件（`settings.json` 或旧版 `claude.json`，按上游路径函数解析）、`config.toml`、`auth.json`、`models.json` chmod 为 0600；apply 成功或回滚完成后，对这些文件再 chmod 0600。只收紧，不放宽 |
| WE2AI 自己的写入 | 追加写入（如 `requires_openai_auth`）、WorkBuddy 写入、回滚写回，一律用上游已有的 `atomic_write_private`（`config.rs:387-390`，临时文件创建即 0600）；回滚不调用上游快照的恢复函数（它先用普通写入写回再恢复原权限，`codex_config.rs:200,230`） |
| 上游写入调用 | 不改。v22 计划改的 `live.rs:1317`、`codex_config.rs:1136` 两处撤销，由目录收紧覆盖 |
| 自定义目录 | `WORKBUDDY_CONFIG_DIR` / `CODEBUDDY_CONFIG_DIR` 以及 Claude、Codex 的目录覆盖，均按上两行处理 |

验收：三个目录 apply 后为 0700；首次使用时三个目录均不存在，apply 成功创建三个 0700 目录并写入 0600 文件；负例一：用户目录只有旧版 `claude.json`，apply 后注入失败，`claude.json` 回滚到原字节且为 0600、未新建 `settings.json`；负例二：Codex 目录覆盖指向属主为其他用户的目录，apply 在写入前停止，目录内文件不变；预置 Codex MCP 记录后 apply，在写入过程中观察到的临时文件所在目录为 0700、最终文件为 0600；三阶段故障注入回滚后同样满足。

**外来密钥收敛：** 合并基底可能含用户自己的其他密钥（Codex 其他 `[model_providers.*]` 的 `experimental_bearer_token`、Claude 其他 env 密钥），而 `switch` 读取的是数据库行（`services/provider/mod.rs:5718`），合并结果必须先落库。`switch` 成功后立即再次 `save_provider`，把该行 `settings_config` 收敛为只含托管字段；下次 apply 仍从 live 重新取基底，不依赖数据库中的完整配置。固定 id 供应商的 `meta.common_config_enabled` 固定为 `Some(false)`，不能不设：为 `None` 时上游会回退到"当前配置是公共片段的超集就合并"（`services/provider/live.rs:541-556,678-699`）。验收：预置含 hooks、permissions、自定义 env 的 `settings.json` 和含 MCP、profiles 的 `config.toml`，预置含其他 provider 表 token 的 `config.toml`，apply 后数据库供应商行不含该 token。连续 apply 两次后：Claude 非托管字段的结构与值不变（现有 `write_json_file` 会对键排序并重新格式化，`config.rs:332`，不要求逐字节）；Codex 非托管内容的注释与格式保留。

托管字段的具体取值与 live 验收（数据库行在收敛后只保留"供应商内部配置"一列）：

| 工具 | 固定 id | 供应商内部配置 | live 文件验收 |
|---|---|---|---|
| Claude Code | `we2ai-claude` | `env.ANTHROPIC_BASE_URL`、`ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_MODEL`、`ANTHROPIC_DEFAULT_{SONNET,OPUS,HAIKU}_MODEL` | `~/.claude/settings.json` 对应字段 |
| Codex | `we2ai-codex` | `auth.OPENAI_API_KEY` + `[model_providers.we2ai]`（`we2ai` 不是保留名，不会被管道的保留名迁移改写，`codex_config.rs:2900-2915`），交给现有管道投影 | `config.toml` 含 `model_provider`、`model`、`base_url`、`wire_api="responses"`、provider 级 `experimental_bearer_token`；**Key 不出现在 `auth.json`**；用户已有的官方 ChatGPT 登录保留不删 |

依据：Codex 0.149 起第三方 Key 只写 config.toml（`src-tauri/src/codex_config.rs:3890-3902`）。

**保留 ChatGPT 登录：** 上游设置 `preserve_codex_official_auth_on_switch` 默认 false（`settings.rs:393-396`），为 false 时切到第三方会删除 `auth.json`（`codex_config.rs:3925`）。该设置还决定管道给活动 provider 表写入的 `requires_openai_auth`（`codex_config.rs:3960-3966`）。上游注释说明两点（`codex_config.rs:3300-3320`）：① 对带 `experimental_bearer_token` 的表，这个字段**不参与请求鉴权**，只影响 Codex 的登录界面；② `true` 而无登录会卡在登录页，`false` 而保留了 ChatGPT 登录会让 Codex 隐藏账户状态、不刷新这份登录的令牌。

Codex 的实际登录态无法可靠观察：凭据可能在 `auth.json`、系统钥匙串（`cli_auth_credentials_store = keyring / auto`）或只在内存（`ephemeral`），且用户随时可能在两次 apply 之间登录或登出。因此 WE2AI **不判定登录态**，采用与登录态无关的固定组合：

| 项 | 取值 | 效果 |
|---|---|---|
| `preserve_codex_official_auth_on_switch` | WE2AI 模式固定 true，初始化本地设置时写入，不在 `we2ai_save_settings` 可改字段内 | 管道永不删除 `auth.json`，用户的 ChatGPT 登录材料保留 |
| `[model_providers.we2ai].requires_openai_auth` | `switch` 之后、回读之前，由 `we2ai_apply_model` 用 `toml_edit` 只改 WE2AI 自己这张表，写为 `false`，覆盖上游按上一项盖的 `true` | Codex 使用 WE2AI 时不弹登录页，与用户是否登录 ChatGPT、凭据存哪里、何时登录登出都无关 |

已知限制：用户使用 WE2AI 期间，Codex 不显示 ChatGPT 账户状态，也不刷新那份 ChatGPT 令牌；日后切回官方 ChatGPT 时若令牌已过期需重新登录。这只影响 WE2AI 之外的官方登录体验，不影响 WE2AI 请求。

回读门（第 4.1 节）对 Codex 追加一项：`[model_providers.we2ai].requires_openai_auth == false`。H1 在这次追加写入之后计算。验收：① 预置已登录 ChatGPT 的 `auth.json`，apply 后该文件逐字节不变；② 分别在无 `auth.json`、仅含 `OPENAI_API_KEY`、仅含 `last_refresh` 等元数据、`cli_auth_credentials_store = keyring`、`ephemeral` 五种状态下 apply，`requires_openai_auth` 均为 false；③ 上述每种状态 apply 后实际启动一次 `codex` 并发出一次请求，均不出现登录页且请求走 WE2AI；④ apply 后用户在 Codex 中登录或登出 ChatGPT，再启动 `codex` 仍不出现登录页、请求仍走 WE2AI。

### 4.3 WorkBuddy

| 项 | 规则 |
|---|---|
| 目录 | `$WORKBUDDY_CONFIG_DIR` → `$CODEBUDDY_CONFIG_DIR` → `<home>/.workbuddy`；home 用 cc-switch 的 `dirs::home_dir()`，不读 `HOME`（Windows 上 Git/MSYS 的 `HOME` 可能错误，`src-tauri/src/config.rs:9-16`） |
| 条目模型 | 一个条目 = 一个模型（WorkBuddy 以 `id` 作模型名）。WE2AI 在自有数据库记录"上次托管的 id" |
| 托管记录 | 数据库记录上次写入条目的 `id` 与**内容指纹**（规范化 JSON 的 SHA-256） |
| 切换 | 按 id 找到旧条目并比较指纹。**新旧 id 不同**：指纹一致则删除旧条目；不一致说明被改过，保留并提示"检测到手工修改，未删除"；然后写入新条目。**新旧 id 相同**（同一模型再次指定，如换 Key）：指纹一致则原地替换；不一致时弹窗让用户选择"覆盖为 WE2AI 配置"或"取消"，不产生重复 id。新 id 与非托管条目重名时同样弹窗确认后覆盖 |
| 字段 | `id, name:"WE2AI <model>", vendor:"Custom", url:<网关>/v1, apiKey, useCustomProtocol:false`；能力字段仅在 B1 返回时写入，未返回则省略，键名映射：`supports_tool_call → supportsToolCall`、`supports_images → supportsImages`、`reasoning_efforts → supportsReasoning:true + reasoning.supportedEfforts` |
| 写入 | 读原数组并记录文件哈希 → 修改 → 替换前再算一次哈希，变化则重读重算（最多 3 次，仍冲突则报错）→ 原子替换；不触碰非托管条目。WorkBuddy 自身不使用文件锁，最终比对与替换之间的窗口无法消除，同 4.1 属于尽力检测 |
| 文件权限 | 配置目录收紧为 0700、`models.json` 读取前收紧为 0600（第 4.2 节"凭据文件保护"）；写入用 `atomic_write_private`，临时文件创建即 0600。参考写入器先按默认权限写临时文件再 chmod（`localwrite.rs:110,135`），且最终为 0644（`localwrite.rs:424`），不照搬 |
| 验收 | 写入过程中观察到的临时文件为 0600、所在目录为 0700；同一模型换 Key 再次指定，指纹一致时原地替换、文件中该 id 只有一条；手改后再次指定同一模型，出现确认弹窗，选取消则文件不变；新建文件与原为 0644 的已有文件，写入后均为 0600；写入后手改同 id 条目，再切换模型，手改条目保留且出现提示；读取之后、最终比对之前由另一写入器修改其他条目，该修改不丢失 |

WorkBuddy 事实（本机核实）：腾讯出品，`/Applications/WorkBuddy.app`，bundle id `com.tencent.workbuddy.mac`，5.5.3；`models.json` 为顶层数组。

### 4.4 安装检测

| 工具 | 检测 | 下载 |
|---|---|---|
| Claude Code | 复用 `get_tool_versions` | Anthropic 官方文档页 |
| Codex | 复用 `get_tool_versions` | openai/codex 发布页 |
| WorkBuddy | macOS 读 `/Applications/WorkBuddy.app/Contents/Info.plist` 版本；Windows 查 `%LOCALAPPDATA%\Programs\WorkBuddy\WorkBuddy.exe`；兜底查配置目录 | 国际 `workbuddy.ai/downloads`，国内 `workbuddy.cn/downloads/` |

## 5. 会话与安全

### 5.1 登录验证码（窗口内拦截，不经过系统协议）

```mermaid
sequenceDiagram
  participant D as 主窗口
  participant R as Rust captcha.rs
  participant W as 验证码 WebviewWindow
  participant P as SubPanel /desktop/captcha
  D->>R: 需要验证码(provider, region)
  R->>R: 生成 32 字节随机 nonce，登记 pending(一次性, 120s)
  R->>W: 打开 P?n=nonce，注册 on_navigation
  W->>P: 渲染官方组件
  P-->>W: window.location.assign(/desktop/captcha/done#n=&p=&proof)
  R->>R: on_navigation：校验 https、host=当前区域、路径、p 与 pending 一致、nonce，取消导航并一次消费
  R->>W: 关闭窗口
  R-->>D: 票据（仅内存，不写日志不发事件）
  D->>D: 立即提交登录或发短信
```

| 要点 | 规则 |
|---|---|
| 不走 `we2ai://` | 系统协议可被其他程序抢注（Windows 注册在 HKCU），截获 URL 即可抢先消费票据 |
| 一次性 | 每次发短信、每次登录各取一张新票据；pending 超时或窗口被关闭即作废 |
| 票据结构 | 与 B3 一致，按 provider 结构化，腾讯为 ticket+randstr 两字段，Rust 侧映射到对应请求字段 |
| 触发条件 | 公开设置中任一家开启才弹窗；当前线上均关闭 |
| 深链入口 | 注册 `we2ai://`，解析器 `deeplink/parser.rs` 改为接受 `we2ai`；WE2AI 模式下 provider、mcp、skill、prompt 四类导入全部拒绝（避免创建非 WE2AI 供应商），第一版深链只用于唤起窗口。三条入口（single-instance、`on_open_url`、macOS `RunEvent::Opened`）合并到同一分发函数，错误分支不回传原始 URL |

### 5.2 令牌

| 项 | 规则 |
|---|---|
| access_token | 只在内存 |
| refresh_token | `keyring` crate；service=`com.we2ai.desktop`，account=`{region}:{user_id}`；切区域加载该区域会话，没有则登录 |
| 会话索引 | `~/.we2ai` 下持久化非秘密索引 `{region → user_id, email_masked}`，登录成功时写入（用户 id 取自登录后 `GET /api/v1/user/profile`）；重启时按当前区域查索引再读钥匙串 |
| 轮转落盘 | 服务端轮转后旧 refresh 立即失效（B5 后为原子操作）。响应到达后先写钥匙串再使用新 access token；写入失败、或刷新途中崩溃导致新 token 丢失，状态定义为"需重新登录"，不承诺免登录 |
| 会话锁 | 每个 `{region}:{user_id}` 一把会话锁，refresh 与 logout 共用；钥匙串写入带 generation，旧请求不得覆盖或清除新 token |
| 单飞续期 | 同一时刻只有一个 refresh，其余请求等待其结果 |
| 失败分类 | 按接口区分（错误码见 `backend/internal/service/auth_service.go:28-48`、`session_binding.go:13`）：<br>**受保护接口**返回 `TOKEN_EXPIRED` 或 `ACCESS_TOKEN_EXPIRED`：access token 过期，触发单飞 refresh 后重放请求。JWT 中间件对过期 access token 返回的正是 `TOKEN_EXPIRED`（`backend/internal/server/middleware/jwt_auth.go:60-66`），不能当作会话终止。<br>**受保护接口**返回 `TOKEN_REVOKED`、`INVALID_TOKEN`、`USER_NOT_ACTIVE`、`BACKEND_MODE_ACTIVE`：终止会话；后者提示"平台当前仅允许管理员登录"。<br>**`/auth/refresh`** 返回 `REFRESH_TOKEN_INVALID`、`REFRESH_TOKEN_EXPIRED`、`REFRESH_TOKEN_REUSED`、`TOKEN_REVOKED`、`SESSION_BINDING_MISMATCH`、`USER_NOT_ACTIVE`，`BACKEND_MODE_ACTIVE`：终止会话。<br>终止会话 = 清钥匙串与索引、回登录页、不重试。网络错误、429、5xx 保留凭证并退避重试。其他未知 401 按终止会话处理 |
| 登出 | ① 置会话为"登出中"，拒绝新 refresh；② 等待在途 refresh 完成；③ 持最新 refresh_token 调 `/auth/logout`；④ 清钥匙串和索引，generation 递增使迟到的写入失效。仅当响应 `revoked:true` 才显示"已退出"，其余情况（网络失败、`revoked:false`、B4 未部署）显示"本地已退出，远端撤销未确认" |
| 免登录时长 | 文案写"默认最长 30 天，服务端策略、凭证撤销或网络指纹变化可能提前失效"（`backend/internal/service/session_binding.go:12-33`） |
| 钥匙串异常 | 锁定、不可用、拒绝访问时退化为本次会话内存保存，并提示 |
| API Key 落盘位置 | 复用供应商管道意味着明文 Key 会进入：① 三个工具的 live 配置文件；② WE2AI 数据库供应商表的 `settings_config`（`database/dao/providers.rs:219,249`）；③ 数据库定期备份（`database/backup.rs:412`）。v1–v8 的"数据库不存 Key"与此矛盾，予以删除 |
| 落盘权限 | Unix：`~/.we2ai` 目录 0700，数据库与备份文件 0600，启动时校验并收紧；Windows：数据根位于用户目录，继承当前用户 ACL，不额外处理 |
| 外来密钥 | apply 过程中用户 live 文件里的其他密钥会短暂进入数据库行，`switch` 成功后立即收敛清除（第 4.2 节）；若恰在此窗口触发定期备份，备份中会带上这些密钥，登出时随备份一并删除。列为已知限制 |
| 登出处理 | 登出时清空 WE2AI 数据库中两条固定供应商行的整个 `settings_config`、删除 `proxy_live_backup` 表中 Claude 与 Codex 的记录，并删除 `~/.we2ai/backups` 下全部备份；工具 live 文件中的 Key 保留（否则工具立即不可用），登出确认弹窗说明这一点，并提供"同时从工具配置中移除 Key"勾选项 |
| Key 吊销 | 客户端不吊销 Key；需要作废时去网页端删除或禁用 Key |

## 6. 品牌与数据隔离

### 6.1 数据目录（阻断项修复）

cc-switch 在运行时代码里有 4 处独立写死 `~/.cc-switch`（2026-09-23 全仓扫描，排除测试代码与临时文件前缀）。只改 identifier 会让两个应用共享数据库、本地设置和备份：

| 位置 | 用途 |
|---|---|
| `config.rs:264,277` `get_app_config_dir()` | 数据库、日志等主数据根 |
| `settings.rs:583` `AppSettings::settings_path()` | 本地设置（含每个应用的本地 current 供应商、工具目录覆盖），每次切换都会写（`services/provider/mod.rs:5963`） |
| `panic_hook.rs:28` | setup 之前的崩溃日志回退目录 |
| `services/env_manager.rs:71` `get_backup_dir()` | 环境变量备份目录 |

| 项 | 规则 |
|---|---|
| 数据根 | `get_app_config_dir()` 在 WE2AI 模式返回 `~/.we2ai`，覆盖数据库、设置、日志、备份、OAuth marker |
| 迁移 | 不从 `~/.cc-switch` 自动导入任何数据 |
| 实现 | 新增 `we2ai::mode::data_root()`，上表 4 处在 WE2AI 模式下都改为调用它。其中 `get_app_config_dir()` **最先**返回 `~/.we2ai`，早于 Store override 与 Windows 旧目录回退（`config.rs:259`）；panic hook 在 setup 之前安装，其初始化前回退目录同步改为 `~/.we2ai`（`panic_hook.rs:24`、`lib.rs:347`） |
| 验收 | 负例一：预置指向 `~/.cc-switch` 的 Store override，WE2AI 仍写 `~/.we2ai`；负例二：setup 前触发 panic，日志落在 `~/.we2ai`；负例三：CC Switch 已有 `settings.json`（含自定义 Claude/Codex 目录），WE2AI 指定模型前后该文件内容与修改时间不变，WE2AI 写入的是默认工具目录 |
| 守卫 | `check-guards.sh` 扫描 `src-tauri/src` 中的 `.join(".cc-switch")`，按 `#[cfg(test)]` 模块边界排除测试代码（`database/backup.rs:1202`、`services/skill.rs:6329,6736` 位于测试模块内），命中上表以外的位置即失败，拦截上游新增的写死路径 |

### 6.2 启动白名单

`lib.rs` 启动时会导入工具配置、初始化供应商、恢复代理、启动同步任务（`lib.rs:534-541,727-782,1141-1148,1257-1264`）。WE2AI 模式下：

| 启动项 | 处理 |
|---|---|
| 数据库初始化、托盘、窗口、更新器、深链 | 保留 |
| 首次运行导入现有工具供应商 | 禁用 |
| 官方供应商种子 `init_default_official_providers()`（`lib.rs:776`、函数定义 `database/dao/providers.rs:697`、种子数据 `database/dao/providers_seed.rs`） | 禁用 |
| 累加模式每次启动导入供应商 | 禁用 |
| 默认 Skills 初始化、Skills 扫描 | 禁用 |
| 表为空时导入 MCP、导入提示词 | 禁用 |
| 本地代理恢复与接管（`lib.rs:1260`） | 禁用 |
| 启动时的接管残留恢复 `recover_from_crash()`（`lib.rs:1235-1243`） | 禁用。CC Switch 开启代理接管时 live 文件含 `PROXY_MANAGED` 占位符（`services/proxy.rs:2729-2774`），WE2AI 若执行恢复会改写 CC Switch 正在接管的文件 |
| 退出时的 live 恢复 `stop_with_restore_keep_state()`（`lib.rs:1916-1928`） | 禁用，理由同上 |
| WebDAV/S3 同步、会话索引、用量轮询 | 禁用 |
| 从用户 live 抽取公共配置片段 `initialize_common_config_snippets`、`scrub_leaked_gemini_common_config`（`lib.rs:1247-1257`） | 禁用，避免把用户 hooks、permissions 等非托管内容落进 WE2AI 数据库与备份 |
| 托盘供应商切换菜单 | 隐藏 |
| 命令注册表 | 上游 `generate_handler!` 列表一行不动；`we2ai_*` 命令放在 WE2AI 自己的第二个 `generate_handler!` 中 |
| 命令分发 | **默认拒绝的 IPC 白名单**：`.invoke_handler(tauri::generate_handler![...])` 改为 `.invoke_handler(we2ai::mode::gate(we2ai_handler, tauri::generate_handler![...]))`。`gate` 读取 `invoke.message.command()`（Tauri `ipc/mod.rs:543`；本节 Tauri 行号取自本机 registry 的 tauri-2.11.5 源码，`src-tauri/Cargo.lock` 当前锁定 2.10.3，两版结构相同，上游升级后需复核）：`we2ai_*` 交给 WE2AI handler；白名单内的上游命令交给上游 handler；其余调用 `resolver.reject(...)` 后返回 `true`，避免框架再报一次 not found |
| 插件命令 | `plugin:*` 命令走插件分发，不经 `invoke_handler`（Tauri 2 `webview/mod.rs:1811-1912`），gate 管不到，由 `src-tauri/capabilities/default.json` 控制。当前只开放 opener、updater、log、process、dialog、window 子集，未开放 store、fs。守卫对 capabilities 文件做权限白名单，上游新增权限即失败 |

IPC 白名单（WE2AI 模式唯一可调用的命令）：

| 类别 | 命令 |
|---|---|
| WE2AI 自有 | 全部 `we2ai_*`：登录、登出、会话、Key、模型、`we2ai_apply_model`、安装检测、验证码，以及 `we2ai_get_settings`、`we2ai_save_settings` |
| 上游启动必需 | `get_init_error`、`get_migration_result`、`get_skills_migration_result`、`set_window_theme` |
| 上游功能 | `get_tool_versions`、`check_for_updates`、`install_update_and_restart`、`restart_app`、`open_external`、`get_auto_launch_status`、`set_auto_launch` |
| 窗口控制 | 走 `plugin:window`，受 capabilities 管控，不在此表 |

设置读写不放行上游 `get_settings` / `save_settings`：gate 只能读 payload、不能改写（`ipc/mod.rs:498-509`），而 `save_settings` 会整体覆盖 `AppSettings`，包括本地 current 供应商与工具目录覆盖（`commands/settings.rs:62-71`）。改由 `we2ai_get_settings` 只返回主题、语言、窗口、开机自启字段；`we2ai_save_settings` 读取现有设置、只覆盖这些字段后保存。

前端：WE2AI 模式下不调用 `syncModelsDevPricingOnStartup`（`src/main.tsx:24,135`）等上游启动任务；实施时以"启动日志中无 IPC 拒绝记录"为准逐一补齐白名单。

所有供应商命令（`add/update/delete/switch_provider`）、导入导出（`import_config_from_file`、`restore_db_backup` 等）、MCP、Skills、Prompts、代理、同步命令都不在白名单内。托管记录只能经 `we2ai_apply_model` 修改，它在 Rust 内部调用服务层，不经过 IPC，因此不受白名单影响。上游以后新增的命令默认被拒，无需逐个跟进。

命令层验收：直接 `invoke` 以下调用均返回错误，数据库与工具 live 文件不变——`add_provider`（非 WE2AI id）、`update_provider`（固定 id + 非 WE2AI 地址）、`add_provider`（固定 id + 错误 app）、`switch_provider`、`import_config_from_file`、`restore_db_backup`；`get_settings`、`save_settings` 被拒；`we2ai_save_settings` 携带工具目录字段时该字段被忽略。守卫：单测断言白名单外的任一已注册命令被拒。全流程验收：启动、登录、指定模型、切换主题、检查更新、开关开机自启、登出，全程无 IPC 拒绝日志。

实现：新增 `we2ai::mode::startup_allowed(task) -> bool`，在上述每个调用点前加一行判断（起点 `lib.rs:852` 导入链，实施时逐个枚举调用点并登记到功能列表）。验收：全新数据根首次启动、第二次启动各一次，确认数据库中供应商表只有 WE2AI 写入的记录，MCP、提示词、Skills、`config_snippets` 表为空，无代理监听端口。CC Switch 开启代理接管后启动并退出 WE2AI，Claude 与 Codex 的 live 文件逐字节不变。

### 6.3 改名清单

| 位置 | 现值 | 新值 |
|---|---|---|
| `tauri.conf.json` productName / identifier / scheme | CC Switch / com.ccswitch.desktop / ccswitch | WE2AI / com.we2ai.desktop / we2ai |
| `src-tauri/src/deeplink/parser.rs` 协议校验 | ccswitch | we2ai（WiX 模板从 `tauri.conf.json` 的 schemes 取值，无需改 wxs） |
| `lib.rs` 深链前缀、启动日志、tooltip | ccswitch:// / CC Switch | we2ai:// / WE2AI |
| `tray.rs` 主页链接 | ccswitch.io | we2ai.com |
| `src-tauri/src/auto_launch.rs` 开机自启注册名 | CC Switch（`auto_launch.rs:20,34`） | WE2AI |
| `src-tauri/icons/*` | 上游 | `pnpm tauri icon assets/designs/appicon.png` |
| `src/index.html` favicon、登录页与关于页 logo | 上游 | `assets/designs/favicon.svg`、`web-logo.svg`；**备注**：登录页与上游关于页（`AboutSection.tsx`）在 WE2AI 模式下均不挂载（P0 只有 `We2aiShell` 渲染），本期 `We2aiShell` 的顶栏与关于卡片已经使用 `web-logo.svg`，上游 `AboutSection.tsx` 里的 `app-icon.png` 暂不替换，延后到该页面真正启用时再处理 |
| i18n 与 12 个前端文件中的品牌文字 | CC Switch | WE2AI |
| updater endpoints 与公钥 | fork 已有 | 不变；identifier 不影响验签 |
| `.github/workflows/release.yml` DMG 打包步骤 | `.app` 名、卷名、图标目标写死为 CC Switch（`release.yml:324` 附近） | WE2AI；产物文件名同步改为 WE2AI 前缀 |
| `.github/workflows/release.yml` Windows 便携版 | 直接复制 `cc-switch.exe` 进 ZIP（`release.yml:468`） | ZIP 内可执行文件重命名为 `WE2AI.exe` |

identifier、productName、scheme 在首次发版前冻结，之后不再改。

## 7. 上游同步

上游文件触点（v1 的"≤3 处入口"低估了改动面，已废弃）：

| 文件 | 触点 | 冲突预期 |
|---|---|---|
| `src/App.tsx` | WE2AI_MODE 时渲染 `We2aiShell`，其余视图入口不渲染 | 高 |
| `src-tauri/src/lib.rs` | `mod we2ai`、`gate(we2ai_handler, ...)` 包裹上游 `generate_handler!`、启动与退出白名单判断（约 8 处）、深链前缀 | 高 |
| `src/main.tsx` | WE2AI 模式跳过上游启动任务 | 中 |
| `src-tauri/src/config.rs`、`settings.rs`、`panic_hook.rs`、`services/env_manager.rs` | 数据根（第 6.1 节 4 处） | 中 |
| `src-tauri/src/services/provider/mod.rs` | 热切换拒绝一行（第 4.1 节） | 高（上游高频改动文件，每次同步核对该行仍在接管判定之后） |
| `src-tauri/src/tray.rs`、`auto_launch.rs` | 品牌、隐藏托盘切换菜单 | 低 |
| `src-tauri/capabilities/default.json` | 不改，受守卫监控 | 低 |
| `tauri.conf.json` | 品牌与 scheme | 中 |
| `src-tauri/src/deeplink/parser.rs`、深链导入分发 | 协议名、WE2AI 模式拒绝导入 | 中 |
| `src-tauri/Cargo.toml` | `keyring` 依赖 | 低 |
| 品牌文字文件 | 第 6.3 节 | 低 |
| `.github/workflows/release.yml` | 打包命名 | 中（上游改发版流程时冲突） |

守卫从"数标记"改为校验行为：

| 守卫 | 检查方式 |
|---|---|
| 数据根 | 单测断言 WE2AI 模式下 `get_app_config_dir()` 以 `.we2ai` 结尾，含存在 override 的负例 |
| 启动白名单 | 单测断言禁用项返回 false |
| scheme 唯一 | `tauri.conf.json` 只有 `we2ai`；单测断言 `we2ai://` 能解析、`ccswitch://` 被拒、WE2AI 模式下导入请求被拒 |
| 品牌残留 | 用户可见字符串中无 `CC Switch` |
| 写死路径 | 非测试代码中 `.join(".cc-switch")` 只出现在第 6.1 节登记的 4 处 |
| IPC 白名单 | 单测断言白名单外的任一已注册命令被拒 |
| 插件权限 | `capabilities/default.json` 权限集与登记清单一致，上游新增权限即失败 |

每次上游合并后执行：无提交合并演练 → 守卫 → 同时启动 CC Switch 与 WE2AI，确认数据库、日志、窗口状态互不影响。

## 8. 实施阶段与验收

| 阶段 | 交付 | 验收 |
|---|---|---|
| P0 壳、品牌、隔离 | 6.1–6.3；WE2AI_MODE；隐藏视图；启动与退出白名单；IPC gate | 两个应用同时运行互不影响；`~/.cc-switch` 无新写入；第 6.2 节命令层验收通过；守卫通过 |
| P1 SubPanel 改动 | B1–B7（B3 为前端路由，其余为后端）+ 契约测试（真实 Gin router，含分页、2FA、B1 全部负例、登出撤销家族、并发 refresh、登出后旧 access token 401、四种错误响应形态（含认证路由与受保护路由两种 429 均保留凭证并退避重试）、后端模式错误码） | SubPanel 单测通过；在 jiwu 测试环境部署 |
| P2 登录会话 | 5.1、5.2 | 重启免登录；升级版本后续期不掉线；并发 10 个请求同时过期（受保护接口返回 `TOKEN_EXPIRED`）只发生一次 refresh 且会话保留；断网不回登录页；钥匙串写入失败进入"需重新登录"；refresh 返回 `REFRESH_TOKEN_REUSED` 或 `REFRESH_TOKEN_EXPIRED` 后钥匙串已清空、界面回到登录页且不再重试；登出后旧 refresh 无效；三家验证码各自开启时发短信与登录均通过 |
| P3 Key 与模型广场 | Key 分页拉取、B1 渲染 | 不同分组 Key 显示不同模型与按钮 |
| P4 写入 | 4.1–4.4 | 三工具各指定两次，live 文件托管字段符合第 4 节、非托管内容不变；三阶段故障注入后快照清单全部回到原状；同工具并发 apply 不挂起；WorkBuddy 手改条目保留；Windows 下错误 `HOME` + 正确 `USERPROFILE` 仍写对目录 |
| P5 收尾 | 功能列表、守卫、macOS/Windows 安装升级卸载并存矩阵、发版；检查实际产出的 DMG 内 `.app` 名与卷名为 WE2AI、Windows 便携版 ZIP 内为 `WE2AI.exe`，并与 CC Switch 并存安装；两款应用分别开启、查询、关闭开机自启互不影响；Unix 下 `~/.we2ai` 为 0700、数据库与备份为 0600；登出后数据库供应商行与 `proxy_live_backup` 表均无 Key、备份目录为空 | `pnpm test`、`cargo test` 通过 |

分支：cc-switch 从 `we2ai` 切 `feature/we2ai-client`；SubPanel 从 `zhiguofan` 切 `feature/desktop-api`，按其 CHANGELOG 与功能列表规则提交。

## 9. 风险

| # | 风险 | 处理 |
|---|---|---|
| R1 | B1 能力计算与网关实际行为漂移 | B1 复用网关路由判定函数，不另写一套；契约测试逐工具实际请求一次 |
| R2 | 国内版正式环境未上线 | 开发期对 jiwu 联调；上线后回归登录、Key 列表、B1 |
| R3 | App.tsx、lib.rs 上游改动频繁 | 触点集中到 `We2aiShell` 与 `we2ai::mode`，每次同步走第 7 节演练 |
| R4 | WorkBuddy Windows 安装路径未验证 | 兜底查配置目录；在 Windows 实机验证 |
| R5 | 钥匙串在部分 Linux 桌面不可用 | 退化为内存会话并提示 |

## 10. 与 SubPanel 的仓库关系

两仓库分离，客户端继续跟 cc-switch 上游，与 SubPanel 通过接口契约联动。

| 选项 | 结论 | 理由 |
|---|---|---|
| A 分离并跟上游 | **采用** | 白拿上游对工具写入、检测、平台兼容的持续修复 |
| B 分离并脱钩 | 备选 | 触发条件：单次同步冲突超过 1 小时，或上游两次以上破坏 WE2AI 入口 |
| C 搬进 SubPanel | 不采用 | 需多养 Rust/Tauri 工具链与签名发版，每次推送过 SubPanel pre-push 与 e2e |

SubPanel 侧联动：

| # | 动作 | 验收 |
|---|---|---|
| 1 | `API_DOCUMENTATION.md` 新增"桌面客户端依赖接口"，与本方案 3.3 一致 | 清单一致 |
| 2 | 契约测试走真实 Gin router，校验 envelope、分页、2FA、B1 跨 Key 负例 | 字段改名或行为变化时测试失败 |
| 3 | 识别 `X-We2ai-Client` 头用于日志与统计 | 日志可见 |
| 4 | 两仓库功能列表互相登记 | 任一侧同步上游时可见对面依赖 |

## 附录：评审记录

每轮意见均逐条核实代码证据后处理，版本号与轮次对应关系见"处理"列。

| 轮次 | 评审者 | 结论 | 处理 |
|---|---|---|---|
| 1 | Codex（gpt-5.6-sol） | FAIL：阻断 2、高 7、中 4 | 13 条全部采纳，形成 v2 |
| 2 | Codex（gpt-6-sol） | FAIL：第 1 轮 4 条已解决、9 条部分解决；新增阻断 1、高 5、中 3 | 9 条全部采纳，形成 v3 |
| 3 | Codex（gpt-6-sol） | FAIL：第 2 轮 6 条已解决、3 条部分解决；剩高 3、中 3，无阻断 | 6 条全部采纳，形成 v4 |
| 4 | Codex（gpt-6-sol） | FAIL：第 3 轮 2 条已解决、4 条部分解决；阻断 1、高 2、中 2 | 5 条全部采纳，形成 v5；跨进程两条按"无法协调则收窄保证"处理 |
| 5 | Codex（gpt-6-sol） | FAIL：第 4 轮 3 条已解决、2 条部分解决；阻断 1、中 2 | 3 条采纳形成 v6；H1 未按"从写入层传出字节"实现，改为切换后回读，避免改上游写入层 |
| 6 | Codex（gpt-6-sol） | FAIL：第 5 轮 2 条已解决、1 条部分解决；阻断 1、高 1 | 2 条采纳形成 v7；消费空档改用"消费与签发同一脚本"消除，未采用"已消费 token 映射" |
| 7 | Codex（gpt-6-sol） | FAIL：第 6 轮 2 条部分解决；阻断 1、高 1、中 1 | 3 条采纳形成 v8；第 6 轮拒绝的"已消费 token 映射"被证明必要（响应丢失场景），改为采纳并加复用检测 |
| 8 | Codex（gpt-6-sol） | FAIL：第 7 轮 3 条已解决；高 1、中 2 | 3 条采纳形成 v9 |
| 9 | Codex（gpt-6-sol） | FAIL：第 8 轮 3 条已解决；高 1、中 2 | 3 条采纳形成 v10；并主动全仓扫描写死路径，一次补齐 4 处 |
| 10 | Codex（gpt-6-sol） | FAIL：第 9 轮 2 条已解决、1 条部分解决；高 2 | 改为 IPC 默认拒绝白名单，一次覆盖两条及上游未来新增命令，形成 v11 |
| 11 | Codex（gpt-6-sol） | 未完成：Codex 用量额度耗尽 | 改由 Fable 5 先行评审 |
| 11 | Fable 5（claude-fable-5-1） | FAIL：高 1、中 3、低 3；确认 IPC 白名单在 Tauri 2 可行，前 10 轮修复无回退 | 7 条全部采纳，形成 v12 |
| 12 | Fable 5（claude-fable-5-1） | **PASS**：上轮 6 条已解决、1 条部分解决；剩中 1、低 6 | 全部采纳，形成 v13 |
| — | Claude 自查 | 修正 9 处叠加修改造成的前后不一致 | v13 内修订 |
| — | Sonnet 5（claude-sonnet-5） | **PASS**：仅 6 处行号偏移，引用对象无误；确认 4 处写死路径无遗漏、IPC gate 在 Tauri 2 可行 | 行号全部修正 |
| 12 | Codex（gpt-6-sol） | FAIL：Codex 第 10 轮与 Fable 第 1 轮意见 8 条已解决、1 条部分解决；高 1、中 2 | 3 条采纳形成 v14 |
| 13 | Codex（gpt-6-sol） | FAIL：第 12 轮 2 条已解决、1 条部分解决；高 1、中 2 | 3 条采纳形成 v15；v14 把 `TOKEN_EXPIRED` 归为终止会话是错误分类，已按接口区分 |
| 14 | Codex（gpt-6-sol） | FAIL：第 13 轮 2 条已解决、1 条部分解决；高 1、中 2 | 3 条采纳形成 v16；新增 B7 |
| 15 | Codex（gpt-6-sol） | FAIL：第 14 轮 3 条均部分解决；高 1、中 3 | 4 条采纳形成 v17；接管竞态从"事后回滚"改为"热切换拒绝"根治，新增上游触点一处 |
| 16 | Codex（gpt-6-sol） | **PASS**：第 15 轮 4 条全部解决；剩中 3 | 3 条采纳形成 v18：保留 ChatGPT 登录、第四种错误形态、后端模式管理员手机入口表述收窄 |
| 终验 1 | Fable 5（claude-fable-5-1） | FAIL：高 1、低 2；高项由 v18 新改动引入（保留登录开关固定为 true 会让无 ChatGPT 登录用户卡在 Codex 登录页） | 3 条采纳形成 v19：保留开关改为按 `auth.json` 登录凭据动态决定；按流程回到 Codex 复审 |
| 17 | Codex（gpt-6-sol） | FAIL：高 2；Codex 登录态无法仅凭 `auth.json` 判定（keyring、auto、ephemeral），且登录态会在两次 apply 之间变化 | 放弃动态判定，形成 v20：保留开关固定 true、WE2AI 表 `requires_openai_auth` 在 switch 后固定写 false，与登录态无关 |
| 18 | Codex（gpt-6-sol） | FAIL：第 17 轮 2 条已解决；阻断 1（Claude、Codex live 文件含明文 Key 但权限未收紧）、中 1 | 2 条采纳形成 v21：umask 077 + apply 前 chmod 0600 + 追加写入用 `atomic_write_private`；B1 增加 Key 准入状态 |
| 19 | Codex（gpt-6-sol） | FAIL：第 18 轮 2 条部分解决；高 1、中 2 | 3 条采纳形成 v22：`callable` 定义为网关完整准入结果；放弃进程级 umask，改为两处凭据写入调用用私有写入，回滚后收紧权限；Key 列表保留配额耗尽状态 |
| 20 | Codex（gpt-6-sol） | FAIL：第 19 轮 1 条已解决、2 条部分解决；阻断 1、中 1 | 2 条采纳形成 v23：回滚不再调用上游快照恢复，改为 WE2AI 私有写回原字节并在 apply 开始时先收紧权限；B1 不按状态过滤，`blocked_reason` 统一为业务错误码 |
| 21 | Codex（gpt-6-sol） | FAIL：第 20 轮 1 条已解决、1 条部分解决；阻断 1、高 1、中 2 | 4 条采纳形成 v24：凭据保护改为以目录 0700 为主防线，覆盖所有写入路径，撤销两处上游写入改动；`switch` 中途失败单列规则；快照由 WE2AI 自行采集原字节 |
| 22 | Codex（gpt-6-sol） | FAIL：第 21 轮 3 条已解决、1 条部分解决；高 1、中 1 | 2 条采纳形成 v25：Claude 配置按上游路径函数解析并固定（覆盖旧版 `claude.json`）；三个目录统一"解析、核对属主、收紧、复核，任一失败即停止" |
| 23 | Codex（gpt-6-sol） | **PASS**：第 22 轮 2 条全部解决；剩中 1 | 采纳形成 v26：配置目录不存在时先以 0700 创建再复核 |
| 终验 2 | Fable 5（claude-fable-5-1） | **PASS**：上次终验高项已正确解决，v19–v26 设计一致可实施；剩中 1、低 3 | 全部采纳形成 v27 终版：`common_config_enabled` 固定 `Some(false)` 并禁用公共片段抽取、current 清空 SQL、守卫按测试模块边界排除；代码行号在 P1 切分支时统一复核 |
