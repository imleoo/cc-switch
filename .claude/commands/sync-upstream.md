# /sync-upstream — we2ai 上游同步风险分析

合并前对比上游差异与 fork 功能列表，输出风险分析报告。**本命令只做分析，不执行任何改动仓库的 git 操作**（`git fetch` 除外）。

分支模型、准入规则、风险表都以 `自定义开发功能列表.md` 为唯一依据，本文件不重复。

## 执行步骤

### Step 1：完整读取 `自定义开发功能列表.md`（强制）

读完之前不做任何 diff 分析。

### Step 2：获取分支状态

```bash
git fetch upstream --tags --quiet && git fetch origin --quiet

# 版本快照（package.json 为准）
git show upstream/main:package.json | grep -m1 '"version"'
git show main:package.json          | grep -m1 '"version"'
git show we2ai:package.json         | grep -m1 '"version"'

git log --oneline main..upstream/main      # main 落后上游（应可 fast-forward）
git log --oneline upstream/main..main      # 必须为空，否则 main 被污染
git log --oneline we2ai..upstream/main     # we2ai 待合并内容（分析主体）

git diff we2ai...upstream/main --name-only # 上游自分叉点以来改动的文件
git diff we2ai...upstream/main --stat
git diff we2ai...upstream/main --name-only --diff-filter=A -- .github/workflows/  # 新增 workflow
```

### Step 3：交叉比对

1. 上游改动文件 ∩ 功能列表风险表登记文件 → 冲突矩阵
2. 逐条看待合并提交，是否落入「排除类别」
3. 可选：`git merge-tree --write-tree we2ai upstream/main` 预演冲突文件（不改工作区）

### Step 4：输出报告（各节都不能省略）

---

## 上游同步风险分析报告

**分析时间**：[当前时间]

| 分支 | 版本 | 落后上游提交数 | 状态 |
|---|---|---|---|
| `upstream/main` | … | — | 基准 |
| `main` | … | … | ✅ 可快进 / ⚠️ 被污染 |
| `we2ai` | … | … | … |

### 待合并提交摘要

每条一行，标注是否触及 fork 功能。

### 排除类别核查

| 提交 | 是否命中排除类别 | 依据（功能列表哪一条） | 结论 |
|---|---|---|---|
| … | 是/否 | … | ✅ 整体合入 / ⚠️ 合入后裁剪（写明范围） / 🆕 疑似新排除类别，需人工评估后登记 |

### 冲突矩阵

| 风险表登记文件 | 上游是否修改 | 涉及 fork 功能 | 预期处理 |
|---|---|---|---|
| … | 是/否 | … | 脚本自动 / 手动 |

### fork 功能逐项评估

逐个功能说明：上游改动是否触及；触及时合并后要恢复什么。

### 合并建议

**整体风险**：🔴 / 🟡 / 🟢

- [ ] `./scripts/we2ai/sync-upstream.sh`（或先 `NO_PUSH=1` 本地演练）
- [ ] 冲突时对照风险表逐个解决 → `git commit --no-edit` → 重跑脚本
- [ ] 需裁剪的内容：[列出文件与范围]
- [ ] 验证：`./scripts/we2ai/check-guards.sh`；`pnpm typecheck && pnpm test:unit`；`cd src-tauri && cargo check`
- [ ] `自定义开发功能列表.md`「同步记录」追加一行；新排除类别同步登记

---

报告完成后等待用户确认，不要自动执行合并。
