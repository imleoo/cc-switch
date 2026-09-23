#!/usr/bin/env bash
# we2ai fork 守卫检查：防止上游同步静默还原 fork 定制。
# 规则来源：自定义开发功能列表.md 风险表。
#   1. 功能 1：4 个版本文件版本号一致，且主号 = main 分支主号 + 1
#   2. 功能 2：.github/workflows/*.yml 的 on: 只允许 workflow_dispatch
#   3. 功能 4：自动更新地址/签名公钥指向 we2ai，代码里无上游发布页链接
# 退出码：0 全部通过；非 0 至少一条违规（详见 stderr）
set -uo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"
source scripts/we2ai/lib.sh

MAIN_REF="${MAIN_REF:-main}"
fail=0
err() { echo "FAIL: $*" >&2; fail=1; }

# ── 1. 版本号 ────────────────────────────────────────────────
expected="$(fork_version "$(ref_version "$MAIN_REF")")"
for f in "${WE2AI_VERSION_FILES[@]}"; do
  v="$(read_version "$f" < "$f")"
  if [[ -z "$v" ]]; then
    err "$f: 读不到版本号"
  elif [[ "$v" != "$expected" ]]; then
    err "$f: 版本 $v，期望 $expected（${MAIN_REF} 主号 +${WE2AI_MAJOR_OFFSET}）"
  fi
done

# ── 2. workflow 仅手动触发 ────────────────────────────────────
for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
  [[ -f "$wf" ]] || continue
  if grep -qE '^on:[[:space:]]*[^[:space:]#]' "$wf"; then
    err "$wf: on: 使用了行内写法，需改为仅 workflow_dispatch"
    continue
  fi
  triggers="$(awk '/^on:/{p=1;next} p&&/^[^[:space:]#]/{exit} p&&/^  [A-Za-z_]+:/{sub(/^  /,"");sub(/:.*/,"");print}' "$wf")"
  if [[ -z "$triggers" ]]; then
    err "$wf: 找不到 on: 触发器块"
  elif [[ "$triggers" != "workflow_dispatch" ]]; then
    err "$wf: 存在非手动触发器：$(echo "$triggers" | grep -v '^workflow_dispatch$' | tr '\n' ' ')"
  fi
done

# ── 3. 自动更新指向 we2ai ─────────────────────────────────────
WE2AI_UPDATER_ENDPOINT="https://github.com/imleoo/cc-switch/releases/latest/download/latest.json"
WE2AI_UPDATER_KEY_ID="3B2842A26CC12882"   # minisign 公钥 ID，私钥 ~/.tauri/we2ai.key
conf=src-tauri/tauri.conf.json
endpoints="$(node -e 'const c=require(process.argv[1]);console.log((c.plugins?.updater?.endpoints||[]).join("\n"))' "$PWD/$conf")"
if [[ "$endpoints" != "$WE2AI_UPDATER_ENDPOINT" ]]; then
  err "$conf: updater.endpoints 应只有 ${WE2AI_UPDATER_ENDPOINT}，实际：$(echo "$endpoints" | tr '\n' ' ')"
fi
pubkey="$(node -e 'const c=require(process.argv[1]);console.log(c.plugins?.updater?.pubkey||"")' "$PWD/$conf")"
if ! echo "$pubkey" | base64 -d 2>/dev/null | grep -q "$WE2AI_UPDATER_KEY_ID"; then
  err "$conf: updater.pubkey 不是 we2ai 公钥（期望 key ID ${WE2AI_UPDATER_KEY_ID}）"
fi
upstream_links="$(grep -rnE 'farion1231/cc-switch/releases|dl\.ccswitch\.io' src src-tauri/src 2>/dev/null || true)"
if [[ -n "$upstream_links" ]]; then
  err "代码中残留上游发布页/更新地址：
${upstream_links}"
fi

if [[ "$fail" == 0 ]]; then
  echo "we2ai guards: all passed (version=${expected})"
fi
exit "$fail"
