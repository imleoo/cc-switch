#!/usr/bin/env bash
# we2ai 共享函数：版本号读取。被 check-guards.sh source。

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
