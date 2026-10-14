# 网关问题：doubao-seedance-2.0 / 2.5 全部请求返回 502

> 报告时间：2026-10-14（服务端时间见下表 `Date`）｜环境：`https://api.we2ai.com`｜Key：分组含 Seedance 的 Key（已脱敏）
> 用途：交给网关运维排查。客户端（WE2AI 工坊）无需改动，网关修复后按第 6 节验收。

## 1. 摘要

| 项 | 内容 |
|---|---|
| 现象 | `POST /v1/videos` 选 `doubao-seedance-2.0` 或 `doubao-seedance-2.5`，**任何请求体**都返回 `502 {"error":{"message":"Upstream request failed","type":"upstream_error"}}` |
| 范围 | 4 个 Seedance 模型中 2 个故障：**2.0 与 2.5**；`2.0-fast`、`2.0-mini` 正常 |
| 关键特征 | 502 发生在**参数校验之前**：非法时长 999 本应返回 400，实际返回 502；而 fast / mini 同样请求返回 400 `InvalidParameter` |
| 响应速度 | 176–654 ms，是快速失败，不是超时 |
| 影响 | 若用户在工坊选到这两个模型，只会看到“网关服务异常”，无法出片；不产生任务，不计费 |
| 结论级别 | 现象与对照为实测；原因为推测，见第 5 节 |

## 2. 各模型对照

同一请求体（纯文本 + `duration:999`，必然非法，不会创建任务）：

| 模型 | HTTP | 响应 |
|---|---|---|
| `doubao-seedance-2.0` | **502** | `{"error":{"message":"Upstream request failed","type":"upstream_error"}}` |
| `doubao-seedance-2.5` | **502** | 同上 |
| `doubao-seedance-2.0-fast` | 400 | `InvalidParameter: the parameter duration specified in the request is not valid for model doubao-seedance-2-0-fast in t2v` |
| `doubao-seedance-2.0-mini` | 400 | `InvalidParameter: the parameter duration specified in the request is not valid for model doubao-seedance-2-0-mini in t2v` |
| `doubao-seedance-2.0-nonexistent`（不存在） | 404 | `Model "doubao-seedance-2.0-nonexistent" is not supported by any configured account in this group`（`model_not_found`） |

要点：
- 不存在的模型走 404，说明 2.0 与 2.5 **在分组里是有账号配置的**（否则也是 404），问题出在调用上游这一步。
- fast / mini 的 400 带火山方舟原生的 `InvalidParameter` 与上游 Request id，说明请求确实到达了方舟；2.0 / 2.5 没有任何上游信息，像是在到达方舟之前或被方舟拒绝后被网关统一改写成了 502。
- mini 实测可以完整出片（5 秒 480p，约 115 秒，`status:succeeded`），其上游模型 id 为 `doubao-seedance-2-0-mini-260615`，可见网关把别名映射到了带日期版本的方舟模型 id。

## 3. 复现

最小复现（免费，不会创建任务）：

```bash
curl -sS -i https://api.we2ai.com/v1/videos \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"doubao-seedance-2.0","content":[{"type":"text","text":"x"}],"duration":999,"ratio":"16:9","resolution":"480p"}'
```

| | 期望（与 mini 一致） | 实际 |
|---|---|---|
| 状态码 | 400 | **502** |
| 响应体 | `InvalidParameter ... not valid for model doubao-seedance-2-0 ...` | `Upstream request failed` |

把 `model` 换成 `doubao-seedance-2.0-mini` 即得到对照结果。

## 4. 完整抓取记录

请求体统一含 `"ratio":"16:9","watermark":false,"generate_audio":false`；A、B、C 另带一张 256×256 PNG 首帧（`image_url` + `"role":"first_frame"`，base64 共 184898 字符，已省略）。

| 编号 | 请求 | 服务端 Date | HTTP | 耗时 | x-request-id | 响应体 |
|---|---|---|---|---|---|---|
| A | `doubao-seedance-2.0`，`duration:5`，`resolution:480p`，带首帧 | Wed, 14 Oct 2026 10:49:42 GMT | **502** | 654 ms | `a68902ca-962f-455d-ae60-ba5347471bdb` | `{"error":{"message":"Upstream request failed","type":"upstream_error"}}` |
| B | **对照** `doubao-seedance-2.0-mini`，同 A | Wed, 14 Oct 2026 10:49:43 GMT | **400** | 821 ms | `08ccd95c-c052-41cb-8c16-960bd75e6491` | `{"error":{"code":"InvalidParameter","message":"expected the width to be at least 300px, but received a 256x256px image instead Request id: 0217919749831508576152ffdd4e725b7d51a061031c1fa522109","param":"image_url","type":"BadRequest"}}` |
| C | `doubao-seedance-2.0`，`duration:999`，带首帧 | Wed, 14 Oct 2026 10:49:44 GMT | **502** | 381 ms | `01919636-87b1-4e99-8878-2d673ce5851e` | `{"error":{"message":"Upstream request failed","type":"upstream_error"}}` |
| D | `doubao-seedance-2.0`，纯文本，`duration:5`，`resolution:9999p` | Wed, 14 Oct 2026 10:49:45 GMT | **502** | 176 ms | `39ba918a-87af-410a-8f56-8ab11e7ef45c` | `{"error":{"message":"Upstream request failed","type":"upstream_error"}}` |
| E | `GET /v1/videos/cgt-00000000000000-nonexist`（鉴权与连通性基线） | Wed, 14 Oct 2026 10:49:45 GMT | **404** | 195 ms | `ee2d8e41-609e-4e75-8860-9669137e9399` | `{"error":{"message":"Video request not found","type":"not_found_error"}}` |

响应头只保留了 `server: openresty`、`content-type: application/json; charset=utf-8`、`x-request-id`。`GET /v1/models` 返回 200，且列表包含 `doubao-seedance-2.0` 与 `doubao-seedance-2.5`。

## 5. 可能原因（推测，需运维核对）

| # | 推测 | 依据 | 核对方式 |
|---|---|---|---|
| 1 | 这两个别名映射到的方舟模型 id 错误或已下线（fast / mini 的映射是正常的） | 502 在参数校验前发生；mini 的真实 id 带日期后缀 | 对比三个渠道的“模型重定向 / 上游模型名”配置 |
| 2 | 渠道使用的方舟账号未开通这两个模型 | 其他两个模型同账号体系可用 | 在方舟控制台查看账号的模型开通状态与配额 |
| 3 | 渠道 Key / 区域对 2.0、2.5 无权限 | 同上 | 用渠道 Key 直连方舟 `POST /api/v3/contents/generations/tasks` 测这两个模型 |
| 4 | 网关把该渠道上游的任何非 2xx 都改写成 502，掩盖了真实错误 | 非法参数本应是 400，却是不带细节的 502 | 按下面的 request id 查网关日志里的上游原始响应 |

## 6. 请网关侧处理与验收

| 动作 | 责任方 | 输入 | 验收标准 |
|---|---|---|---|
| 按 A、C、D 的 `x-request-id` 查日志，找出上游原始响应 | 网关运维 | 第 4 节的 3 个 request id | 能说明 502 的真实上游错误 |
| 修正渠道的模型映射 / 方舟账号开通状态 | 网关运维 | 第 5 节核对结果 | 第 3 节的最小复现返回 **400 `InvalidParameter`**（与 mini 一致） |
| 最小出片验证（**会计费**）：`doubao-seedance-2.0`，纯文本，`duration:5`，`ratio:16:9`，`resolution:480p`，`generate_audio:false` | 网关运维或我方 | 一个正常的 Key | 返回 `{"id":"cgt-..."}`，轮询 `running → succeeded`，`/content` 返回 `video/mp4` |
| 2.5 重复上两步 | 同上 | 同上 | 同上 |
| 若暂不修复：把这两个模型从 Key 的可用模型里下线，或标注不可用 | 网关运维 | — | 客户端不再列出它们，用户不会选到 |

## 7. 客户端侧情况

- 工坊是否列出这两个模型，取决于服务端是否把它们标为视频（`kind=video`；运营未标注 `mode` 的视频模型会被当成文本，见 `B1定价契约.md`）。我方未核实这两个模型的归类；若被列出，选择后提交即返回 502，界面显示“网关服务异常，请稍后重试”。
- 客户端不需要改动：错误按状态码映射，没有读取上游响应体。修复后无需发版。
- 我方已用 `doubao-seedance-2.0-mini` 完成端到端联调（含首帧图、轮询、下载），结论见 `AI营销工坊设计方案.md` 第 2、13 节。
- 修复后我方会用第 3 节的命令和第 6 节的最小出片各复测一次，并把设计方案第 13 节第 13 项更新为已验证。
