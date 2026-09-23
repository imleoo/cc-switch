#!/usr/bin/env bash
# we2ai fork 共享函数：版本号读写。被 sync-upstream.sh / check-guards.sh source。
# 版本规则：we2ai 主号 = 上游主号 + 1，次/修订号跟随上游（3.20.4 → 4.20.4）。

WE2AI_MAJOR_OFFSET="${WE2AI_MAJOR_OFFSET:-1}"

# 版本号载体（4 处必须一致）
WE2AI_VERSION_FILES=(
  package.json
  src-tauri/tauri.conf.json
  src-tauri/Cargo.toml
  src-tauri/Cargo.lock
)

# read_version <file>：从文件内容（stdin）读出本应用版本号
read_version() {
  local file="$1"
  case "$file" in
    *.json)       perl -0ne 'print $1 if /"version"\s*:\s*"([^"]+)"/' ;;
    *Cargo.toml)  perl -0ne 'print $1 if /^version\s*=\s*"([^"]+)"/m' ;;
    *Cargo.lock)  perl -0ne 'print $1 if /^name = "cc-switch"\nversion = "([^"]+)"/m' ;;
    *) echo "read_version: unsupported file $file" >&2; return 1 ;;
  esac
}

# rewrite_version <file> <new_version>：原地改写文件中的本应用版本号（仅首个匹配）
rewrite_version() {
  local file="$1" new="$2"
  case "$file" in
    *.json)       NEW="$new" perl -0pi -e 's/("version"\s*:\s*")[^"]+(")/$1$ENV{NEW}$2/' "$file" ;;
    *Cargo.toml)  NEW="$new" perl -0pi -e 's/^(version\s*=\s*")[^"]+(")/$1$ENV{NEW}$2/m' "$file" ;;
    *Cargo.lock)  NEW="$new" perl -0pi -e 's/^(name = "cc-switch"\nversion = ")[^"]+(")/$1$ENV{NEW}$2/m' "$file" ;;
    *) echo "rewrite_version: unsupported file $file" >&2; return 1 ;;
  esac
}

# fork_version <upstream_version>：3.20.4 → 4.20.4
fork_version() {
  echo "$1" | awk -F. -v off="$WE2AI_MAJOR_OFFSET" 'BEGIN{OFS="."} {$1=$1+off; print}'
}

# ref_version <git-ref>：读取某个 ref 上 package.json 的版本
ref_version() {
  git show "$1:package.json" | read_version package.json
}
