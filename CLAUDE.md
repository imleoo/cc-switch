# CLAUDE.md — we2ai-client

本仓库是 `imleoo/we2ai-client`（私有），源自 `farion1231/cc-switch` 的 `we2ai` 分支，**独立维护，不再同步上游**。

## 分支

- `main`：唯一主干与发布分支，开发从它切 feature 分支
- 不再有 `upstream` 同步流程：不要 merge / cherry-pick 上游，需要上游的某个改动时手动移植并在 `自定义开发功能列表.md` 登记
- `origin` = `imleoo/we2ai-client`（私有源码）；`legacy` = `imleoo/cc-switch`（公开，**只放 Release**，不要往里推源码分支）

## 硬性规则

- 新增或修改 we2ai 功能时，同一批提交内更新 `自定义开发功能列表.md`；能机械校验的同时加进 `scripts/we2ai/check-guards.sh`
- 版本号：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 4 处必须一致，自行递增
- `.github/workflows/*.yml` 的 `on:` 只允许 `workflow_dispatch`
- Release 发布到公开仓库 `imleoo/cc-switch`（`release.yml` 的 `RELEASE_REPO`，需要 secret `RELEASE_REPO_TOKEN`），与 updater 地址同仓库，不得改成发回本仓库
- 本文件和 `.claude/` 被 `.gitignore` 忽略，新增时用 `git add -f`
- 提交前跑 `./scripts/we2ai/check-guards.sh`
