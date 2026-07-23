#!/usr/bin/env bash
set -euo pipefail

MODE="diagnose"
TARGET_PROVIDER="custom"
SOURCE_PROVIDERS="openai,hellotalk,ccswitch"
CODEX_DIR="${CODEX_HOME:-$HOME/.codex}"
CC_SWITCH_DIR="${CC_SWITCH_HOME:-$HOME/.cc-switch}"

usage() {
  cat <<'EOF'
Usage:
  scripts/diagnose-codex-history-buckets.sh
  scripts/diagnose-codex-history-buckets.sh --repair [--target custom] [--sources openai,hellotalk,ccswitch]

Default mode is read-only. --repair rewrites Codex history metadata to the
target provider after creating backups under ~/.cc-switch/backups/.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repair)
      MODE="repair"
      shift
      ;;
    --target)
      TARGET_PROVIDER="${2:-}"
      [[ -n "$TARGET_PROVIDER" ]] || { echo "--target requires a value" >&2; exit 2; }
      shift 2
      ;;
    --sources)
      SOURCE_PROVIDERS="${2:-}"
      [[ -n "$SOURCE_PROVIDERS" ]] || { echo "--sources requires a value" >&2; exit 2; }
      shift 2
      ;;
    --codex-dir)
      CODEX_DIR="${2:-}"
      [[ -n "$CODEX_DIR" ]] || { echo "--codex-dir requires a value" >&2; exit 2; }
      shift 2
      ;;
    --cc-switch-dir)
      CC_SWITCH_DIR="${2:-}"
      [[ -n "$CC_SWITCH_DIR" ]] || { echo "--cc-switch-dir requires a value" >&2; exit 2; }
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

command -v python3 >/dev/null 2>&1 || {
  echo "python3 is required for this diagnostic script." >&2
  exit 1
}

python3 - "$MODE" "$CODEX_DIR" "$CC_SWITCH_DIR" "$TARGET_PROVIDER" "$SOURCE_PROVIDERS" <<'PY'
import collections
import datetime as dt
import json
import os
import pathlib
import re
import shutil
import sqlite3
import sys
import tempfile

mode, codex_dir_arg, cc_switch_dir_arg, target, sources_arg = sys.argv[1:6]
codex_dir = pathlib.Path(codex_dir_arg).expanduser()
cc_switch_dir = pathlib.Path(cc_switch_dir_arg).expanduser()
sources = {item.strip() for item in sources_arg.split(",") if item.strip()}
sources.discard(target)

def section(title):
    print(f"\n== {title} ==")

def read_text(path):
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""
    except Exception as exc:
        return f"<read error: {exc}>"

def toml_model_provider(text):
    match = re.search(r'(?m)^\s*model_provider\s*=\s*"([^"]+)"', text)
    return match.group(1) if match else "<implicit-openai>"

def config_provider_tables(text):
    return re.findall(r'(?m)^\s*\[model_providers\.(?:"([^"]+)"|([A-Za-z0-9_-]+))\]\s*$', text)

def table_names(matches):
    return sorted({a or b for a, b in matches if (a or b)})

def iter_jsonl_files():
    for base_name in ("sessions", "archived_sessions"):
        base = codex_dir / base_name
        if base.exists():
            yield from base.rglob("rollout-*.jsonl")

def scan_jsonl():
    counts = collections.Counter()
    examples = {}
    errors = []
    files = []
    for path in iter_jsonl_files():
        file_providers = set()
        try:
            with path.open(encoding="utf-8") as handle:
                for line in handle:
                    if '"session_meta"' not in line or '"model_provider"' not in line:
                        continue
                    record = json.loads(line)
                    if record.get("type") != "session_meta":
                        continue
                    provider = (record.get("payload") or {}).get("model_provider") or "<missing>"
                    counts[provider] += 1
                    examples.setdefault(provider, str(path))
                    file_providers.add(provider)
        except Exception as exc:
            errors.append((str(path), str(exc)))
        if file_providers:
            files.append((path, file_providers))
    return counts, examples, errors, files

def state_db_paths():
    candidates = [
        codex_dir / "sqlite" / "state_5.sqlite",
        codex_dir / "state_5.sqlite",
    ]
    return [path for path in candidates if path.exists()]

def scan_state_dbs():
    results = {}
    for path in state_db_paths():
        try:
            conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=5)
            try:
                rows = conn.execute(
                    "SELECT COALESCE(model_provider, '<null>'), COUNT(*) "
                    "FROM threads GROUP BY COALESCE(model_provider, '<null>') "
                    "ORDER BY COUNT(*) DESC"
                ).fetchall()
            finally:
                conn.close()
            results[str(path)] = rows
        except Exception as exc:
            results[str(path)] = [("<error>", str(exc))]
    return results

def scan_cc_switch_db():
    db_path = cc_switch_dir / "cc-switch.db"
    if not db_path.exists():
        return []
    rows = []
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=5)
        try:
            for provider_id, name, category, is_current, config in conn.execute(
                "SELECT id, name, category, is_current, json_extract(settings_config,'$.config') "
                "FROM providers WHERE app_type='codex' ORDER BY is_current DESC, name"
            ):
                rows.append({
                    "id": provider_id,
                    "name": name,
                    "category": category,
                    "is_current": bool(is_current),
                    "model_provider": toml_model_provider(config or ""),
                    "tables": table_names(config_provider_tables(config or "")),
                })
        finally:
            conn.close()
    except Exception as exc:
        rows.append({"error": str(exc)})
    return rows

def backup_path_for(backup_root, path):
    try:
        rel = path.resolve().relative_to(codex_dir.resolve())
    except Exception:
        rel = pathlib.Path("external") / path.name
    return backup_root / rel

def rewrite_jsonl_files(backup_root):
    changed_files = 0
    changed_lines = 0
    for path in iter_jsonl_files():
        try:
            original = path.read_text(encoding="utf-8")
        except Exception:
            continue
        changed = False
        next_lines = []
        for segment in original.splitlines(keepends=True):
            line = segment[:-1] if segment.endswith("\n") else segment
            newline = "\n" if segment.endswith("\n") else ""
            next_line = line
            if '"session_meta"' in line and '"model_provider"' in line:
                try:
                    record = json.loads(line)
                    payload = record.get("payload")
                    if record.get("type") == "session_meta" and isinstance(payload, dict):
                        if payload.get("model_provider") in sources:
                            payload["model_provider"] = target
                            next_line = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
                            changed = True
                            changed_lines += 1
                except Exception:
                    pass
            next_lines.append(next_line + newline)
        if changed:
            dst = backup_path_for(backup_root / "jsonl", path)
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, dst)
            fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=str(path.parent))
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write("".join(next_lines))
            shutil.copymode(path, tmp_name)
            os.replace(tmp_name, path)
            changed_files += 1
    return changed_files, changed_lines

def rewrite_state_dbs(backup_root):
    changed_rows = 0
    for path in state_db_paths():
        try:
            conn = sqlite3.connect(str(path), timeout=5)
            conn.execute("PRAGMA busy_timeout = 5000")
            placeholders = ",".join("?" for _ in sources)
            if not placeholders:
                conn.close()
                continue
            count_sql = f"SELECT COUNT(*) FROM threads WHERE model_provider IN ({placeholders})"
            count = conn.execute(count_sql, tuple(sources)).fetchone()[0]
            if count <= 0:
                conn.close()
                continue
            backup = backup_root / "state" / path.name
            backup.parent.mkdir(parents=True, exist_ok=True)
            src = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=5)
            dst = sqlite3.connect(str(backup), timeout=5)
            try:
                src.backup(dst)
            finally:
                dst.close()
                src.close()
            update_sql = f"UPDATE threads SET model_provider = ? WHERE model_provider IN ({placeholders})"
            cursor = conn.execute(update_sql, (target, *tuple(sources)))
            conn.commit()
            changed_rows += cursor.rowcount or 0
            conn.close()
        except Exception as exc:
            print(f"state db repair skipped for {path}: {exc}")
    return changed_rows

config_text = read_text(codex_dir / "config.toml")
live_provider = toml_model_provider(config_text)
live_tables = table_names(config_provider_tables(config_text))
settings_text = read_text(cc_switch_dir / "settings.json")

section("Codex live config")
print(f"codex_dir: {codex_dir}")
print(f"live model_provider: {live_provider}")
print(f"defined provider tables: {', '.join(live_tables) if live_tables else '<none>'}")

section("CC Switch settings")
if settings_text and not settings_text.startswith("<read error"):
    try:
        settings = json.loads(settings_text)
        print(f"unifyCodexSessionHistory: {settings.get('unifyCodexSessionHistory')}")
        print(f"unifyCodexMigrateExisting: {settings.get('unifyCodexMigrateExisting')}")
        print(f"currentProviderCodex: {settings.get('currentProviderCodex')}")
        migrations = settings.get("localMigrations") or {}
        print(f"codexOfficialHistoryUnifyV1: {migrations.get('codexOfficialHistoryUnifyV1')}")
    except Exception as exc:
        print(f"settings parse error: {exc}")
else:
    print("settings.json not found")

section("CC Switch Codex providers")
for row in scan_cc_switch_db():
    if "error" in row:
        print(f"db error: {row['error']}")
    else:
        current = "current" if row["is_current"] else "stored"
        print(f"{current}: {row['name']} [{row['category']}] model_provider={row['model_provider']} tables={row['tables']}")

jsonl_counts, examples, errors, _files = scan_jsonl()
section("Codex JSONL history buckets")
if jsonl_counts:
    for provider, count in jsonl_counts.most_common():
        print(f"{provider}: {count} example={examples.get(provider, '')}")
else:
    print("<none>")
if errors:
    print(f"errors: {len(errors)}")

section("Codex SQLite history buckets")
state_results = scan_state_dbs()
if state_results:
    for path, rows in state_results.items():
        print(path)
        for provider, count in rows:
            print(f"  {provider}: {count}")
else:
    print("<none>")

problem_sources = {provider for provider in jsonl_counts if provider in sources}
for rows in state_results.values():
    for provider, _count in rows:
        if provider in sources:
            problem_sources.add(provider)

section("Diagnosis")
if live_provider == target and problem_sources:
    print(f"LIKELY ISSUE: live provider is {target}, but history still exists in {sorted(problem_sources)}.")
    print(f"Run with --repair to move those buckets to {target}.")
elif live_provider == target:
    print(f"OK: live provider is {target} and known old buckets are not present.")
else:
    print(f"NOTE: live provider is {live_provider}. If CCS expects {target}, switch/enable the CCS Codex card first.")

if mode == "repair":
    section("Repair")
    backup_root = cc_switch_dir / "backups" / ("codex-history-bucket-repair-" + dt.datetime.now().strftime("%Y%m%d-%H%M%S"))
    backup_root.mkdir(parents=True, exist_ok=True)
    changed_files, changed_lines = rewrite_jsonl_files(backup_root)
    changed_rows = rewrite_state_dbs(backup_root)
    print(f"target_provider: {target}")
    print(f"sources: {sorted(sources)}")
    print(f"jsonl_files_changed: {changed_files}")
    print(f"jsonl_lines_changed: {changed_lines}")
    print(f"sqlite_rows_changed: {changed_rows}")
    print(f"backup_dir: {backup_root}")
PY
