#!/usr/bin/env bash
# we2ai fork 守卫检查：防止上游同步静默还原 fork 定制。
# 规则来源：自定义开发功能列表.md 风险表。
#   1. 功能 1：4 个版本文件版本号一致，且主号 = main 分支主号 + 1
#   2. 功能 2：.github/workflows/*.yml 的 on: 只允许 workflow_dispatch
#   3. 功能 4：自动更新地址/签名公钥指向 we2ai，代码里无上游发布页链接
# 退出码：0 全部通过；非 0 至少一条违规（详见 stderr）
set -euo pipefail

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
    err "$f: 版本 ${v}，期望 ${expected}（${MAIN_REF} 主号 +${WE2AI_MAJOR_OFFSET}）"
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

# ── 4. WE2AI 壳/品牌/数据隔离守卫（P0，方案第 6、7 节） ──────────────
# 4.1 数据根写死路径只允许出现在带 `// we2ai-allow-cc-switch` 标记的行
# （其余出现在 #[cfg(test)] 模块内也放行）。改用标记而不是 "file:line" 精确
# 登记——行号登记太脆：上游改了前后行会让行号漂移导致误报（明明是登记过的同
# 一行只是挪了位置），删掉 we2ai 分支但那一行本身还留着又会漏报（行号凑巧还
# 在登记表里）。标记跟着代码走，挪到哪行都还在它上一行，删除整段分支时标记
# 也会一起被删，不会有登记表和代码各说各话的问题。
# 标记本身不足以放行——还要求标记所在的函数是登记过的那一个，否则任何人在
# 任意位置的 `.cc-switch` 字面量上一行贴个标记就能骗过守卫。
we2ai_dir_allowed_locations=(
  "src-tauri/src/config.rs:get_app_config_dir"
  "src-tauri/src/settings.rs:settings_path"
  "src-tauri/src/panic_hook.rs:default_app_config_dir"
  "src-tauri/src/services/env_manager.rs:get_backup_dir"
)
we2ai_dir_enclosing_fn() {
  local file="$1" target="$2"
  awk -v target="$target" '
    NR>=target { exit }
    /fn[ \t]+[A-Za-z_][A-Za-z0-9_]*/ {
      line = $0
      sub(/.*fn[ \t]+/, "", line)
      sub(/[^A-Za-z0-9_].*/, "", line)
      if (line != "") last = line
    }
    END { print last }
  ' "$file"
}
we2ai_dir_marker_allowed() {
  local file="$1" target="$2"
  local prev_line
  prev_line="$(sed -n "$((target - 1))p" "$file" 2>/dev/null || true)"
  [[ "$prev_line" == *"we2ai-allow-cc-switch"* ]] || return 1

  local enclosing_fn key
  enclosing_fn="$(we2ai_dir_enclosing_fn "$file" "$target")"
  key="${file}:${enclosing_fn}"
  for allowed in "${we2ai_dir_allowed_locations[@]}"; do
    [[ "$key" == "$allowed" ]] && return 0
  done
  return 1
}
we2ai_dir_in_test_module() {
  local file="$1" target="$2"
  awk -v target="$target" '
    /#\[cfg\(test\)\]/ { pending=1; next }
    pending {
      if ($0 ~ /mod[ \t]+[A-Za-z_][A-Za-z0-9_]*[ \t]*\{/) {
        depth=1; start=NR; pending=0; in_test=1; next
      }
      # #[cfg(test)] 后面不是 mod 块（例如单独标注的 fn/use），清掉 pending，
      # 避免误把后面很远的一个 mod 块当成测试模块。
      pending=0
    }
    in_test {
      n=gsub(/\{/,"{"); depth+=n
      n=gsub(/\}/,"}"); depth-=n
      if (depth<=0) {
        if (target>=start && target<=NR) print "yes"
        in_test=0
      }
    }
  ' "$file" | grep -q yes
}
# 匹配精确的 ".cc-switch" 字符串字面量（不要求紧跟 .join(...)），这样也能
# 拦住 PathBuf::from(".cc-switch")、format!("{}", ".cc-switch") 等写法；同时
# 不会误伤 codex_config.rs 里 "[model_providers.cc-switch]" 这类无关字面量
# （那是一个更长字符串的子串，不是独立的 ".cc-switch" 字面量）。
while IFS=: read -r dir_file dir_line _rest; do
  [[ -z "$dir_file" ]] && continue
  if we2ai_dir_marker_allowed "$dir_file" "$dir_line"; then
    continue
  fi
  if we2ai_dir_in_test_module "$dir_file" "$dir_line"; then
    continue
  fi
  err "$dir_file:$dir_line: 写死 .cc-switch 路径既没有落在登记的 file+函数名 组合（上一行 // we2ai-allow-cc-switch 标记必须出现在 config.rs::get_app_config_dir / settings.rs::settings_path / panic_hook.rs::default_app_config_dir / env_manager.rs::get_backup_dir 之一），也不在测试模块内（自定义开发功能列表.md 第 6.1 节）"
done < <(grep -rn '"\.cc-switch"' src-tauri/src --include="*.rs" 2>/dev/null || true)

# 4.2 deep-link scheme 只允许 we2ai
conf=src-tauri/tauri.conf.json
schemes="$(node -e 'const c=require(process.argv[1]);console.log((c.plugins?.["deep-link"]?.desktop?.schemes||[]).join(","))' "$PWD/$conf")"
if [[ "$schemes" != "we2ai" ]]; then
  err "$conf: plugins.deep-link.desktop.schemes 应只有 we2ai，实际：$schemes"
fi
product_name="$(node -e 'const c=require(process.argv[1]);console.log(c.productName||"")' "$PWD/$conf")"
if [[ "$product_name" != "WE2AI" ]]; then
  err "$conf: productName 应为 WE2AI，实际：$product_name"
fi
identifier="$(node -e 'const c=require(process.argv[1]);console.log(c.identifier||"")' "$PWD/$conf")"
if [[ "$identifier" != "com.we2ai.desktop" ]]; then
  err "$conf: identifier 应为 com.we2ai.desktop，实际：$identifier"
fi
main_binary_name="$(node -e 'const c=require(process.argv[1]);console.log(c.mainBinaryName||"")' "$PWD/$conf")"
if [[ "$main_binary_name" != "we2ai" ]]; then
  err "$conf: mainBinaryName 应为 we2ai，实际：${main_binary_name}（Linux 上 tauri-plugin-deep-link 按可执行文件名生成 <名字>-handler.desktop，二进制名撞名会跟真实 CC Switch 抢同一个文件，见自定义开发功能列表.md 功能 5 的 Linux 深链条目）"
fi

# 4.3 用户可见品牌残留扫描。
# 排除区：i18n 里的 partnerPromotion（合作伙伴推广文案，指代真实签约主体
# "CC Switch"、优惠码如 "cc-switch"/"CCSWITCH" 均是第三方事实，不属于我们
# 自称的品牌文案，不应替换，见自定义开发功能列表.md 功能 5）；lib.rs 里
# 提及"CC Switch"是在描述与另一个真实存在的应用（CC Switch 本体）互操作
# 的注释，一律以整行 trim 后是否以 `//` 开头判定是否为注释。
i18n_brand_leftover=""
for locale in src/i18n/locales/*.json; do
  [[ -f "$locale" ]] || continue
  hit="$(node -e '
    const fs = require("fs");
    const path = process.argv[1];
    const raw = fs.readFileSync(path, "utf8");
    const lines = raw.split("\n");
    const startIdx = lines.findIndex((l) => l.includes("\"partnerPromotion\""));
    let endIdx = lines.length;
    if (startIdx >= 0) {
      let depth = 0;
      let started = false;
      for (let i = startIdx; i < lines.length; i++) {
        const opens = (lines[i].match(/\{/g) || []).length;
        const closes = (lines[i].match(/\}/g) || []).length;
        if (opens > 0) started = true;
        depth += opens - closes;
        if (started && depth <= 0) { endIdx = i; break; }
      }
    }
    const hits = [];
    lines.forEach((line, i) => {
      if (i > startIdx && i <= endIdx && startIdx >= 0) return; // 跳过 partnerPromotion 区间
      if (line.includes("CC Switch")) hits.push(i + 1);
    });
    if (hits.length) console.log(hits.join(","));
  ' "$locale")"
  if [[ -n "$hit" ]]; then
    i18n_brand_leftover+="$locale:$hit\n"
  fi
done
if [[ -n "$i18n_brand_leftover" ]]; then
  err "以下 i18n 文件残留品牌文字 'CC Switch'（partnerPromotion 区块除外，行号）：
$(printf "%b" "$i18n_brand_leftover")"
fi

# 4.3b tauri.*.conf.json / Info.plist 无残留 "CC Switch" / "ccswitch"
for conf_file in src-tauri/tauri.conf.json src-tauri/tauri.*.conf.json src-tauri/Info.plist; do
  [[ -f "$conf_file" ]] || continue
  residue="$(grep -niE 'CC Switch|ccswitch' "$conf_file" 2>/dev/null || true)"
  if [[ -n "$residue" ]]; then
    err "$conf_file: 残留品牌字符串：
$residue"
  fi
done

# 4.3c lib.rs 里用户可见字符串（对话框/日志等，非注释行）无残留 "CC Switch"
lib_brand_leftover="$(grep -n "CC Switch" src-tauri/src/lib.rs 2>/dev/null | awk -F: '
  { line=$0; sub(/^[0-9]+:/, "", line); trimmed=line; sub(/^[ \t]+/, "", trimmed);
    if (trimmed !~ /^\/\//) print }
' || true)"
if [[ -n "$lib_brand_leftover" ]]; then
  err "src-tauri/src/lib.rs 非注释行残留品牌文字 'CC Switch'（应为 WE2AI 或指代协作的另一真实应用）：
$lib_brand_leftover"
fi

# 4.3c2 Linux deep-link handler 文件名不得写死。tauri-plugin-deep-link 按
# `<可执行文件名>-handler.desktop` 在共享的 ~/.local/share/applications 里
# 生成处理器文件；写死 "cc-switch-handler" 会在同机安装 CC Switch 时抢占/被
# 抢占同一个文件（见自定义开发功能列表.md 功能 5 的 Linux 深链条目）。
if grep -qE 'cc-switch-handler' src-tauri/src/lib.rs; then
  err "src-tauri/src/lib.rs: 出现写死的 'cc-switch-handler'，deep-link handler 文件名必须按运行时实际二进制名（tauri::utils::platform::current_exe()）动态推导"
fi

# 4.3d IPC 分发唯一入口：.invoke_handler( 在 lib.rs 里只能出现一次，且必须是
# gate() 包裹的那一次——否则有可能存在第二条绕过白名单的分发路径。
invoke_handler_count="$(grep -c '\.invoke_handler(' src-tauri/src/lib.rs || true)"
if [[ "$invoke_handler_count" != "1" ]]; then
  err "src-tauri/src/lib.rs: .invoke_handler( 出现 ${invoke_handler_count} 次，期望恰好 1 次（多一处可能绕开 IPC 白名单）"
elif ! grep -q 'invoke_handler(we2ai::mode::gate(' src-tauri/src/lib.rs; then
  err "src-tauri/src/lib.rs: 唯一的 .invoke_handler( 调用未接 we2ai::mode::gate(...)，IPC 白名单被绕过"
fi

# 4.3e 启动/退出白名单：lib.rs 里每个 StartupTask 变体必须恰好出现一次
# （对应恰好一个门控调用点），而不是只核对调用点总数——总数相同不代表每个
# 变体都被用到，可能出现"漏了一个变体、但另一个变体被误用两次"从而总数凑巧
# 相等的情况。变体清单直接从 mode.rs 的 `pub enum StartupTask { ... }` 解析，
# 不在这里手写第二份清单，避免两边各说各话。
#
# 光核对"变体名出现过"还不够：变体名可能只出现在门控 if 的条件里、而真正被
# 门控的调用早就被挪到了 if 块外面（比如重构时手滑）。所以每个变体额外登记一个
# "应该被这个门控保护的调用名"，再核对该调用名真的出现在
# `startup_allowed(...StartupTask::X)` 所在的那个 if 块内部（用花括号深度定位
# 该 if 块的起止行，再看调用名是否落在这个行区间里）。调用名只挑一个具有代表性
# 的、块内独有的标识符，不追求覆盖块内每一行代码。
we2ai_startup_task_variants() {
  awk '
    /pub enum StartupTask/ { in_enum=1; next }
    in_enum && /^\}/ { exit }
    in_enum {
      line = $0
      gsub(/^[ \t]+/, "", line)
      gsub(/[ \t]+$/, "", line)
      if (line ~ /^[A-Za-z_][A-Za-z0-9_]*,?$/) {
        sub(/,$/, "", line)
        print line
      }
    }
  ' src-tauri/src/we2ai/mode.rs
}
# variant:被门控的代表性调用名（子串匹配即可，不需要完整签名）
we2ai_startup_gate_calls=(
  "FirstRunImportToolProviders:import_default_config"
  "SeedOfficialProviders:init_default_official_providers"
  "AdditiveProviderImport:import_opencode_providers_from_live"
  "DefaultSkillsInit:init_default_skill_repos"
  "ImportMcpOnEmptyTable:import_from_claude"
  "ImportPromptsOnEmptyTable:import_from_file_on_first_launch"
  "ProxyStateRestoreOnStartup:restore_proxy_state_on_startup"
  "CrashRecovery:recover_from_crash"
  "ExitLiveRestore:stop_with_restore_keep_state"
  "WebdavS3Sync:webdav_auto_sync::start_worker"
  "SessionUsageSync:run_session_sync"
  "CommonConfigSnippets:initialize_common_config_snippets"
  "SkillsSsotMigration:migrate_skills_to_ssot"
  "CodexHistoryMigration:maybe_migrate_codex_third_party_history_provider_bucket"
  "PeriodicBackup:periodic_backup_if_needed"
)
we2ai_gate_call_for_variant() {
  local variant="$1" entry
  for entry in "${we2ai_startup_gate_calls[@]}"; do
    if [[ "${entry%%:*}" == "$variant" ]]; then
      printf '%s' "${entry#*:}"
      return 0
    fi
  done
  return 1
}
# 找到 lib.rs 里从 target_line 开始、按花括号深度匹配到的 if 块 [起始行,结束行]。
# target_line 是包含 `StartupTask::X` 的那一行，块的 `{` 可能就在同一行，也可能
# 在后续几行（多行 if 条件）；用花括号深度从该行往下扫，深度第一次 >0 记起始行，
# 深度回到 0 记结束行。
we2ai_block_range_from() {
  local target="$1"
  local file="${2:-src-tauri/src/lib.rs}"
  awk -v target="$target" '
    # 计数前先把字符串字面量整体挖空，避免日志/文案里的花括号（如
    # `log::info!("{}"）` 或提示文案 `"...{name}..."`）被误当成代码结构，
    # 从而算错块的起止行。只处理双引号字符串，`\\.` 吃掉转义字符（含 `\"`）。
    function strip_strings(line,    out) {
      out = line
      gsub(/"([^"\\]|\\.)*"/, "", out)
      return out
    }
    NR < target { next }
    {
      line = strip_strings($0)
      n = gsub(/\{/, "{", line); depth += n
      n = gsub(/\}/, "}", line); depth -= n
      if (!started && depth > 0) { started = 1; start = NR }
      if (started && depth <= 0) { print start","NR; exit }
    }
  ' "$file"
}
startup_task_variants="$(we2ai_startup_task_variants)"
if [[ -z "$startup_task_variants" ]]; then
  err "解析 src-tauri/src/we2ai/mode.rs 的 StartupTask 枚举失败，未取到任何变体（守卫脚本的 awk 解析可能需要跟着 enum 写法更新）"
else
  while IFS= read -r variant; do
    [[ -z "$variant" ]] && continue
    variant_lines="$(grep -n "StartupTask::${variant}" src-tauri/src/lib.rs || true)"
    variant_count=0
    [[ -n "$variant_lines" ]] && variant_count="$(printf '%s\n' "$variant_lines" | wc -l | tr -d ' ')"
    if [[ "$variant_count" != "1" ]]; then
      err "src-tauri/src/lib.rs: StartupTask::${variant} 出现 ${variant_count} 次，期望恰好 1 次（每个变体应该只对应一个门控调用点）"
      continue
    fi

    gate_call="$(we2ai_gate_call_for_variant "$variant" || true)"
    if [[ -z "$gate_call" ]]; then
      err "scripts/we2ai/check-guards.sh: StartupTask::${variant} 没有在 we2ai_startup_gate_calls 里登记对应的被门控调用名，无法校验调用是否真的在 if 门控内（新增 StartupTask 变体时需要同步登记）"
      continue
    fi

    target_line="${variant_lines%%:*}"
    block_range="$(we2ai_block_range_from "$target_line")"
    if [[ -z "$block_range" ]]; then
      err "src-tauri/src/lib.rs:${target_line}: 未能定位 StartupTask::${variant} 所在 if 的花括号块范围（花括号可能不匹配，或该行不在 if 语句里）"
      continue
    fi
    block_start="${block_range%%,*}"
    block_end="${block_range##*,}"

    if ! sed -n "${block_start},${block_end}p" src-tauri/src/lib.rs | grep -qF -- "$gate_call"; then
      err "src-tauri/src/lib.rs:${block_start}-${block_end}: StartupTask::${variant} 对应的调用 '${gate_call}' 没有出现在这个 if 门控块内——门控条件和被门控的调用必须写在同一个 if 块里，调用被挪到 if 外时这条检查要能抓到"
    fi
  done < <(printf '%s\n' "$startup_task_variants")
fi

# 4.3f gate() 必须先校验调用来源窗口标签，再分发命令（自定义开发功能列表.md
# 第 9 节 / 方案第 6.2 节安全边界：Tauri 2.10.3 向所有 webview 无条件注入 IPC
# 桥接脚本，capabilities 的 windows=["main"] 只挡插件命令，挡不住自定义命令，
# 验证码窗口等非 main 窗口若不在 gate() 里被拦，可以直接调用 we2ai_* 命令）。
# 只做静态结构检查：函数体内存在窗口来源校验的调用，且该调用出现在真正把
# 请求交给 we2ai_handler/upstream_handler 的分发行之前。真正验证"非 main
# 窗口确实被拒绝"的是 cargo test 里的
# gate_rejects_invokes_from_non_main_windows_even_for_whitelisted_commands /
# gate_rejects_invokes_from_main_window_navigated_to_a_remote_url。
gate_start_line="$(grep -n '^pub fn gate<' src-tauri/src/we2ai/mode.rs | head -1 | cut -d: -f1)"
if [[ -z "$gate_start_line" ]]; then
  err "src-tauri/src/we2ai/mode.rs: 找不到 pub fn gate< 定义"
else
  gate_block_range="$(we2ai_block_range_from "$gate_start_line" src-tauri/src/we2ai/mode.rs)"
  if [[ -z "$gate_block_range" ]]; then
    err "src-tauri/src/we2ai/mode.rs:${gate_start_line}: 未能定位 gate() 函数体的花括号范围"
  else
    gate_start="${gate_block_range%%,*}"
    gate_end="${gate_block_range##*,}"
    gate_body="$(sed -n "${gate_start},${gate_end}p" src-tauri/src/we2ai/mode.rs)"
    trust_check_line="$(printf '%s\n' "$gate_body" | grep -n 'is_trusted_invoke_source(' | head -1 | cut -d: -f1)"
    dispatch_line="$(printf '%s\n' "$gate_body" | grep -n 'we2ai_handler(invoke)' | head -1 | cut -d: -f1)"
    if [[ -z "$trust_check_line" ]]; then
      err "src-tauri/src/we2ai/mode.rs:${gate_start}-${gate_end}: gate() 函数体内没有调用窗口来源校验（is_trusted_invoke_source），验证码等非 main 窗口可能绕过 IPC 白名单直接调用 we2ai_* 命令"
    elif [[ -n "$dispatch_line" && "$trust_check_line" -gt "$dispatch_line" ]]; then
      err "src-tauri/src/we2ai/mode.rs:${gate_start}-${gate_end}: gate() 里 is_trusted_invoke_source() 校验出现在分发给 we2ai_handler 之后，必须先校验来源再分发"
    fi
  fi
fi

# 4.4 capabilities 权限集与登记清单一致（未开放 store、fs）
caps_file=src-tauri/capabilities/default.json
expected_perms="core:default
opener:default
updater:default
log:default
core:window:allow-set-skip-taskbar
core:window:allow-start-dragging
core:window:allow-minimize
core:window:allow-toggle-maximize
core:window:allow-is-maximized
core:window:allow-close
core:window:allow-set-decorations
process:allow-exit
process:allow-restart
dialog:default
notification:allow-is-permission-granted"
actual_perms="$(node -e 'const c=require(process.argv[1]);console.log((c.permissions||[]).join("\n"))' "$PWD/$caps_file")"
if [[ "$actual_perms" != "$expected_perms" ]]; then
  err "$caps_file: permissions 与登记清单不一致（自定义开发功能列表.md 第 7 节）。实际：
$actual_perms"
fi

# 4.5 功能 10：明文 Key 不经过 IPC。返回前端的 KeyView（Rust）与 We2aiKeyView
# （前端）采用字段允许清单：新增任何字段（含改名、serde 重命名）都要先在这里
# 登记并确认不是秘密（Codex P3 验收第 1 轮低危项）。
keys_rs=src-tauri/src/we2ai/keys.rs
expected_rs_fields="group_name id masked_key name status"
expected_ts_fields="groupName id maskedKey name status"
if [[ ! -f "$keys_rs" ]]; then
  err "$keys_rs 不存在（功能 10）"
else
  keyview_body="$(awk '/^pub struct KeyView \{/{f=1;next} f&&/^\}/{exit} f' "$keys_rs")"
  if [[ -z "$keyview_body" ]]; then
    err "$keys_rs: 找不到 pub struct KeyView，无法校验明文 Key 不出 IPC"
  else
    rs_fields="$(printf '%s\n' "$keyview_body" | sed -nE 's/^[[:space:]]*pub[[:space:]]+([a-z_0-9]+)[[:space:]]*:.*/\1/p' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
    if [[ "$rs_fields" != "$expected_rs_fields" ]]; then
      err "$keys_rs: KeyView 字段与允许清单不一致（实际：${rs_fields}；允许：${expected_rs_fields}）"
    fi
    if printf '%s\n' "$keyview_body" | grep -q 'serde('; then
      err "$keys_rs: KeyView 字段上不允许 serde 属性（重命名会绕过字段清单）"
    fi
  fi
fi
ts_keyview="$(awk '/^export interface We2aiKeyView \{/{f=1;next} f&&/^\}/{exit} f' src/we2ai/api.ts)"
if [[ -z "$ts_keyview" ]]; then
  err "src/we2ai/api.ts: 找不到 We2aiKeyView 接口"
else
  ts_fields="$(printf '%s\n' "$ts_keyview" | sed -nE 's/^[[:space:]]*([A-Za-z_0-9]+)[?]?[[:space:]]*:.*/\1/p' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
  if [[ "$ts_fields" != "$expected_ts_fields" ]]; then
    err "src/we2ai/api.ts: We2aiKeyView 字段与允许清单不一致（实际：${ts_fields}；允许：${expected_ts_fields}）"
  fi
fi

# 4.6 功能 11：上游 ProviderService::switch 的热切换拒绝（本 fork 对该文件的
# 唯一改动）。上游同步若把这段冲掉，WE2AI 在接管状态下会走热切换，把自己的
# 代理地址写进 CC Switch 正在接管的 live 文件。
provider_rs=src-tauri/src/services/provider/mod.rs
takeover_hits="$(grep -c 'crate::we2ai::apply::is_managed_provider_id(id)' "$provider_rs" || true)"
if [[ "$takeover_hits" != "1" ]]; then
  err "$provider_rs: 热切换拒绝判定（crate::we2ai::apply::is_managed_provider_id）应恰好出现 1 次，实际 ${takeover_hits} 次（功能 11）"
else
  guard_line="$(grep -n 'crate::we2ai::apply::is_managed_provider_id(id)' "$provider_rs" | cut -d: -f1)"
  hot_line="$(grep -n 'hot_switch_provider_inner(app_type.as_str(), id)' "$provider_rs" | head -1 | cut -d: -f1)"
  if [[ -z "$hot_line" || "$guard_line" -gt "$hot_line" ]]; then
    err "$provider_rs: 热切换拒绝判定必须位于 hot_switch_provider_inner 调用之前（功能 11）"
  fi
fi

# 4.7 功能 14：cargo/tauri dev 产出的二进制名必须是 we2ai，而不是随 [package]
# name 走的 cc-switch。`cargo build`/`pnpm tauri dev` 不经过 `tauri build` 的
# mainBinaryName 重命名步骤，没有这个显式 [[bin]] table 的话，macOS 钥匙串
# 授权提示、登录项等系统级 UI 在 dev 场景下仍会显示 cc-switch（自定义开发功能
# 列表.md 功能 14）。同时确认 [package] name / [lib] name 没有被上游同步改掉
# ——scripts/we2ai/lib.sh 的版本号定位、`cc_switch_lib` crate 名依赖它们不变。
cargo_toml=src-tauri/Cargo.toml
bin_block="$(awk '/^\[\[bin\]\]/{f=1;next} f&&/^\[/{exit} f' "$cargo_toml")"
if [[ -z "$bin_block" ]]; then
  err "$cargo_toml: 找不到 [[bin]] table，cargo build/pnpm tauri dev 会产出 cc-switch(.exe) 而不是 we2ai（功能 14）"
else
  bin_name="$(printf '%s\n' "$bin_block" | sed -nE 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' | head -1)"
  if [[ "$bin_name" != "we2ai" ]]; then
    err "$cargo_toml: [[bin]] name 应为 we2ai，实际：${bin_name:-<空>}（功能 14）"
  fi
fi
pkg_name="$(awk '/^\[package\]/{f=1;next} f&&/^\[/{exit} f&&/^[[:space:]]*name[[:space:]]*=/{print;exit}' "$cargo_toml" | sed -nE 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p')"
if [[ "$pkg_name" != "cc-switch" ]]; then
  err "$cargo_toml: [package] name 被改成了 ${pkg_name:-<空>}（应保持 cc-switch，见功能 14——scripts/we2ai/lib.sh 的版本号定位依赖这个包名）"
fi

# Codex 验收 Z3：`we2ai::detect::cc_switch_running_status`（Y1/Z2）用
# `tokio::process::Command` 异步执行检测子进程并带超时/`kill_on_drop`，
# 依赖 tokio 的 `process` feature；上游同步改这一行（新增/调整 tokio
# features 列表）时如果误删这个 feature，只有编译才会报错，且这个依赖
# 只在 we2ai 分支需要——同步演练里如果没跑一次 cargo check/test，可能被
# 忽略过去。这里加一道机械检查提前拦下。
tokio_line="$(grep -nE '^tokio = ' "$cargo_toml" | head -1)"
if [[ -z "$tokio_line" ]]; then
  err "$cargo_toml: 找不到 tokio 依赖声明行，无法核对 process feature（功能 9/Z3）"
elif [[ "$tokio_line" != *'"process"'* ]]; then
  err "$cargo_toml: tokio 的 features 列表缺少 \"process\"（we2ai::detect::cc_switch_running_status 依赖它异步执行检测子进程，功能 9/Z3）：${tokio_line}"
fi
lib_name="$(awk '/^\[lib\]/{f=1;next} f&&/^\[/{exit} f&&/^[[:space:]]*name[[:space:]]*=/{print;exit}' "$cargo_toml" | sed -nE 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p')"
if [[ "$lib_name" != "cc_switch_lib" ]]; then
  err "$cargo_toml: [lib] name 被改成了 ${lib_name:-<空>}（应保持 cc_switch_lib，见功能 14——测试/CI 里大量 cc_switch_lib:: 引用依赖这个 crate 名）"
fi

# 4.8 功能 15：release.yml 在没有 Apple 证书 Secret 时也能跑通（内测包），且
# 只能在 v* tag 上手动运行——不做运行时验证（那要真的跑一次 CI），只做静态
# 结构检查：guard 步骤存在、HAS_APPLE_CERT 在 job 级 env 里定义、publish-release
# 声明了 environment: release（否则读不到 secrets.APPLE_CERTIFICATE，正文文案
# 会一律显示"未公证"）。
release_yml=.github/workflows/release.yml
if [[ -f "$release_yml" ]]; then
  if ! grep -q "github.ref_type" "$release_yml"; then
    err "$release_yml: 找不到 ref_type 校验，release job 可能在分支 ref 上运行并创建以分支名为 tag 的错误 Release（功能 15）"
  fi
  if ! grep -qE '^\s*HAS_APPLE_CERT:\s*\$\{\{\s*secrets\.APPLE_CERTIFICATE\s*!=\s*.{0,2}\s*\}\}' "$release_yml"; then
    err "$release_yml: 找不到 job 级 env HAS_APPLE_CERT（应为 \${{ secrets.APPLE_CERTIFICATE != '' }}），macOS 签名/公证步骤的 if 条件依赖它（功能 15）"
  fi
  publish_release_block="$(awk '/^  publish-release:/{f=1} f{print} f&&/^  [a-zA-Z]/&&!/^  publish-release:/&&NR>1{if(seen)exit} /^  publish-release:/{seen=1}' "$release_yml")"
  if ! printf '%s\n' "$publish_release_block" | grep -qE '^\s*environment:\s*release\s*$'; then
    err "$release_yml: publish-release job 没有声明 environment: release，读不到 secrets.APPLE_CERTIFICATE，Release 正文的签名/未公证文案会永远显示成未公证（功能 15）"
  fi
else
  err "$release_yml: 文件不存在"
fi

# 4.9 功能 17（P6 恢复官方配置）：旧命令 we2ai_remove_tool_keys /
# RemoveToolKeysOutcome 已被 we2ai_restore_official / RestoreOfficialOutcome
# 取代，全仓不得再出现（上游同步不会引入这两个符号，只防我们自己手滑改回）；
# 新命令必须注册在 lib.rs 的 WE2AI 命令列表（generate_handler! 白名单）里——
# 它是 `we2ai_` 前缀命令，已经受 `gate()` 的默认放行规则覆盖，不需要再登记进
# 上游命令白名单（mode.rs::UPSTREAM_COMMAND_WHITELIST / ipcWhitelist.ts），
# 这里只检查它确实出现在 lib.rs 的第一个 generate_handler! 列表里。
removed_symbol_hits="$(grep -rn 'we2ai_remove_tool_keys\|RemoveToolKeysOutcome' src src-tauri/src --include="*.rs" --include="*.ts" --include="*.tsx" 2>/dev/null || true)"
if [[ -n "$removed_symbol_hits" ]]; then
  err "功能 17：we2ai_remove_tool_keys / RemoveToolKeysOutcome 已被 P6 的 we2ai_restore_official / RestoreOfficialOutcome 取代，不应再出现：
${removed_symbol_hits}"
fi
if ! grep -q 'we2ai::commands_apply::we2ai_restore_official' src-tauri/src/lib.rs; then
  err "src-tauri/src/lib.rs: 找不到 we2ai::commands_apply::we2ai_restore_official 注册（功能 17：P6 恢复官方配置命令）"
fi
if ! grep -q 'we2ai::commands_apply::we2ai_restore_plan' src-tauri/src/lib.rs; then
  err "src-tauri/src/lib.rs: 找不到 we2ai::commands_apply::we2ai_restore_plan 注册（功能 17：恢复确认弹窗的独立计划命令，Opus 复核中危项 2）"
fi
# Codex 验收 Y1：apply 前只等"CC Switch 是否在运行"这一项快速检测的独立
# 命令，同样是 `we2ai_` 前缀、同样只需要确认出现在 lib.rs 的注册列表里。
if ! grep -q 'we2ai::commands_apply::we2ai_cc_switch_running_quick' src-tauri/src/lib.rs; then
  err "src-tauri/src/lib.rs: 找不到 we2ai::commands_apply::we2ai_cc_switch_running_quick 注册（Codex 验收 Y1：apply 前快速检测命令）"
fi

# 4.10 Codex 验收报告 r0 偏差修复项 A：数据根（~/.we2ai）权限收紧失败不能
# 只打日志，必须在写入任何含 Key 数据（数据库行 / 工具 live 文件）之前被
# 拒绝，且必须 fail-closed（Opus 复核中危项 M1）。lib.rs 必须记录
# harden_data_root() 的结果，且记录点必须早于 We2aiSessionState 被
# app.manage()（会话管理器一旦可用，登录命令就可能开始写 ~/.we2ai）；
# tighten_all() 是 apply_provider_tool（Claude/Codex）与 apply_workbuddy
# 共同的写入前置，必须紧接着调用 ensure_data_root_hardened()；mode.rs 不得
# 出现 `unwrap_or(true)` 这类"未记录/异常时默认放行"的写法（fail-open 回归）。
lib_rs=src-tauri/src/lib.rs
if ! grep -q 'we2ai::mode::record_harden_result(' "$lib_rs"; then
  err "$lib_rs: 找不到 we2ai::mode::record_harden_result 调用，harden_data_root() 的失败结果不会被记录，apply 无法据此拒绝写入（偏差修复项 A）"
else
  # 排除纯注释行再取行号（保留原始行号）：注释里若提到这两个调用的字面
  # 写法（供人类读者理解顺序要求），不能被误当成真正的调用点。
  record_line="$(grep -n 'we2ai::mode::record_harden_result(' "$lib_rs" | grep -vE '^[0-9]+:[[:space:]]*//' | head -1 | cut -d: -f1)"
  manage_line="$(grep -n 'app\.manage(We2aiSessionState(' "$lib_rs" | grep -vE '^[0-9]+:[[:space:]]*//' | head -1 | cut -d: -f1)"
  if [[ -z "$manage_line" ]]; then
    err "$lib_rs: 找不到 app.manage(We2aiSessionState(...))，无法核对与 record_harden_result 的先后顺序（偏差修复项 A / Opus 复核中危项 M1）"
  elif [[ "$record_line" -gt "$manage_line" ]]; then
    err "$lib_rs: we2ai::mode::record_harden_result（第 ${record_line} 行）必须早于 app.manage(We2aiSessionState(...))（第 ${manage_line} 行），否则会话管理器可用后、收紧结果记录前存在窗口（Opus 复核中危项 M1）"
  fi
fi
apply_rs=src-tauri/src/we2ai/apply.rs
if ! grep -A1 'pub(crate) fn tighten_all(claude_settings: &Path)' "$apply_rs" | grep -q 'ensure_data_root_hardened()'; then
  err "$apply_rs: tighten_all() 必须紧接着调用 ensure_data_root_hardened()，否则数据根收紧失败时仍可能写入含 Key 的数据（偏差修复项 A）"
fi
mode_rs=src-tauri/src/we2ai/mode.rs
if grep -q 'unwrap_or(true)' "$mode_rs"; then
  err "$mode_rs: 出现 unwrap_or(true)，data_root_hardened() 必须 fail-closed（未记录/锁中毒都视为未通过），不能默认放行（Opus 复核中危项 M1）"
fi

# 4.11 N1（用户新决定）：WE2AI 用户界面文案不得残留 "CC Switch" 字样（含
# 大小写与连字符/下划线/空格变体：CC Switch / CC-Switch / ccswitch /
# cc-switch / cc_switch，大小写不敏感），只改显示文案，进程检测本身（bundle
# id `com.ccswitch.desktop`、可执行名 `cc-switch`/`cc-switch.exe`）保留不变。
# 这是按行的启发式扫描，不是真正的语法分析：
#   - 排除整行是注释的行（Rust `//`/`///`/`//!`、TS/CSS `//`/`*`/`/*` 开头）；
#   - 排除 `*_tests.rs` 文件（独立测试文件）；
#   - 只跳过 `#[cfg(test)]` **紧跟 `mod <name> {` 的整块**（按花括号深度找到
#     匹配的收尾 `}`），而不是遇到文件里第一个 `#[cfg(test)]` 就整段停止
#     扫描——本仓库里同一文件常有多处 `#[cfg(test)]`（散落的测试钩子/字段/
#     单个函数），只有真正的 `mod tests { ... }` 才是测试模块，其余一律照常
#     扫描（Opus 复核中危项 R1：旧版在第一个 `#[cfg(test)]` 就 `exit`，导致
#     apply.rs/session.rs/mode.rs/workbuddy.rs 中段以后的生产代码完全没被
#     扫描过）；
#   - 匹配加了前后单词边界，排除 `ccSwitchRunning`/`cc_switch_running` 这类
#     标识符（"switch" 后面紧跟标识符字符时不算命中）。
# 不保证覆盖跨行拼接等刁钻写法。
brand_pattern='(^|[^A-Za-z0-9_])cc[ _-]?switch([^A-Za-z0-9_]|$)'
# 白名单按"文件 + 精确字面量"配对，收窄到 detect.rs 的进程检测调用形态本身
# （Opus 复核中危项 R2：旧版按字符串内容判断，任何文件只要含这几个子串都会
# 被放行；改为同时要求命中来自 detect.rs）。
#
# Codex 验收 X4③：仅要求"文件是 detect.rs"还不够——`$content` 是
# grep -n 输出的整行（"行号:整行原文"），旧版用 `case "$content" in
# *'子串'*)` 做的是"这一行任意位置包含这个子串就整行放行"；如果同一行
# 除了合法的检测调用之外还夹带其他需要被扫到的品牌残留文本，也会被这个
# 子串命中一并放过。改成：去掉行号前缀与首尾空白后，要求剩下的整行内容
# 逐字等于下列四种已知检测调用形态之一（不是"包含"）。
is_whitelisted_process_detection_literal() {
  local file="$1" content="$2" text
  case "$file" in
    */we2ai/detect.rs) ;;
    *) return 1 ;;
  esac
  text="${content#*:}"
  text="${text#"${text%%[![:space:]]*}"}"
  text="${text%"${text##*[![:space:]]}"}"
  case "$text" in
    'cmd.args(["find", "bundleid=com.ccswitch.desktop"])') return 0 ;;
    'cmd.args(["/FI", "IMAGENAME eq cc-switch.exe", "/NH"])') return 0 ;;
    'if stdout.to_ascii_lowercase().contains("cc-switch.exe") {') return 0 ;;
    'cmd.args(["-x", "cc-switch"])') return 0 ;;
  esac
  return 1
}

# apply.rs 的 `looks_like_brand_residue`（Codex 验收 X2②/X6：额外变更 display
# 文案的中性化判定）需要把品牌残留的字面量本身写进代码里才能识别、进而把它
# 从确认弹窗的界面文案里替换掉——这几个字面量本身是内部匹配用途，从不
# 展示给用户，性质与 detect.rs 的进程检测标识相同：都不适合用"登记豁免
# 界面可见文字"的宽松方式处理，因此同样只精确匹配这一行本身（文件 +
# 逐字整行），不做子串匹配、也不做别的形式的放宽。
is_whitelisted_brand_residue_matcher_literal() {
  local file="$1" content="$2" text
  case "$file" in
    */we2ai/apply.rs) ;;
    *) return 1 ;;
  esac
  text="${content#*:}"
  text="${text#"${text%%[![:space:]]*}"}"
  text="${text%"${text##*[![:space:]]}"}"
  case "$text" in
    'const BRAND_RESIDUE_NEEDLES: [&str; 4] = ["ccswitch", "cc-switch", "cc_switch", "cc switch"];') return 0 ;;
  esac
  return 1
}

# 跳过"紧跟 `mod <name> {` 的 `#[cfg(test)]` 整块"，其余行原样透传（保留
# 原始行号，供 grep -n 使用）。
#
# Opus 复核低危项 S4：花括号计数不能把字符串/字符字面量、行注释里的
# `{`/`}` 也算进去——测试模块内一行 `let s = "{";` 会让计数多算一个从不
# 被抵消的开括号，导致"跳过"状态永远退不出来，一路跳到文件尾，把这之后
# （如果有）的生产代码也漏扫。计数前用 `strip_lexical_noise()` 粗略剔除
# 双引号字符串（含反斜杠转义）、字符字面量（含转义）、`//` 行注释，以及
# Rust 原始字符串 `r#"…"#`（含跨行——原始字符串允许内部出现未转义的 `"`
# 和裸换行，必须在遇到匹配的收尾定界符之前，把跨越的每一行都当作字符串
# 内容整行剔除，不能只按单行处理）。跳出"跳过"状态那一刻，额外断言当前
# 这一行的原始文本里确实含有 `}`（S4 要求的"断言该行确为 `}`"）——按剔除
# 只做减法、不改变幸存字符相对位置的构造方式，这个断言在数学上必然成立，
# 这里显式检查是为了在假设被违反时能看见诊断信息，而不是静默按错误的
# 行号继续。
we2ai_strip_inline_test_mod() {
  awk '
    BEGIN { SQ = sprintf("%c", 39) }  # 单引号字符（避免在单引号包裹的 awk 脚本里直接写字面量）
    function is_mod_open(l) {
      return (l ~ /^[[:space:]]*(pub([(][a-z]*[)])?[[:space:]]+)?mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\{[[:space:]]*$/)
    }
    # 剔除字符串/字符字面量/行注释/块注释后的干净文本，供花括号计数使用。
    # 输入/输出都是单行；跨行状态（原始字符串 in_raw_string、块注释
    # in_block_comment、普通双引号字符串 in_str）都用全局变量在多次调用之间
    # 保持——Rust 的普通字符串本可以用行尾反斜杠续行，块注释 `/* ... */`
    # 也可以跨多行，这两类此前只按单行处理，跨行时会把后续几行也一起吞掉
    # 或者相反漏判（Opus 复核低危项 T3）。字符字面量与生命周期标注
    # （如 static 生命周期标注）单靠单引号无法区分：字符字面量总是紧跟一个
    # 转义序列或单字符、再跟一个收尾单引号，生命周期标注后面不会再出现
    # 单引号——用向前看几个字符的方式区分，而不是无条件把单引号当成字符串
    # 起点。
    function strip_lexical_noise(line,    i, n, c, out, closer, j, hashes, idx, jdx, k, nx, found, m) {
      n = length(line)
      out = ""
      i = 1
      if (in_raw_string) {
        closer = raw_string_delim
        idx = index(line, closer)
        if (idx == 0) {
          return ""  # 整行仍在原始字符串里，没有出现收尾定界符。
        }
        in_raw_string = 0
        i = idx + length(closer)
      }
      while (i <= n) {
        if (in_block_comment) {
          # Rust 的块注释允许嵌套（`/* outer /* inner */ still outer */`），
          # `in_block_comment` 是深度计数而不是布尔值：找到的下一个 `/*`（嵌套
          # 加深）或 `*/`（退一层，退到 0 才真正算退出注释）里更靠前的那个，
          # 逐个处理，而不是不管嵌套、只找最近的一个 `*/` 就直接退出（Codex
          # 验收 X4②：旧版遇到内层的 `*/` 就当整个注释结束，把外层注释剩余
          # 部分里的 `{` 误当成真实代码计入花括号深度）。
          while (i <= n) {
            idx = index(substr(line, i), "*/")
            jdx = index(substr(line, i), "/*")
            if (idx == 0 && jdx == 0) {
              i = n + 1
              break
            }
            if (jdx > 0 && (idx == 0 || jdx < idx)) {
              in_block_comment++
              i = i + jdx + 1
            } else {
              in_block_comment--
              i = i + idx + 1
              if (in_block_comment == 0) break
            }
          }
          if (in_block_comment > 0) {
            return out  # 整行剩余部分仍在（可能嵌套的）块注释里，没有完全收尾。
          }
          continue
        }
        if (in_str) {
          c = substr(line, i, 1)
          if (c == "\\") { i += 2; continue }
          if (c == "\"") { in_str = 0 }
          i++
          continue
        }
        c = substr(line, i, 1)
        if (c == "/" && substr(line, i + 1, 1) == "/") {
          break  # 行注释：本行剩余部分全部丢弃。
        }
        if (c == "/" && substr(line, i + 1, 1) == "*") {
          in_block_comment = 1
          i += 2
          continue
        }
        # 原始字符串起始：`r` 或 `br`，后面跟 0 个以上 `#`，再跟 `"`。
        if (c == "r" || (c == "b" && substr(line, i + 1, 1) == "r")) {
          j = i + (c == "b" ? 2 : 1)
          hashes = 0
          while (substr(line, j, 1) == "#") { hashes++; j++ }
          if (substr(line, j, 1) == "\"") {
            closer = "\""
            for (k = 0; k < hashes; k++) { closer = closer "#" }
            idx = index(substr(line, j + 1), closer)
            if (idx == 0) {
              in_raw_string = 1
              raw_string_delim = closer
              i = n + 1
              continue
            } else {
              i = j + 1 + idx + length(closer) - 1
              continue
            }
          }
        }
        if (c == "\"") { in_str = 1; i++; continue }
        if (c == SQ) {
          nx = substr(line, i + 1, 1)
          if (nx == "\\") {
            # 转义字符字面量（换行符/反斜杠/引号本身/十六进制或 unicode 转义
            # 等），粗略地在接下来几个字符里找收尾单引号。
            found = 0
            for (m = i + 2; m <= i + 10 && m <= n; m++) {
              if (substr(line, m, 1) == SQ) { found = m; break }
            }
            if (found) { i = found + 1; continue }
            else { i++; while (i <= n && substr(line, i, 1) ~ /[A-Za-z0-9_]/) i++; continue }
          } else if (nx != "" && substr(line, i + 2, 1) == SQ) {
            # 普通单字符字面量（引号包住一个字符再收尾）。
            i = i + 3
            continue
          } else {
            # 生命周期标注：跳过引号和随后的标识符字符，不进入字符串状态。
            i++
            while (i <= n && substr(line, i, 1) ~ /[A-Za-z0-9_]/) i++
            continue
          }
        }
        out = out c
        i++
      }
      return out
    }
    {
      raw_line = $0
      if (skipping) {
        clean = strip_lexical_noise(raw_line)
        o = gsub(/\{/, "{", clean)
        c = gsub(/\}/, "}", clean)
        depth += o - c
        if (depth <= 0) {
          skipping = 0
          if (raw_line !~ /\}/) {
            # Codex 验收 Y2：此前这里只打印到 stderr，awk 自身仍然以退出码 0
            # 结束——调用方（`we2ai_verify_scan_or_die`）能不能发现这个内部
            # 断言失败，完全要看它凑巧有没有顺带触发行数不匹配；断言真正
            # 失败时必须让 awk 本身非零退出，不依赖旁的不变量凑巧生效。
            print "we2ai_strip_inline_test_mod: 内部断言失败——退出跳过状态但原始行不含 }：" raw_line > "/dev/stderr"
            exit 2
          }
        }
        print ""   # 占位，保持行号不变，内容清空避免误命中
        next
      }
      if (pending) {
        pending = 0
        if (is_mod_open(raw_line)) {
          skipping = 1
          depth = 1
          print ""
          next
        }
        print raw_line
        next
      }
      if (raw_line ~ /^#\[cfg\(test\)\]/) {
        pending = 1
        print raw_line
        next
      }
      print raw_line
    }
    END {
      # Codex 验收 Y2：文件在仍处于"跳过"状态时结束（`mod tests { ... }`
      # 从未真正闭合、花括号深度从未回到 0）——此前这种情况下行数不变量
      # 依然成立（跳过状态下每行仍然打印一个占位空行，行数没有减少），
      # 不会被 `we2ai_verify_scan_or_die` 的行数检查捕捉到，导致文件末尾
      # 剩余的全部生产代码被永久当成"测试模块内容"悄悄跳过、从未报告任何
      # 错误。显式在这里检查并让 awk 非零退出。
      if (skipping) {
        print "we2ai_strip_inline_test_mod: 内部断言失败——文件在跳过状态未闭合时结束（mod tests 未找到匹配的收尾 }）：" FILENAME > "/dev/stderr"
        exit 2
      }
    }
  ' "$1"
}

# Codex 验收 X4①：任何一步扫描输入/执行失败都必须让整个脚本非零退出，不能
# 被悄悄吞掉——`set -euo pipefail` 在这里不够用。已证实的具体原因分开列，
# 不合并成一个笼统的推测（Codex 验收 Z6：此前这里在"未确认"的机制上继续
# 断言，把命令替换与 herestring 混为一谈）：
#   已证实 A（临时文件创建失败）：herestring（`<<<`）/`mktemp` 在只读沙箱
#      环境下确实会失败——直接注入验证过：`mktemp` 因文件系统只读而报错时，
#      自测正确返回非零，且没有真的创建出临时文件。这类失败发生的位置
#      通常是 `while` 循环的条件（或为它准备输入的重定向），而 `errexit`
#      明确不对 `while`/`until` 的条件生效——会被"当次循环就当作 0 次
#      迭代"悄悄吞掉。
#   已证实 B（退出码被吞）：`... | grep ... || true` 这类写法，以及
#      `find`/`grep` 的真实退出码在赋值语句本身就被 `errexit` 拦截、根本
#      没运行到下一行的检查代码，都会让真正的执行失败被当成"没有命中"
#      悄悄放过。本轮直接故障注入复验过：`find` 返回 73 → 整个脚本退出
#      1；品牌匹配阶段 `grep` 返回 2 → 退出 1；`grep` 返回 1（无匹配，
#      正常情况）→ 不受影响、仍退出 0；真实扫描函数在跳过状态未闭合的
#      输入上 → 退出 2。这些都是本轮在真实主扫描循环（不是自测里包在
#      `if (...)` 子 shell 条件里的隔离用例）上跑出来的结果，不是推测。
#   未确认（曾观察到但没有定位到确切机制）：`region="$(cmd)"` 整体捕获
#      **较大输出**（一份 800 多行、含 `#[cfg(test)] mod tests {}` 块的
#      真实源文件即可复现）时，捕获到的内容比 `cmd` 实际写出的少，`cmd`
#      本身退出码却是 0、没有任何报错。已经排除的假设：这不是 bash 3.2
#      本身对较大字符串命令替换的固有缺陷（实测常规、可写的 bash 3.2.57
#      环境下，命令替换可以完整保留几十万行输出，不会仅因为体积就截断）。
#      是否与"已证实 A"的临时文件失败同源尚未验证，不下结论。不管具体
#      诱因是什么，把同一个 `cmd` 的输出直接用管道接给下一个命令
#      （`cmd | next`），或者只捕获 `next` 处理后的一个很小的结果（如
#      `cmd | wc -l` 只捕获一个数字），观察到的问题都不会出现，因此仍然
#      值得在"命令替换整体捕获较大字符串"这一种用法上加一道不依赖具体
#      诱因的兜底——但这是防御性措施，不是"已经查明并规避了某个机制"的
#      断言。
#
# 因此这里的验证函数不再把整份扫描结果整体捕获成一个 bash 变量，只捕获
# 两个小整数（源文件行数、扫描结果行数）来验证不变量：`strip_lexical_noise`
# 逐行处理，永远一行输入对应一行输出（跳过状态时打印占位空行，其余原样
# 透传），无论走哪条分支都不会改变行数，只要两个数对不上就说明扫描管道
# 某处提前中断或产生了不完整的结果——这个不变量的检测不依赖于失败具体
# 发生在哪一步（herestring、进程替换、awk 本身、命令替换捕获不完整），
# 只要结果不对就一律判定为失败。真正需要用到扫描结果内容的调用方（主
# 扫描循环、自测）必须在这个函数返回成功之后，用**管道**（不是命令替换）
# 重新消费一次 `we2ai_strip_inline_test_mod` 的输出去提取少量命中行——
# 命中行本身很短、数量很少，命令替换捕获它们是安全的，真正大的中间结果
# 全程只走管道。
we2ai_verify_scan_or_die() {
  local f="$1" expected_lines actual_lines
  expected_lines="$(awk 'END{print NR}' "$f")"
  if ! actual_lines="$(we2ai_strip_inline_test_mod "$f" | awk 'END{print NR}')"; then
    echo "FAIL: $f: we2ai_strip_inline_test_mod 执行失败（退出码非 0），扫描结果不可信，判定为守卫失效而非通过" >&2
    exit 1
  fi
  if [[ "$actual_lines" != "$expected_lines" ]]; then
    # 注意：CJK 全角标点紧跟裸 `$var`（不加花括号）在部分 bash/locale 组合下
    # 会被 `set -u` 误判成"变量名的一部分"触发 unbound variable（真实复现于
    # 本仓库的 bash 环境）；一律用 `${var}` 带花括号形式，两侧用花括号明确
    # 变量名边界，不能省略。
    echo "FAIL: $f: 扫描输出行数（${actual_lines}）与源文件行数（${expected_lines}）不一致，怀疑扫描管道中途失败或被截断，判定为守卫失效而非通过" >&2
    exit 1
  fi
}

# Codex 验收 Y2：`find` 通过进程替换喂给 `while read` 时，`find` 自身的退出码
# 不会被观察到——`< <(find ...)` 不是一个可以直接 `$?` 检查的简单命令，
# `find` 失败（权限问题、路径不存在等，实测注入过 73）时这个循环只是读到
# 0 个文件、悄悄跳过，不会被当成错误。改为先用命令替换捕获 `find` 的输出
# （换成换行分隔而不是 `-print0`：NUL 字节在 `$(...)` 里会被截断，这是
# bash 的已知限制，仓库源码路径不含换行符，可以接受）、显式检查退出码，
# 再用进程替换把这份已经验证过的列表喂给循环体（继续避免 herestring 依赖
# 磁盘临时文件、避免把循环体放进子 shell 丢失 `we2ai_brand_leak` 累积）。
we2ai_check_find_status() {
  local status="$1" context="$2"
  if (( status != 0 )); then
    echo "FAIL: ${context}：find 枚举失败（退出码 ${status}），判定为守卫失效而非通过" >&2
    exit 1
  fi
}

# grep 的退出码只有 0（找到匹配）与 1（没有匹配）是正常结果，其余（参数
# 错误、正则语法错误、读取输入失败等）必须让守卫整体失败——此前用
# `... || true` 兜底，会把这些真正的执行故障也一并当成"没有命中"悄悄放过
# （Codex 验收 Y2：故障注入 grep 返回 2 后，旧写法仍报 "all passed"）。
we2ai_check_grep_status() {
  local status="$1" context="$2"
  if (( status > 1 )); then
    echo "FAIL: ${context}：grep 执行失败（退出码 ${status}），判定为守卫失效而非通过" >&2
    exit 1
  fi
}

# 对 grep 做"捕获输出 + 检查真实退出码"这一组合动作，且总是以退出码 0
# 返回（除非判定为真失败并直接 `exit 1`）：grep 退出码 1（无匹配）是绝大
# 多数文件的正常情况，而 `set -e` 会把 `var="$(grep ...)"` 这条赋值语句
# 本身的非零退出码当成命令失败、不等运行到下一行检查就直接终止整个脚本
# ——这正是 Codex 验收 Y2 复测发现的真问题：`we2ai_check_grep_status "$?"`
# 写在赋值语句的下一行，本意是"赋值之后再检查"，但赋值语句本身在多数
# 文件上就会先因为 grep 返回 1 被 errexit 拦下，下一行的检查代码根本没有
# 机会运行，跟旧版的 `|| true` 殊途同归——都是"看起来检查了、实际上检查
# 代码从未被执行到"。用 `grep ... || status=$?` 这个不触发 errexit 的写法
# 先把真实状态接住，再交给 `we2ai_check_grep_status` 判断，调用方只需要
# `result="$(we2ai_grep_or_die context grep-args...)"`，不用在外面再单独
# 处理 `$?`。
we2ai_grep_or_die() {
  local context="$1"
  shift
  local out status
  out="$(grep "$@")" || status=$?
  we2ai_check_grep_status "${status:-0}" "$context"
  printf '%s' "$out"
}

# 守卫自测（Opus 复核中危项 R1、低危项 S4，Codex 验收 X4）：验证 4.11 的
# 核心逻辑本身没有回归到"遇到第一个 #[cfg(test)] 就整段停止扫描"的旧
# bug，且真正的 mod tests {} 块内容确实不被误报；同时验证花括号计数不会
# 被测试模块内部的字符串/字符字面量（其中恰好含花括号）搞乱，导致跳过
# 状态该结束时没结束、把 mod tests 块之后的生产代码也漏扫。构造一个临时
# 探针文件（`mktemp -d`，退出前自动清理，不在仓库留任何文件）：
#   1) 不相关的内联 `#[cfg(test)] struct`；
#   2) "文件后段"的生产代码违规三行（模拟 apply.rs/session.rs/mode.rs/
#      workbuddy.rs 中"先有散落的 #[cfg(test)] 测试钩子，后面还有很长一段
#      生产代码"的真实结构）；
#   3) 真正的 `mod tests { ... }` 块，内部既写同款违规文本，也故意写一个
#      含花括号的字符串字面量、一个含花括号的字符字面量、一处生命周期标注
#      （验证不会被误当成字符字面量起点）、一个跨行的普通字符串（行尾反
#      斜杠续行，含花括号）、一个单行块注释、一个多行块注释（都含花括号，
#      T3），以及一个**嵌套**块注释（含花括号，X4②）；
#   4) mod tests 块**之后**的第四段生产代码违规——只有花括号计数没被搞乱、
#      跳过状态在 mod tests 真正的收尾花括号处准确结束，这一段才会被扫到。
# 用法：`./scripts/we2ai/check-guards.sh --self-test-brand-guard`。
if [[ "${1:-}" == "--self-test-brand-guard" ]]; then
  self_test_failed=0

  # X4①：mktemp 失败必须给出明确信息并非零退出，不能静默继续、让后面的
  # 探针写入一个从未真正创建的目录（那样会在别的地方产生更难懂的报错）。
  if ! self_test_dir="$(mktemp -d)"; then
    echo "self-test FAIL: mktemp -d 创建自测临时目录失败，无法继续自测" >&2
    exit 1
  fi
  trap 'rm -rf "$self_test_dir"' EXIT
  probe="$self_test_dir/probe.rs"
  {
    echo 'fn producer_code_before() -> i32 { 1 }'
    echo ''
    echo '#[cfg(test)]'
    echo 'struct Unrelated { field: i32 }'
    echo ''
    echo 'fn producer_code_after_inline_cfg_test() -> i32 {'
    echo '    // 注入点：模拟"散落的内联 #[cfg(test)] 之后、真正的 mod tests'
    echo '    // 之前"的生产代码，三种大小写/连字符/空格变体各一行。'
    echo '    let _a = "CC Switch";'
    echo '    let _b = "cc switch";'
    echo '    let _c = "Cc-Switch";'
    echo '    2'
    echo '}'
    echo ''
    echo '#[cfg(test)]'
    echo 'mod tests {'
    echo '    // 真正的测试模块：这里面即使写 CC Switch 也不该被扫到。'
    echo '    const NOTE: &str = "CC Switch inside a real test mod, must NOT be flagged";'
    echo '    // S4：花括号计数不能被字符串/字符字面量里的花括号搞乱。'
    printf '    const BRACE_STR: &str = "{";\n'
    printf "    const BRACE_CHAR: char = '{';\n"
    echo '    fn with_lifetime() -> &'"'"'static str {'
    echo '        "y"'
    echo '    }'
    echo '    // T3：跨行的普通字符串（行尾反斜杠续行）与块注释，二者都'
    echo '    // 含花括号，且都跨越多行。'
    echo '    const MULTILINE_STR: &str = "start \'
    echo '        middle { still same string \'
    echo '        end";'
    echo '    /* a single-line block comment with a brace { inside */'
    echo '    /* a multi-line block comment'
    echo '       with a brace { inside'
    echo '       still going */'
    echo '    // X4②：嵌套块注释——内层的 */ 不该被当成整个注释的收尾，里面'
    echo '    // 的 { 必须仍然算在注释里，不能被当成真实代码计入花括号深度。'
    echo '    /* outer comment /* inner comment */ still outer, with a stray { in here */'
    echo '}'
    echo ''
    echo 'fn producer_code_after_real_test_mod() -> i32 {'
    echo '    // 只有跳过状态在 mod tests 真正的收尾花括号处准确结束，'
    echo '    // 这一行才会被扫描器看到并检出。'
    echo '    let _d = "cc_switch";'
    echo '    4'
    echo '}'
  } >"$probe"

  we2ai_verify_scan_or_die "$probe"
  self_test_hits="$(we2ai_strip_inline_test_mod "$probe" | grep -inE "$brand_pattern" | grep -vE '^[0-9]+:[[:space:]]*(//|/\*|\*)' || true)"
  self_test_hit_count="$(printf '%s\n' "$self_test_hits" | grep -c '.' || true)"

  if [[ "$self_test_hit_count" -eq 4 ]]; then
    echo "self-test PASS: 生产代码里的 4 处 CC Switch 变体（含 mod tests 块之后那一处）被正确检出，真正 mod tests 块内的同款文本、花括号字面量与嵌套块注释均未被误报/未打乱计数"
  else
    echo "self-test FAIL: 期望检出 4 处，实际检出 ${self_test_hit_count} 处：" >&2
    printf '%s\n' "$self_test_hits" >&2
    self_test_failed=1
  fi

  # X4①负例：扫描管道产出被截断（行数对不上）时，必须判定为失败而不是
  # "碰巧没扫到就当作通过"。在子 shell 里局部替换成一个故意截断输出的桩
  # 函数（只输出源文件的第一行），不影响脚本其余部分对真实实现的使用。
  if (
    we2ai_strip_inline_test_mod() { head -n 1 "$1"; }
    we2ai_verify_scan_or_die "$probe" >/dev/null 2>/dev/null
  ); then
    echo "self-test FAIL: 扫描输出被截断（行数与源文件不符）时，we2ai_verify_scan_or_die 竟然返回 0，未能检测出扫描失败" >&2
    self_test_failed=1
  else
    echo "self-test PASS: 扫描输出被截断时会被判定为失败并非零退出，不会被当成'碰巧没扫到'而放行"
  fi

  # X4①负例（第二例）：修复过程中在真实源文件 keys.rs 上实测复现过"命令
  # 替换 `region="$(cmd)"` 整体捕获较大输出时，捕获到的内容比 `cmd` 实际
  # 写出的少"这一现象。根因未确认（Codex 验收 Z6：已排除"bash 3.2 本身对
  # 较大字符串命令替换有固有缺陷"这个假设——常规、可写的 bash 3.2.57
  # 环境下命令替换可以完整保留几十万行输出；是否与只读沙箱下临时文件
  # 创建失败同源尚未验证，不下结论）。不管具体诱因是什么，"全程走管道、
  # 不整体捕获大字符串"这个写法本身是有效的防御性兜底，构造一个足够大
  # 的探针文件（在真正违规行之前填充大量占位内容）验证：① `we2ai_verify_
  # scan_or_die` 不会对这份合法的大文件误报失败；② 随后用管道从
  # `we2ai_strip_inline_test_mod` 提取到的命中确实包含填充内容之后、文件
  # 末尾的那一条真实违规——两点合在一起才能证明这个写法真的有效，而不是
  # 恰好躲开了旧探针的问题。
  large_probe="$self_test_dir/large_probe.rs"
  {
    i=0
    while [[ "$i" -lt 900 ]]; do
      echo "// filler line $i to pad this file with enough content to exercise the pipe-only scanning path"
      i=$((i + 1))
    done
    echo 'fn producer_code_at_the_very_end() -> i32 {'
    echo '    let _e = "cc-switch";'
    echo '    5'
    echo '}'
  } >"$large_probe"
  if ! we2ai_verify_scan_or_die "$large_probe" 2>/dev/null; then
    echo "self-test FAIL: we2ai_verify_scan_or_die 对一份合法的大文件误报失败" >&2
    self_test_failed=1
  elif ! we2ai_strip_inline_test_mod "$large_probe" \
    | grep -inE "$brand_pattern" | grep -vE '^[0-9]+:[[:space:]]*(//|/\*|\*)' \
    | grep -q 'cc-switch'; then
    echo "self-test FAIL: 大文件末尾的真实违规没有被扫到，怀疑命令替换捕获较大扫描输出时不完整" >&2
    self_test_failed=1
  else
    echo "self-test PASS: 较大文件末尾的违规仍被正确扫到（全程走管道、不整体捕获大字符串这个防御性写法本身有效）"
  fi

  # X4③负例：白名单必须锚定整行，而不是"行内任意位置含有这个子串就整行
  # 放行"。构造一行"合法检测调用文本 + 额外的品牌残留文本"拼在一起的假
  # 命中，确认不再被放行（旧版会因为整行仍然包含合法调用子串而误放行）。
  tampered_hit='213:            cmd.args(["find", "bundleid=com.ccswitch.desktop"])  // 顺手提一句 cc-switch 也在用'
  if is_whitelisted_process_detection_literal "src-tauri/src/we2ai/detect.rs" "$tampered_hit"; then
    echo "self-test FAIL: 白名单对'合法检测调用 + 额外品牌残留文本'拼在同一行的假命中仍然放行，说明锚定的是子串而不是整行" >&2
    self_test_failed=1
  else
    echo "self-test PASS: 白名单不再对'整行任意位置含有已知子串'放行，混入额外文本后正确判定为未放行"
  fi
  # 正例照旧必须放行，证明上面的负例不是因为规则整体失效才被拒绝的。
  clean_hit='213:            cmd.args(["find", "bundleid=com.ccswitch.desktop"])'
  if ! is_whitelisted_process_detection_literal "src-tauri/src/we2ai/detect.rs" "$clean_hit"; then
    echo "self-test FAIL: 白名单连 detect.rs 里真正的检测调用本身都不再放行，规则收得过紧" >&2
    self_test_failed=1
  else
    echo "self-test PASS: 白名单仍然放行 detect.rs 里真正的检测调用本身"
  fi

  # Codex 验收 Y2 负例 1：`find` 失败（实测注入过退出码 73）必须让流程
  # 非零退出，不能被"读到 0 个文件就当作正常"悄悄放过。用一个返回非零的
  # 桩函数局部替换 `find`，只在这个子 shell 里生效。
  if (
    find() { return 73; }
    file_list="$(find /nonexistent -name '*.rs')"
    we2ai_check_find_status "$?" "self-test probe"
  ); then
    echo "self-test FAIL: find 返回非零（模拟退出码 73）时未能让流程非零退出" >&2
    self_test_failed=1
  else
    echo "self-test PASS: find 失败（模拟退出码 73）被正确判定为守卫失效"
  fi

  # Codex 验收 Y2 负例 2：grep 返回真正的执行错误（不是"无匹配"的退出码
  # 1）必须让流程非零退出；同时确认退出码 1（无匹配，正常情况）不会被
  # 误判为失败——两个方向都要验证，否则不能证明这条判断规则本身是对的。
  if (
    grep() { return 2; }
    matched="$(grep -inE "pattern" /dev/null)"
    we2ai_check_grep_status "$?" "self-test probe"
  ); then
    echo "self-test FAIL: grep 返回真实错误码（模拟退出码 2）时未能让流程非零退出" >&2
    self_test_failed=1
  else
    echo "self-test PASS: grep 执行错误（模拟退出码 2）被正确判定为守卫失效"
  fi
  if (
    grep() { return 1; }
    matched="$(grep -inE "pattern" /dev/null)"
    we2ai_check_grep_status "$?" "self-test probe"
  ); then
    echo "self-test PASS: grep 返回 1（无匹配）被正确视为正常，不误判为失败"
  else
    echo "self-test FAIL: grep 返回 1（无匹配，属于正常情况）被错误地当成了失败" >&2
    self_test_failed=1
  fi

  # Codex 验收 Y2 负例 3：跳过状态结束但原始行不含 `}` 这个内部断言必须让
  # awk 本身以非零退出码结束，不能只打印到 stderr、自己却仍报退出码 0。
  # 真实实现里这个分支只应该在未知的计数 bug 下触发，这里用一段独立的
  # 最小 awk 片段复现同样的判断结构，直接验证"断言触发 → 非零退出"这条
  # 兜底路径本身有效，不依赖真的先找到一个计数 bug。
  if printf 'no closing brace here\n' | awk '
    BEGIN { skipping = 1; depth = 1 }
    {
      depth = 0
      if (depth <= 0) {
        skipping = 0
        if ($0 !~ /\}/) {
          print "assertion probe: exiting skip state without a closing brace" > "/dev/stderr"
          exit 2
        }
      }
    }
  ' 2>/dev/null; then
    echo "self-test FAIL: 内部断言（跳过状态结束但原始行不含 }）触发时 awk 未能以非零退出码结束" >&2
    self_test_failed=1
  else
    echo "self-test PASS: 内部断言（跳过状态结束但原始行不含 }）会让 awk 以非零退出码结束"
  fi

  # Codex 验收 Y2 负例 4：`mod tests { ... }` 从未闭合（文件在跳过状态下
  # 结束）必须被 END 块里的检查兜住——这种情况下行数不变量本身依然成立
  # （跳过状态下每行仍打印一个占位空行），不能指望它顺带发现这个问题。
  unclosed_probe="$self_test_dir/unclosed_probe.rs"
  {
    echo '#[cfg(test)]'
    echo 'mod tests {'
    echo '    const NOTE: &str = "never closed";'
  } >"$unclosed_probe"
  if we2ai_strip_inline_test_mod "$unclosed_probe" >/dev/null 2>/dev/null; then
    echo "self-test FAIL: mod tests 块未闭合时 we2ai_strip_inline_test_mod 仍然以退出码 0 结束" >&2
    self_test_failed=1
  else
    echo "self-test PASS: mod tests 块未闭合（跳过状态未闭合结束）被正确判定为失败"
  fi

  if [[ "$self_test_failed" == 0 ]]; then
    exit 0
  else
    exit 1
  fi
fi

we2ai_brand_leak=""
# `|| we2ai_find_status=$?` 而不是让下一行去读 `$?`（Codex 验收 Y2）：
# `find` 正常情况下返回 0（哪怕枚举到 0 个文件），但一旦它真的失败，
# `set -e` 会把这条赋值语句本身的非零退出码当成命令失败、直接终止整个
# 脚本——下一行的检查代码根本没有机会运行，跟旧版的悄悄放过殊途同归。
we2ai_rs_files="$(find src-tauri/src/we2ai -name '*.rs')" || we2ai_find_status=$?
we2ai_check_find_status "${we2ai_find_status:-0}" "src-tauri/src/we2ai 下的 .rs 文件枚举"
unset we2ai_find_status
while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  case "$f" in
    *_tests.rs) continue ;;
  esac
  we2ai_verify_scan_or_die "$f"
  # 用 `we2ai_grep_or_die` 而不是"赋值后再读 `$?`"（Codex 验收 Y2）：grep
  # 退出码 1（无匹配）是绝大多数文件的正常情况，直接在赋值语句上会被
  # errexit 拦下，下一行的 `we2ai_check_grep_status "$?"` 根本运行不到——
  # 这正是本轮复测在真实扫描（而不是自测探针）上发现的问题：自测的负例
  # 都包在 `if (...)` 子 shell 条件里，errexit 在条件位置本就不生效，掩盖
  # 了这个赋值语句本身会被 errexit 拦下的问题。
  we2ai_matched="$(we2ai_strip_inline_test_mod "$f" | we2ai_grep_or_die "$f 品牌残留匹配" -inE "$brand_pattern")"
  hits="$(printf '%s\n' "$we2ai_matched" | we2ai_grep_or_die "$f 排除注释行" -vE '^[0-9]+:[[:space:]]*(//|/\*|\*)')"
  # 用进程替换而不是 herestring（`<<<`）喂给内层循环（Codex 验收 X4①）：
  # herestring 在 bash 里通过临时文件实现，沙箱环境里 `/tmp` 不可写会导致
  # 创建失败；进程替换走匿名管道/`/dev/fd`，不依赖磁盘临时文件，且同样不会
  # 把内层循环放进子 shell（不像结尾用 `|` 接管道那样会丢失 `we2ai_brand_leak`
  # 的累积）。
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    if ! is_whitelisted_process_detection_literal "$f" "$hit" \
      && ! is_whitelisted_brand_residue_matcher_literal "$f" "$hit"; then
      we2ai_brand_leak+="$f:$hit"$'\n'
    fi
  done < <(printf '%s\n' "$hits")
done < <(printf '%s\n' "$we2ai_rs_files")

we2ai_frontend_files="$(find src/we2ai -type f \( -name '*.ts' -o -name '*.tsx' -o -name '*.css' \))" || we2ai_find_status=$?
we2ai_check_find_status "${we2ai_find_status:-0}" "src/we2ai 下的前端文件枚举"
unset we2ai_find_status
while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  we2ai_matched="$(we2ai_grep_or_die "$f 品牌残留匹配" -inE "$brand_pattern" "$f")"
  hits="$(printf '%s\n' "$we2ai_matched" | we2ai_grep_or_die "$f 排除注释行" -vE '^[0-9]+:[[:space:]]*(//|/\*|\*)')"
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    we2ai_brand_leak+="$f:$hit"$'\n'
  done < <(printf '%s\n' "$hits")
done < <(printf '%s\n' "$we2ai_frontend_files")

if [[ -n "$we2ai_brand_leak" ]]; then
  err "WE2AI 用户界面文案残留 CC Switch 字样（N1，非注释/非测试代码；进程检测标识除外）：
${we2ai_brand_leak}"
fi

# 4.12 功能 18（B1 定价扩展）+ We2aiShell 更新检查修复：Opus 复核两处只需
# 机械字面量检查就能防回归的点。不重新实现完整语义（那部分交给
# `cargo test --locked we2ai::keys --lib` 与 `tests/integration/We2aiShellUpdateCheck.test.tsx`），
# 这里只确认关键常量/判断没有被静默删掉。
keys_rs=src-tauri/src/we2ai/keys.rs
if ! grep -q '"usd_per_1m_tokens"' "$keys_rs"; then
  err "$keys_rs: 找不到 \"usd_per_1m_tokens\" 字面量，pricing.unit 校验（功能 18，Opus 复核 P1）可能已被移除"
fi
we2ai_shell_tsx=src/we2ai/We2aiShell.tsx
if ! grep -q 'update\.error' "$we2ai_shell_tsx"; then
  err "$we2ai_shell_tsx: 找不到 update.error 判断，检查更新失败状态展示（Opus 复核 P9）可能已回归"
fi

# 4.13 功能 19（公告同步与系统通知）：机械字面量检查，确认注册点、依赖与净化配置
# 没有被上游同步静默还原。语义行为由 `cargo test --lib we2ai::announcements` 与
# `tests/we2ai/announcementMarkdown.test.tsx`、`tests/integration/AnnouncementCenter.test.tsx` 覆盖。
lib_rs=src-tauri/src/lib.rs
for needle in \
  'we2ai::announcements::we2ai_list_announcements' \
  'we2ai::announcements::we2ai_mark_announcement_read' \
  'we2ai::announcements::start_background_poller' \
  'tauri_plugin_notification::init()'; do
  if ! grep -qF "$needle" "$lib_rs"; then
    err "$lib_rs: 找不到 ${needle}（功能 19：公告命令/后台轮询/通知插件注册）"
  fi
done
if ! grep -qE '^tauri-plugin-notification = "=2\.4\.0"' src-tauri/Cargo.toml; then
  err "src-tauri/Cargo.toml: 缺少固定版本 tauri-plugin-notification = \"=2.4.0\"（功能 19；新版会牵动要求 rustc >= 1.89 的依赖）"
fi
notify_rust_version="$(awk '/^name = "notify-rust"$/{getline; print; exit}' src-tauri/Cargo.lock)"
if [[ "$notify_rust_version" != 'version = "4.11.7"' ]]; then
  err "src-tauri/Cargo.lock: notify-rust 应锁定 4.11.7（4.18+ 要求 rustc >= 1.89），实际：${notify_rust_version:-未找到}"
fi
for dep in marked dompurify; do
  if ! grep -qE "^    \"${dep}\":" package.json; then
    err "package.json: 缺少依赖 ${dep}（功能 19：公告 Markdown 渲染与净化）"
  fi
done
if ! grep -q 'AnnouncementCenter' "$we2ai_shell_tsx"; then
  err "$we2ai_shell_tsx: 找不到 AnnouncementCenter，顶栏公告铃铛（功能 19）可能已被移除"
fi
announcement_md=src/we2ai/announcementMarkdown.ts
# 净化配置必须逐字保持：标签白名单引用、仅 href 属性、URI 正则实际取值仅 http(s)。
for needle in \
  'ALLOWED_TAGS: [...ANNOUNCEMENT_ALLOWED_TAGS]' \
  'ALLOWED_ATTR: ["href"]' \
  'ALLOWED_URI_REGEXP: HTTP_URL' \
  'const HTTP_URL = /^https?:\/\//i;'; do
  if ! grep -qF "$needle" "$announcement_md"; then
    err "$announcement_md: 找不到 ${needle}，公告净化白名单（功能 19）可能已被放宽或移除"
  fi
done
# 标签白名单的实际内容必须恰好是约定的 16 个文本排版标签。
announcement_tags="$(awk '/ANNOUNCEMENT_ALLOWED_TAGS: readonly string\[\] = \[/{p=1;next} p&&/^\];/{exit} p{gsub(/[ ",]/,"");print}' "$announcement_md" | sort | tr '\n' ' ')"
announcement_tags_expected="a blockquote br code em h1 h2 h3 h4 hr li ol p pre strong ul "
if [[ "$announcement_tags" != "$announcement_tags_expected" ]]; then
  err "$announcement_md: 标签白名单被改动（功能 19），期望 [${announcement_tags_expected}]，实际 [${announcement_tags}]"
fi
# 链接点击拦截同样是安全边界：所有点击/中键/链接右键都不能让 WebView 导航，
# 只有 http(s) 才交给系统浏览器。
announcement_dialog=src/we2ai/AnnouncementDialog.tsx
for needle in \
  'event.preventDefault();' \
  'isOpenableAnnouncementUrl(href)' \
  'onAuxClick=' \
  'onContextMenu='; do
  if ! grep -qF "$needle" "$announcement_dialog"; then
    err "$announcement_dialog: 找不到 ${needle}，公告链接点击拦截（功能 19）可能已被移除"
  fi
done
# 刷新事件名是 Rust/TS 两侧各写一份的字面量，任一侧改动都会让前端静默收不到事件。
rust_event="$(sed -n 's/.*EVENT_CHANGED: &str = "\([^"]*\)".*/\1/p' src-tauri/src/we2ai/announcements.rs)"
ts_event="$(sed -n 's/.*WE2AI_ANNOUNCEMENTS_CHANGED_EVENT = "\([^"]*\)".*/\1/p' src/we2ai/api.ts)"
if [[ -z "$rust_event" || "$rust_event" != "$ts_event" ]]; then
  err "公告刷新事件名两侧不一致（功能 19）：announcements.rs='${rust_event}'，api.ts='${ts_event}'"
fi
if ! grep -q 'NOTIFIED_MAX: usize = 200' src-tauri/src/we2ai/announcements.rs; then
  err "src-tauri/src/we2ai/announcements.rs: 已通知 id 上限 NOTIFIED_MAX 不是 200（功能 19）"
fi

# 4.14 功能 20（充值入口与余额）：机械字面量检查。客户端不实现支付，充值统一跳 Web 充值页；
# 语义行为由 `cargo test --lib we2ai::billing`、`tests/we2ai/{useBalanceWatch,BillingPage}.test.tsx`、
# `tests/integration/We2aiShellBilling.test.tsx` 覆盖。
for needle in \
  'we2ai::billing::we2ai_get_balance' \
  'we2ai::billing::we2ai_gateway_info'; do
  if ! grep -qF "$needle" "$lib_rs"; then
    err "$lib_rs: 找不到 ${needle}（功能 20：余额与网关地址命令注册）"
  fi
done
billing_page=src/we2ai/BillingPage.tsx
if ! grep -qF '"/purchase"' "$billing_page"; then
  err "$billing_page: 找不到 \"/purchase\"，充值页跳转（功能 20）可能已被改动"
fi
if grep -nE 'https?://' "$billing_page" | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
  err "$billing_page: 出现写死的 http(s) 地址（功能 20：充值页地址必须来自 we2ai_gateway_info 的 base_url，不得硬编码域名）"
fi
# openExternal 参数必须是 base_url 拼接，不能是别的来源。
if ! grep -qF 'openExternal(`${base}${path}`)' "$billing_page"; then
  err "$billing_page: 找不到 openExternal(\`\${base}\${path}\`)，充值页地址必须由 gateway base_url 拼接（功能 20）"
fi
# 不带协议的硬编码域名同样不允许（注释行除外）。
if grep -nE 'we2ai\.com|wtgo\.com\.cn' "$billing_page" | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
  err "$billing_page: 出现写死的域名（api.we2ai.com / api.wtgo.com.cn 等，功能 20：地址必须来自 we2ai_gateway_info 的 base_url）"
fi
if ! grep -q 'BillingPage' "$we2ai_shell_tsx"; then
  err "$we2ai_shell_tsx: 找不到 BillingPage，充值 Tab（功能 20）可能已被移除"
fi
# 充值不引入支付 SDK / 二维码库：支付只在浏览器里的 Web 页完成。
if grep -nE '^[[:space:]]*"(@stripe/[^"]*|qrcode[^"]*)":' package.json; then
  err "package.json: 出现支付 SDK 或二维码依赖（@stripe/*、qrcode*），功能 20 要求客户端不接触支付"
fi

# 4.15 功能 21（Key 管理）：机械字面量检查。语义行为由 `cargo test --lib we2ai::key_manage`、
# `tests/we2ai/{KeyManagePage,KeyEditDialog}.test.tsx`、`tests/integration/We2aiShellKeys.test.tsx` 覆盖。
# ① 6 个命令在 lib.rs 注册；② 返回前端的 KeyManageView（Rust）与 We2aiManagedKey（TS）
# 字段允许清单锁定，且不含 key/明文字段——明文只经创建结果这一条路径出 Rust（复制由 we2ai_copy_key 在 Rust 侧写剪贴板，明文不经 IPC）；
# ③ keys-changed 事件名 Rust/TS 两侧一致。
for needle in \
  'we2ai::key_manage::we2ai_manage_list_keys' \
  'we2ai::key_manage::we2ai_list_key_groups' \
  'we2ai::key_manage::we2ai_create_key' \
  'we2ai::key_manage::we2ai_update_key' \
  'we2ai::key_manage::we2ai_delete_key' \
  'we2ai::key_manage::we2ai_copy_key'; do
  if ! grep -qF "$needle" "$lib_rs"; then
    err "$lib_rs: 找不到 ${needle}（功能 21：Key 管理命令注册）"
  fi
done
key_manage_rs=src-tauri/src/we2ai/key_manage.rs
expected_manage_rs_fields="expires_at group id last_used_at masked_key name quota quota_used status"
expected_manage_ts_fields="expiresAt group id lastUsedAt maskedKey name quota quotaUsed status"
if [[ ! -f "$key_manage_rs" ]]; then
  err "$key_manage_rs 不存在（功能 21）"
else
  manage_body="$(awk '/^pub struct KeyManageView \{/{f=1;next} f&&/^\}/{exit} f' "$key_manage_rs")"
  if [[ -z "$manage_body" ]]; then
    err "$key_manage_rs: 找不到 pub struct KeyManageView，无法校验明文 Key 不出 IPC（功能 21）"
  else
    # 匹配任意 `ident:` 字段行（含无 pub / pub(crate) / pub(super) 的字段），不只 `pub ident:`，
    # 避免用私有字段或受限可见性绕过清单。
    manage_rs_fields="$(printf '%s\n' "$manage_body" | sed -nE 's/^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?([a-z_0-9]+)[[:space:]]*:.*/\3/p' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
    if [[ "$manage_rs_fields" != "$expected_manage_rs_fields" ]]; then
      err "$key_manage_rs: KeyManageView 字段与允许清单不一致（实际：${manage_rs_fields}；允许：${expected_manage_rs_fields}；不得出现 key/secret/plaintext 等明文字段）"
    fi
    if printf '%s\n' "$manage_body" | grep -q 'serde('; then
      err "$key_manage_rs: KeyManageView 字段上不允许 serde 属性（重命名会绕过字段清单）"
    fi
  fi
fi
ts_manage="$(awk '/^export interface We2aiManagedKey \{/{f=1;next} f&&/^\}/{exit} f' src/we2ai/api.ts)"
if [[ -z "$ts_manage" ]]; then
  err "src/we2ai/api.ts: 找不到 We2aiManagedKey 接口（功能 21）"
else
  ts_manage_fields="$(printf '%s\n' "$ts_manage" | sed -nE 's/^[[:space:]]*([A-Za-z_0-9]+)[?]?[[:space:]]*:.*/\1/p' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
  if [[ "$ts_manage_fields" != "$expected_manage_ts_fields" ]]; then
    err "src/we2ai/api.ts: We2aiManagedKey 字段与允许清单不一致（实际：${ts_manage_fields}；允许：${expected_manage_ts_fields}）"
  fi
fi
# Key 变更刷新事件名是 Rust/TS 两侧各写一份的字面量，任一侧改动都会让前端静默收不到事件。
rust_keys_event="$(sed -n 's/.*EVENT_KEYS_CHANGED: &str = "\([^"]*\)".*/\1/p' "$key_manage_rs" 2>/dev/null)"
ts_keys_event="$(sed -n 's/.*WE2AI_KEYS_CHANGED_EVENT = "\([^"]*\)".*/\1/p' src/we2ai/api.ts)"
if [[ -z "$rust_keys_event" || "$rust_keys_event" != "$ts_keys_event" ]]; then
  err "Key 变更事件名两侧不一致（功能 21）：key_manage.rs='${rust_keys_event}'，api.ts='${ts_keys_event}'"
fi
# we2ai_copy_key 必须返回 ()：返回值类型一变（如 String）就等于把明文交给了前端。
copy_sig="$(awk '/pub async fn we2ai_copy_key\(/{f=1} f{print} f&&/\{[[:space:]]*$/{exit}' "$key_manage_rs" 2>/dev/null)"
if ! printf '%s\n' "$copy_sig" | grep -qE '\)[[:space:]]*->[[:space:]]*Result<\(\),[[:space:]]*We2aiApiError>'; then
  err "$key_manage_rs: we2ai_copy_key 的返回类型不是 Result<(), We2aiApiError>（功能 21：明文不得经 IPC 返回前端）"
fi
# 复制必须由 Rust 写剪贴板：不得恢复 reveal 命令，页面不得用 copyText / navigator.clipboard / revealKey。
if grep -qF 'we2ai_reveal_key' "$lib_rs" "$key_manage_rs"; then
  err "出现 we2ai_reveal_key（功能 21：明文不得经 IPC 返回前端，复制走 we2ai_copy_key；P3 若需要要重新评审）"
fi
if ! grep -qF 'invoke("we2ai_copy_key"' src/we2ai/api.ts; then
  err "src/we2ai/api.ts: 找不到 invoke(\"we2ai_copy_key\")（功能 21）"
fi
for f in src/we2ai/KeyManagePage.tsx src/we2ai/KeyEditDialog.tsx; do
  if grep -nE 'copyText|navigator\.clipboard|revealKey|lib/clipboard' "$f" | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
    err "$f: 出现 copyText/navigator.clipboard/revealKey（功能 21：复制必须走 we2aiApi.copyKey，由 Rust 写剪贴板）"
  fi
done
# 删除确认必须是应用内弹窗，不能退回系统 confirm。
if grep -nE '(window\.)?confirm\(' src/we2ai/KeyManagePage.tsx | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
  err "src/we2ai/KeyManagePage.tsx: 出现 confirm(...)（功能 21：删除确认须为应用内弹窗）"
fi

# 4.16 功能 22（调用示例）：机械字面量检查。语义行为由 `cargo test --lib we2ai::key_manage`、
# `tests/we2ai/{codeSamples,CodeSampleDrawer,copyCommands,KeyManagePage}.test.*` 覆盖。
# ① 两个命令（we2ai_copy_text 非敏感文本、we2ai_copy_text_with_key 带占位串的 Key 替换）在 lib.rs 注册；
# ② 两者都返回 ()（返回值一变就等于把明文交给了前端）；
# ③ 占位串字面量 Rust（key_manage.rs::SAMPLE_KEY_PLACEHOLDER）与 TS（codeSamples.ts::KEY_PLACEHOLDER）一致，
#    we2ai_copy_text_with_key 不接收占位串参数；
# ④ 抽屉与模板文件不调用任何返回明文的命令（createKey 响应带一次性明文、reveal 类命令）、不碰
#    copyText / navigator.clipboard（WE2AI 模式下 copy_text_to_clipboard 被 IPC gate 拒绝，前端剪贴板又依赖用户手势，
#    非敏感文本也必须走 we2ai_copy_text），也不出现 plaintext / created.（创建结果明文）；
#    「填入真实 Key」只能走 copyTextWithKey（Rust 替换占位串后写剪贴板）。
for needle in \
  'we2ai::key_manage::we2ai_copy_text,' \
  'we2ai::key_manage::we2ai_copy_text_with_key,'; do
  if ! grep -qF "$needle" "$lib_rs"; then
    err "$lib_rs: 找不到 ${needle%,}（功能 22：调用示例复制命令注册）"
  fi
done
for cmd in we2ai_copy_text we2ai_copy_text_with_key; do
  cmd_sig="$(awk -v name="$cmd" 'index($0, "pub async fn " name "(") {f=1} f{print} f&&/\{[[:space:]]*$/{exit}' "$key_manage_rs" 2>/dev/null)"
  if ! printf '%s\n' "$cmd_sig" | grep -qE '\)[[:space:]]*->[[:space:]]*Result<\(\),[[:space:]]*We2aiApiError>'; then
    err "$key_manage_rs: ${cmd} 的返回类型不是 Result<(), We2aiApiError>（功能 22：明文不得经 IPC 返回前端）"
  fi
  if ! grep -qF "invoke(\"${cmd}\"" src/we2ai/api.ts; then
    err "src/we2ai/api.ts: 找不到 invoke(\"${cmd}\")（功能 22）"
  fi
done
if printf '%s\n' "$(awk '/pub async fn we2ai_copy_text_with_key\(/{f=1} f{print} f&&/\{[[:space:]]*$/{exit}' "$key_manage_rs" 2>/dev/null)" | grep -q 'placeholder'; then
  err "$key_manage_rs: we2ai_copy_text_with_key 不应接收占位串参数（功能 22：占位串只认 Rust 常量 SAMPLE_KEY_PLACEHOLDER）"
fi
rust_placeholder="$(sed -n 's/.*SAMPLE_KEY_PLACEHOLDER: &str = "\([^"]*\)".*/\1/p' "$key_manage_rs" 2>/dev/null)"
ts_placeholder="$(sed -n 's/.*KEY_PLACEHOLDER = "\([^"]*\)".*/\1/p' src/we2ai/codeSamples.ts 2>/dev/null)"
if [[ -z "$rust_placeholder" || "$rust_placeholder" != "$ts_placeholder" ]]; then
  err "Key 占位串两侧不一致（功能 22）：key_manage.rs='${rust_placeholder}'，codeSamples.ts='${ts_placeholder}'"
fi
for f in src/we2ai/CodeSampleDrawer.tsx src/we2ai/codeSamples.ts; do
  if [[ ! -f "$f" ]]; then
    err "$f 不存在（功能 22）"
    continue
  fi
  if grep -nE 'revealKey|we2ai_reveal|\.createKey\(|\.copyKey\(|we2ai_create_key|we2ai_copy_key|plaintext|created\.' "$f" | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
    err "$f: 出现返回明文的命令、revealKey、plaintext 或 created.（功能 22：示例不得拿到明文，「填入真实 Key」只能走 copyTextWithKey）"
  fi
  if grep -nE '(^|[^A-Za-z0-9_])copyText([^A-Za-z0-9_]|$)|navigator\.clipboard|lib/clipboard' "$f" | grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' >/dev/null; then
    err "$f: 出现 copyText / navigator.clipboard（功能 22：WE2AI 模式下前端剪贴板不可靠，非敏感文本走 we2aiApi.copyPlainText，Key 走 copyTextWithKey）"
  fi
done
for fn in copyTextWithKey copyPlainText; do
  if ! grep -qF "$fn" src/we2ai/CodeSampleDrawer.tsx 2>/dev/null; then
    err "src/we2ai/CodeSampleDrawer.tsx: 找不到 ${fn}（功能 22：复制必须由 Rust 写剪贴板）"
  fi
done

if [[ "$fail" == 0 ]]; then
  echo "we2ai guards: all passed (version=${expected})"
fi
exit "$fail"
