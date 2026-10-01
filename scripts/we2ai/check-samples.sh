#!/usr/bin/env bash
# 调用示例（功能 22）模板语法检查：把 6 种语言 × 3 协议 × 2 种 Key 表达写成文件，
# 逐个用本机已有的工具做语法/编译检查，工具不存在就跳过。**可选**脚本，不在
# check-guards.sh 与 CI 必跑范围内。用法：./scripts/we2ai/check-samples.sh
# 生成示例复用 vitest：与 `pnpm test:unit` 一样，Vite 加载配置时会在仓库根目录短暂写
# `vitest.config.ts.timestamp-*.mjs` 并自动删除；示例文件只写进 mktemp 目录。只读沙箱里
# 运行会因这一写入失败，属预期。
set -uo pipefail
cd "$(dirname "$0")/../.."

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

WE2AI_SAMPLES_DIR="$out" pnpm exec vitest run tests/we2ai/codeSamples.test.ts -t "dump samples" >/dev/null 2>&1 \
  || { echo "check-samples: 生成示例失败（先单独跑 pnpm exec vitest run tests/we2ai/codeSamples.test.ts）" >&2; exit 2; }

fail=0
checked=0
skipped=""

have() { command -v "$1" >/dev/null 2>&1; }

run_check() { # <name> <dir> <cmd...>
  local name="$1" dir="$2"
  shift 2
  if ! (cd "$dir" && "$@") >"$dir/.log" 2>&1; then
    echo "FAIL: $name"
    sed 's/^/    /' "$dir/.log"
    fail=1
  fi
  checked=$((checked + 1))
}

for dir in "$out"/*/; do
  dir="${dir%/}"
  name="$(basename "$dir")"
  case "$name" in
    curl_*) run_check "$name" "$dir" bash -n sample.sh ;;
    python_*)
      if have python3; then run_check "$name" "$dir" python3 -m py_compile sample.py; else skipped="$skipped python3"; fi ;;
    node_*)
      if have node; then run_check "$name" "$dir" node --check sample.mjs; else skipped="$skipped node"; fi ;;
    java_*)
      if have javac; then run_check "$name" "$dir" javac --release 11 -d . We2aiDemo.java; else skipped="$skipped javac"; fi ;;
    go_*)
      if have go; then run_check "$name" "$dir" go build -o /dev/null main.go; else skipped="$skipped go"; fi ;;
    powershell_*)
      if have pwsh; then
        run_check "$name" "$dir" pwsh -NoProfile -Command '$e = $null; [void][System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path sample.ps1), [ref]$null, [ref]$e); if ($e.Count) { $e | Out-String | Write-Error; exit 1 }'
      else skipped="$skipped pwsh"; fi ;;
  esac
done

skipped="$(printf '%s\n' $skipped | sort -u | tr '\n' ' ')"
[[ -n "$skipped" ]] && echo "check-samples: 跳过（本机没有）：$skipped"
if [[ "$fail" == 0 ]]; then
  echo "check-samples: ${checked} 份示例语法检查通过"
fi
exit "$fail"
