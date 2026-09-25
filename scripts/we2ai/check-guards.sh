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
dialog:default"
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

if [[ "$fail" == 0 ]]; then
  echo "we2ai guards: all passed (version=${expected})"
fi
exit "$fail"
