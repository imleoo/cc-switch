# CLAUDE.md — we2ai fork

本仓库是 `farion1231/cc-switch` 的 fork，定制分支为 `we2ai`。

## 分支与同步

- `main`：只 fast-forward 对齐 `upstream/main`，**禁止**提交任何 fork 改动
- `we2ai`：fork 定制与唯一发布分支，所有开发在这里（或从它切 feature 分支）
- 同步上游：先 `/sync-upstream` 做风险分析，再 `./scripts/we2ai/sync-upstream.sh`
- fork 差异、风险表、排除类别、同步记录：`自定义开发功能列表.md`（唯一依据）

## 硬性规则

- 新增或修改 fork 功能时，同一批提交内更新 `自定义开发功能列表.md`；能机械校验的同时加进 `scripts/we2ai/check-guards.sh`
- 版本号：主号 = 上游主号 + 1（4 处版本文件由脚本维护，不要手改成上游版本）
- `.github/workflows/*.yml` 的 `on:` 只允许 `workflow_dispatch`
- 本文件和 `.claude/` 被上游 `.gitignore` 忽略，新增时用 `git add -f`
- 提交前跑 `./scripts/we2ai/check-guards.sh`
