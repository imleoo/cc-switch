#!/usr/bin/env bash
# we2ai 上游同步：upstream/main → main → origin/main → we2ai → origin/we2ai
#
#   main   永远对齐上游（只允许 fast-forward，禁止混入 fork 改动）
#   we2ai  fork 定制分支，唯一发布分支；整体 git merge main，冲突在此解决
#
# 用法：
#   ./scripts/we2ai/sync-upstream.sh            # 全流程，含推送
#   NO_PUSH=1 ./scripts/we2ai/sync-upstream.sh  # 只做本地合并，不推送
#
# 冲突时脚本会停下并列出文件；手动解决并 `git commit` 完成合并后，重新运行本脚本
# 即可从断点继续（各步骤幂等）。合并前建议先跑 /sync-upstream 做风险分析。
set -euo pipefail

# 运行中会切到 main（该分支没有本脚本），先从临时副本重新执行，避免 bash 读到一半文件被换掉
if [[ -z "${WE2AI_SYNC_SELF_COPY:-}" ]]; then
  self_copy="$(mktemp)"
  cp "${BASH_SOURCE[0]}" "$self_copy"
  WE2AI_SYNC_SELF_COPY="$self_copy" exec bash "$self_copy" "$@"
fi

UPSTREAM_REMOTE="${UPSTREAM_REMOTE:-upstream}"
UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/farion1231/cc-switch.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-main}"
ORIGIN_REMOTE="${ORIGIN_REMOTE:-origin}"
MAIN_BRANCH="${MAIN_BRANCH:-main}"
WORK_BRANCH="${WORK_BRANCH:-we2ai}"
NO_PUSH="${NO_PUSH:-0}"

die() { echo "error: $*" >&2; exit 1; }

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "请在 git 仓库内运行"
cd "$repo_root"
[[ -f scripts/we2ai/lib.sh ]] || die "找不到 scripts/we2ai/lib.sh，请在 ${WORK_BRANCH} 分支上运行"
source scripts/we2ai/lib.sh

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir" "$WE2AI_SYNC_SELF_COPY"' EXIT

[[ -z "$(git status --porcelain)" ]] || { git status --short >&2; die "工作区不干净，请先提交或 stash"; }
[[ -z "$(git ls-files -u)" ]] || die "存在未解决的合并冲突"

push() {
  if [[ "$NO_PUSH" == "1" ]]; then
    echo "(NO_PUSH=1，跳过推送 $1)"
  else
    git push "$ORIGIN_REMOTE" "$1"
  fi
}

remote_branch_exists() { git show-ref --verify --quiet "refs/remotes/$1/$2"; }

# ── 0. 远端准备 ───────────────────────────────────────────────
if ! git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
  echo "添加 remote ${UPSTREAM_REMOTE} -> ${UPSTREAM_URL}"
  git remote add "$UPSTREAM_REMOTE" "$UPSTREAM_URL"
fi
git remote get-url "$ORIGIN_REMOTE" >/dev/null 2>&1 || die "缺少 remote: $ORIGIN_REMOTE"

echo "==> fetch ${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}、${ORIGIN_REMOTE}"
git fetch "$UPSTREAM_REMOTE" "$UPSTREAM_BRANCH" --tags
git fetch "$ORIGIN_REMOTE"

# ── 1. main 快进到上游 ────────────────────────────────────────
echo "==> [1/4] ${MAIN_BRANCH} 快进到 ${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
if ! git show-ref --verify --quiet "refs/heads/${MAIN_BRANCH}"; then
  if remote_branch_exists "$ORIGIN_REMOTE" "$MAIN_BRANCH"; then
    git branch "$MAIN_BRANCH" "${ORIGIN_REMOTE}/${MAIN_BRANCH}"
  else
    git branch "$MAIN_BRANCH" "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
  fi
fi
git switch "$MAIN_BRANCH"
if remote_branch_exists "$ORIGIN_REMOTE" "$MAIN_BRANCH"; then
  git merge --ff-only "${ORIGIN_REMOTE}/${MAIN_BRANCH}" >/dev/null 2>&1 || true
fi
if ! git merge --ff-only "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"; then
  echo "本地 ${MAIN_BRANCH} 独有提交：" >&2
  git log --oneline "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}..${MAIN_BRANCH}" >&2
  die "${MAIN_BRANCH} 无法快进：它含有上游没有的提交。main 禁止混入 fork 改动，请把这些提交移到 ${WORK_BRANCH} 后重置 main"
fi
push "$MAIN_BRANCH"
upstream_version="$(ref_version "$MAIN_BRANCH")"
target_version="$(fork_version "$upstream_version")"
echo "    上游版本 ${upstream_version} → we2ai 版本 ${target_version}"

# ── 2. we2ai 对齐远端 ─────────────────────────────────────────
echo "==> [2/4] ${WORK_BRANCH} 对齐 ${ORIGIN_REMOTE}/${WORK_BRANCH}"
git switch "$WORK_BRANCH"
if remote_branch_exists "$ORIGIN_REMOTE" "$WORK_BRANCH" \
   && [[ "$(git rev-list --count "HEAD..${ORIGIN_REMOTE}/${WORK_BRANCH}")" != "0" ]]; then
  git merge --no-edit -m "chore(we2ai): sync with ${ORIGIN_REMOTE}/${WORK_BRANCH}" "${ORIGIN_REMOTE}/${WORK_BRANCH}" \
    || die "与 ${ORIGIN_REMOTE}/${WORK_BRANCH} 合并冲突，解决并提交后重跑本脚本"
fi

# 版本文件冲突自动解决：把 base/ours/theirs 三方的版本号统一成占位符再做三方合并，
# 只要冲突仅来自版本号行就能干净合并，随后写入目标版本。
resolve_version_conflicts() {
  local f s name
  for f in "${WE2AI_VERSION_FILES[@]}"; do
    [[ -n "$(git ls-files -u -- "$f")" ]] || continue
    name="${f##*/}"   # 保留文件名后缀，rewrite_version 按后缀识别格式
    local ok=1
    for s in 1 2 3; do
      git show ":${s}:${f}" > "${tmp_dir}/${s}_${name}" 2>/dev/null || { ok=0; break; }
      rewrite_version "${tmp_dir}/${s}_${name}" "0.0.0-we2ai-placeholder"
    done
    [[ "$ok" == 1 ]] || { echo "    ${f}: 缺少合并 stage，需手动处理"; continue; }
    if git merge-file -p "${tmp_dir}/2_${name}" "${tmp_dir}/1_${name}" "${tmp_dir}/3_${name}" > "${tmp_dir}/out_${name}"; then
      cp "${tmp_dir}/out_${name}" "$f"
      rewrite_version "$f" "$target_version"
      git add "$f"
      echo "    自动解决版本文件冲突：${f} → ${target_version}"
    else
      echo "    ${f}: 除版本号外还有其他冲突，需手动处理"
    fi
  done
}

# ── 3. we2ai 整体合并 main ────────────────────────────────────
echo "==> [3/4] ${WORK_BRANCH} 合并 ${MAIN_BRANCH}（上游 v${upstream_version}）"
if [[ "$(git rev-list --count "HEAD..${MAIN_BRANCH}")" != "0" ]]; then
  merge_msg="chore(we2ai): sync from upstream v${upstream_version}"
  if ! git merge --no-edit -m "$merge_msg" "$MAIN_BRANCH"; then
    resolve_version_conflicts
    if [[ -n "$(git ls-files -u)" ]]; then
      echo
      echo "╔══════════════════════════════════════════════════════╗"
      echo "║  需要手动解决冲突                                    ║"
      echo "╚══════════════════════════════════════════════════════╝"
      echo "对照 自定义开发功能列表.md 风险表逐个核对以下文件："
      git diff --name-only --diff-filter=U
      echo
      echo "解决后：git add <file> && git commit --no-edit，然后重跑本脚本继续"
      echo "放弃：git merge --abort"
      exit 1
    fi
    git commit --no-edit
  fi
else
  echo "    ${WORK_BRANCH} 已包含 ${MAIN_BRANCH} 全部提交"
fi

# 无冲突合并时上游版本号可能直接覆盖进来，统一改写为 fork 版本
changed=0
for f in "${WE2AI_VERSION_FILES[@]}"; do
  if [[ "$(read_version "$f" < "$f")" != "$target_version" ]]; then
    rewrite_version "$f" "$target_version"
    git add "$f"
    changed=1
  fi
done
if [[ "$changed" == 1 ]]; then
  git commit -m "chore(we2ai): bump version to ${target_version}"
  echo "    版本号改写为 ${target_version}"
fi

# ── 4. 守卫检查 + 推送 ────────────────────────────────────────
echo "==> [4/4] 守卫检查"
if ! MAIN_REF="$MAIN_BRANCH" bash scripts/we2ai/check-guards.sh; then
  echo
  echo "守卫未通过，未推送。修复后提交，再重跑本脚本。"
  echo "常见原因：上游新增/修改了 workflow 触发器 → 把 on: 改回仅 workflow_dispatch"
  exit 1
fi
push "$WORK_BRANCH"

echo
echo "同步完成：${WORK_BRANCH} = 上游 v${upstream_version} + fork 定制，版本 ${target_version}"
echo "别忘了在 自定义开发功能列表.md「同步记录」追加一行"
