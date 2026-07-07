#!/usr/bin/env bash
set -euo pipefail

APP_NAME="CC Switch"
APP_BUNDLE="${APP_NAME}.app"
INSTALL_DIR="/Applications"
INSTALL_PATH="${INSTALL_DIR}/${APP_BUNDLE}"
RELEASE_REPO="${CC_SWITCH_RELEASE_REPO:-qinghua362330/cc-switch-company}"
RELEASE_TAG="${CC_SWITCH_RELEASE_TAG:-}"
METADATA_URL="${CC_SWITCH_METADATA_URL:-}"
DOWNLOAD_URL="${CC_SWITCH_DOWNLOAD_URL:-}"
EXPECTED_SHA256="${CC_SWITCH_SHA256:-}"
WORKDIR="$(mktemp -d)"
MOUNT_DIR=""

cleanup() {
  if [[ -n "${MOUNT_DIR}" && -d "${MOUNT_DIR}" ]]; then
    hdiutil detach "${MOUNT_DIR}" -quiet >/dev/null 2>&1 || true
  fi
  rm -rf "${WORKDIR}"
}
trap cleanup EXIT

say() {
  printf '[CC Switch] %s\n' "$1" >&2
}

fail() {
  printf '[CC Switch] 安装失败：%s\n' "$1" >&2
  exit 1
}

detect_arch() {
  local kernel
  kernel="$(uname -s 2>/dev/null || printf 'unknown')"
  [[ "$kernel" == "Darwin" ]] || fail "当前 install.sh 仅支持 macOS。Windows 请使用 install.ps1。"

  local machine="${CC_SWITCH_ARCH:-$(uname -m)}"
  case "$machine" in
    arm64|aarch64)
      printf 'aarch64\n'
      ;;
    x86_64|amd64|x64)
      printf 'x86_64\n'
      ;;
    *)
      fail "不支持的 Mac 芯片架构：${machine}"
      ;;
  esac
}

metadata_url() {
  if [[ -n "${METADATA_URL}" ]]; then
    printf '%s\n' "${METADATA_URL}"
  elif [[ -n "${RELEASE_TAG}" ]]; then
    printf 'https://github.com/%s/releases/download/%s/latest-company.json\n' "${RELEASE_REPO}" "${RELEASE_TAG}"
  else
    printf 'https://github.com/%s/releases/latest/download/latest-company.json\n' "${RELEASE_REPO}"
  fi
}

release_api_url() {
  if [[ -n "${RELEASE_TAG}" ]]; then
    printf 'https://api.github.com/repos/%s/releases/tags/%s\n' "${RELEASE_REPO}" "${RELEASE_TAG}"
  else
    printf 'https://api.github.com/repos/%s/releases/latest\n' "${RELEASE_REPO}"
  fi
}

json_get() {
  local file="$1"
  local path="$2"

  if command -v python3 >/dev/null 2>&1; then
    python3 - "$file" "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    value = json.load(fh)

for part in sys.argv[2].split("."):
    if not isinstance(value, dict) or part not in value:
        sys.exit(1)
    value = value[part]

if value is None:
    sys.exit(1)
print(value)
PY
    return
  fi

  if command -v ruby >/dev/null 2>&1; then
    ruby -rjson -e '
      value = JSON.parse(File.read(ARGV[0]))
      ARGV[1].split(".").each do |part|
        exit 1 unless value.is_a?(Hash) && value.key?(part)
        value = value[part]
      end
      exit 1 if value.nil?
      puts value
    ' "$file" "$path"
    return
  fi

  if [[ -x /usr/bin/plutil ]]; then
    /usr/bin/plutil -extract "$path" raw -o - "$file" 2>/dev/null
    return
  fi

  fail "系统缺少 python3/plutil，无法解析 GitHub 版本信息"
}

json_keys() {
  local file="$1"
  local path="$2"

  if command -v python3 >/dev/null 2>&1; then
    python3 - "$file" "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    value = json.load(fh)

for part in sys.argv[2].split("."):
    if not isinstance(value, dict) or part not in value:
        sys.exit(1)
    value = value[part]

if not isinstance(value, dict):
    sys.exit(1)
print(", ".join(sorted(value.keys())))
PY
    return
  fi

  if command -v ruby >/dev/null 2>&1; then
    ruby -rjson -e '
      value = JSON.parse(File.read(ARGV[0]))
      ARGV[1].split(".").each do |part|
        exit 1 unless value.is_a?(Hash) && value.key?(part)
        value = value[part]
      end
      exit 1 unless value.is_a?(Hash)
      puts value.keys.sort.join(", ")
    ' "$file" "$path"
  fi
}

try_metadata_package() {
  local metadata="$1"
  local key="$2"
  local candidate_url
  local candidate_sha

  candidate_url="$(json_get "$metadata" "installers.${key}.url" 2>/dev/null || true)"
  candidate_sha="$(json_get "$metadata" "installers.${key}.sha256" 2>/dev/null || true)"

  if [[ -z "$candidate_url" ]]; then
    candidate_url="$(json_get "$metadata" "platforms.${key}.url" 2>/dev/null || true)"
    candidate_sha=""
  fi

  if [[ -z "$candidate_url" ]]; then
    return 1
  fi

  DOWNLOAD_URL="${DOWNLOAD_URL:-$candidate_url}"
  EXPECTED_SHA256="${EXPECTED_SHA256:-$candidate_sha}"
  return 0
}

find_macos_asset_url() {
  local release_json="$1"

  if command -v python3 >/dev/null 2>&1; then
    python3 - "$release_json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    release = json.load(fh)

assets = release.get("assets") or []
patterns = [
    r"macOS-company-universal\.tar\.gz$",
    r"macOS-company-universal\.zip$",
    r"macOS.*universal.*\.tar\.gz$",
    r"macOS.*universal.*\.zip$",
]

for pattern in patterns:
    rx = re.compile(pattern, re.IGNORECASE)
    for asset in assets:
        name = asset.get("name") or ""
        url = asset.get("browser_download_url") or ""
        if url and rx.search(name):
            print(url)
            sys.exit(0)

sys.exit(1)
PY
    return
  fi

  if command -v ruby >/dev/null 2>&1; then
    ruby -rjson -e '
      release = JSON.parse(File.read(ARGV[0]))
      assets = release["assets"] || []
      patterns = [
        /macOS-company-universal\.tar\.gz$/i,
        /macOS-company-universal\.zip$/i,
        /macOS.*universal.*\.tar\.gz$/i,
        /macOS.*universal.*\.zip$/i,
      ]
      patterns.each do |pattern|
        assets.each do |asset|
          name = asset["name"] || ""
          url = asset["browser_download_url"] || ""
          if !url.empty? && name.match?(pattern)
            puts url
            exit 0
          end
        end
      end
      exit 1
    ' "$release_json"
  fi
}

resolve_from_release_assets() {
  local release_json="${WORKDIR}/release.json"
  local api
  local url

  api="$(release_api_url)"
  say "未在 latest-company.json 找到匹配架构，正在尝试读取 GitHub Release 资产：${api}"
  curl -fsSL --retry 3 --connect-timeout 20 \
    -H "Cache-Control: no-cache" \
    -H "Pragma: no-cache" \
    -o "$release_json" "${api}?cache_bust=$(date +%s)" \
    || return 1

  url="$(find_macos_asset_url "$release_json" 2>/dev/null || true)"
  [[ -n "$url" ]] || return 1

  DOWNLOAD_URL="${DOWNLOAD_URL:-$url}"
  EXPECTED_SHA256="${EXPECTED_SHA256:-}"
  say "已找到 macOS Universal 安装包。"
  return 0
}

resolve_from_metadata() {
  local arch="$1"
  local metadata="${WORKDIR}/latest-company.json"
  local source
  local key
  local -a candidates

  source="$(metadata_url)"
  say "正在读取 GitHub 版本信息：${source}"
  curl -fsSL --retry 3 --connect-timeout 20 \
    -H "Cache-Control: no-cache" \
    -H "Pragma: no-cache" \
    -o "$metadata" "${source}?cache_bust=$(date +%s)" \
    || fail "无法读取 GitHub Release 版本信息"

  candidates=("darwin-${arch}" "darwin-universal" "macos-universal" "macOS-universal")
  if [[ "$arch" == "x86_64" ]]; then
    candidates+=("darwin-aarch64")
  else
    candidates+=("darwin-x86_64")
  fi

  for key in "${candidates[@]}"; do
    if try_metadata_package "$metadata" "$key"; then
      if [[ "$key" != "darwin-${arch}" ]]; then
        say "没有找到 darwin-${arch} 独立条目，已使用 ${key} 安装包。"
      fi
      return
    fi
  done

  if resolve_from_release_assets; then
    return
  fi

  local installer_keys
  local platform_keys
  installer_keys="$(json_keys "$metadata" "installers" 2>/dev/null || true)"
  platform_keys="$(json_keys "$metadata" "platforms" 2>/dev/null || true)"
  fail "latest-company.json 中没有 darwin-${arch} 安装包。installers 可用项：${installer_keys:-无}; platforms 可用项：${platform_keys:-无}"
}

copy_app() {
  local app_path="$1"

  [[ -d "$app_path" ]] || fail "没有找到 ${APP_BUNDLE}"

  say "正在退出已打开的 ${APP_NAME}..."
  osascript -e "tell application \"${APP_NAME}\" to quit" >/dev/null 2>&1 || true
  sleep 1

  say "正在安装到 ${INSTALL_PATH}..."
  if [[ -w "${INSTALL_DIR}" ]]; then
    rm -rf "${INSTALL_PATH}"
    ditto "$app_path" "${INSTALL_PATH}"
    xattr -dr com.apple.quarantine "${INSTALL_PATH}" >/dev/null 2>&1 || true
  else
    sudo rm -rf "${INSTALL_PATH}"
    sudo ditto "$app_path" "${INSTALL_PATH}"
    sudo xattr -dr com.apple.quarantine "${INSTALL_PATH}" >/dev/null 2>&1 || true
  fi

  say "安装完成，正在启动 ${APP_NAME}..."
  open "${INSTALL_PATH}"
}

verify_checksum() {
  local payload="$1"

  if [[ -z "${EXPECTED_SHA256}" ]]; then
    say "未提供 SHA256，跳过哈希校验。"
    return
  fi

  local actual
  actual="$(shasum -a 256 "$payload" | awk '{print $1}')"
  [[ "$actual" == "${EXPECTED_SHA256}" ]] || fail "校验失败，下载文件可能不完整"
}

download_payload() {
  local filename
  filename="$(basename "${DOWNLOAD_URL%%\?*}")"
  [[ -n "$filename" && "$filename" != "/" ]] || filename="cc-switch-macos-download"

  local output="${WORKDIR}/${filename}"
  say "正在下载 ${DOWNLOAD_URL}..."
  curl -fL --retry 3 --connect-timeout 20 -o "$output" "${DOWNLOAD_URL}"
  verify_checksum "$output"
  printf '%s\n' "$output"
}

install_from_archive() {
  local payload="$1"
  say "正在解压 ${payload}..."

  case "$payload" in
    *.zip)
      ditto -x -k "$payload" "${WORKDIR}"
      ;;
    *.tar.gz|*.tgz)
      tar -xzf "$payload" -C "${WORKDIR}"
      ;;
    *)
      fail "不支持的压缩包格式：$payload"
      ;;
  esac

  local app_path
  app_path="$(find "${WORKDIR}" -maxdepth 4 -type d -name "${APP_BUNDLE}" -print -quit)"
  copy_app "$app_path"
}

install_from_dmg() {
  local payload="$1"
  MOUNT_DIR="${WORKDIR}/mount"
  mkdir -p "${MOUNT_DIR}"
  say "正在挂载 ${payload}..."
  hdiutil attach "$payload" -nobrowse -quiet -mountpoint "${MOUNT_DIR}"

  local app_path
  app_path="$(find "${MOUNT_DIR}" -maxdepth 2 -type d -name "${APP_BUNDLE}" -print -quit)"
  copy_app "$app_path"
}

main() {
  local arch
  local payload
  arch="$(detect_arch)"
  say "检测到 Mac 架构：${arch}"

  if [[ -z "${DOWNLOAD_URL}" ]]; then
    resolve_from_metadata "$arch"
  fi

  if [[ "${CC_SWITCH_DRY_RUN:-0}" == "1" ]]; then
    say "下载地址：${DOWNLOAD_URL}"
    say "SHA256：${EXPECTED_SHA256:-未提供}"
    exit 0
  fi

  payload="$(download_payload)"

  case "$payload" in
    *.zip|*.tar.gz|*.tgz)
      install_from_archive "$payload"
      ;;
    *.dmg)
      install_from_dmg "$payload"
      ;;
    *)
      fail "不支持的安装包格式：$payload"
      ;;
  esac

  say "好了，可以开始使用 ${APP_NAME}。"
}

main "$@"
