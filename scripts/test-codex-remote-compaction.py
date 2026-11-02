#!/usr/bin/env python3
"""Regression-test generated synthetic TOML with the desktop Codex engine.

Python 3.11+; macOS sandbox-exec is required for engine execution (fail closed
elsewhere). Zstd-encoded requests require Python's stdlib compression.zstd.
No dependencies, real credentials, model services, or desktop UI are used.
The input TOML is never written; only a temporary copy has its selected base_url
and optional synthetic bearer replaced. WS/storage/capabilities are preserved.
"""

import argparse
import copy
from contextlib import ExitStack
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import queue
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


FIRST = "CAPTURE_FIRST_OLD_USER"
SECOND = "CAPTURE_SECOND_NEW_USER"
OPAQUE = "capture-synthetic-opaque-compaction"
ASSISTANT = "CAPTURE_FIRST_ASSISTANT"
SUMMARY = "CAPTURE_LOCAL_SUMMARY"
TOOL_NAME = "capture_echo"
TOOL_CALL = "capture_echo_call"
TOOL_OUTPUT = "CAPTURE_TOOL_OUTPUT"
DUMMY_KEY = "sk-test-key"
MAX_BODY = 8 * 1024 * 1024


def fixture_request_path(target, host, port):
    authority = f"127.0.0.1:{port}"
    if host != authority or any(char in target for char in ("?", "#", "\\", "\r", "\n", "\t")):
        raise ValueError("non-fixture request target")
    parsed = urlsplit(target)
    if parsed.scheme or parsed.netloc:
        if parsed.scheme != "http" or parsed.netloc != authority:
            raise ValueError("non-fixture proxy target")
    elif not target.startswith("/") or target.startswith("//"):
        raise ValueError("invalid fixture request target")
    return parsed.path


def summarize_input(body):
    items = body.get("input", [])
    serialized = json.dumps(items)
    return {
        "types": [item.get("type", "message") for item in items if isinstance(item, dict)],
        "roles": [item.get("role") for item in items if isinstance(item, dict) and "role" in item],
        "has_old_user": FIRST in serialized,
        "has_new_user": SECOND in serialized,
        "has_stub_compaction": OPAQUE in serialized,
        "has_old_assistant": ASSISTANT in serialized,
        "has_local_summary": SUMMARY in serialized,
        "has_tool_call": any(isinstance(item, dict) and item.get("type") == "function_call"
                             and item.get("call_id") == TOOL_CALL and item.get("name") == TOOL_NAME
                             for item in items),
        "has_tool_output": any(isinstance(item, dict) and item.get("type") == "function_call_output"
                               and item.get("call_id") == TOOL_CALL
                               and TOOL_OUTPUT in json.dumps(item.get("output")) for item in items),
        "has_tool_marker": TOOL_CALL in serialized or TOOL_OUTPUT in serialized,
    }


class CaptureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, tool_history=False):
        super().__init__(("127.0.0.1", 0), CaptureHandler)
        self.phase = "startup"
        self.records = []
        self.tool_history = tool_history


class CaptureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def reply(self, status, data, content_type="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def reject_target(self):
        self.server.records.append({"phase": self.server.phase, "method": self.command,
                                    "rejected_target": True})
        self.reply(403, b'{"error":{"message":"non-fixture target refused"}}')

    def checked_path(self):
        try:
            hosts = self.headers.get_all("Host", [])
            if len(hosts) != 1:
                raise ValueError("ambiguous host")
            return fixture_request_path(self.path, hosts[0], self.server.server_port)
        except ValueError:
            self.reject_target()
            return None

    def do_CONNECT(self):
        self.reject_target()

    def do_GET(self):
        if self.checked_path() is None:
            return
        self.server.records.append({"phase": self.server.phase, "method": "GET"})
        self.reply(404, b'{"error":{"message":"unexpected fixture request"}}')

    def do_POST(self):
        path = self.checked_path()
        if path is None:
            return
        try:
            if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
                chunks = []
                size = 0
                while True:
                    count = int(self.rfile.readline().split(b";", 1)[0], 16)
                    if count == 0:
                        self.rfile.readline()
                        break
                    size += count
                    if size > 8 * 1024 * 1024:
                        raise ValueError("synthetic body exceeds 8 MiB")
                    chunks.append(self.rfile.read(count))
                    self.rfile.read(2)
                raw = b"".join(chunks)
            else:
                size = int(self.headers.get("Content-Length", "0"))
                if size > 8 * 1024 * 1024:
                    raise ValueError("synthetic body exceeds 8 MiB")
                raw = self.rfile.read(size)
            encoding = self.headers.get("Content-Encoding", "identity")
            if encoding == "gzip":
                with gzip.GzipFile(fileobj=io.BytesIO(raw)) as stream:
                    raw = stream.read(MAX_BODY + 1)
            elif encoding == "zstd":
                from compression import zstd
                with zstd.ZstdFile(io.BytesIO(raw)) as stream:
                    raw = stream.read(MAX_BODY + 1)
            elif encoding != "identity":
                raise ValueError("unsupported request encoding: " + encoding)
            if len(raw) > MAX_BODY:
                raise ValueError("oversized fixture request")
            body = json.loads(raw)
            record = {
                "phase": self.server.phase,
                "method": "POST",
                "path": path if path in ("/v1/responses", "/v1/responses/compact") else "unexpected",
                "encoding": encoding,
                "dummy_auth": self.headers.get("Authorization") == "Bearer " + DUMMY_KEY,
                "input": summarize_input(body),
            }
            self.server.records.append(record)
            if path == "/v1/responses/compact":
                record["stub_output_types"] = ["compaction"]
                result = {
                    "id": "cmp_capture",
                    "object": "response.compaction",
                    "created_at": 1700000000,
                    "output": [{"id": "cmp_item", "type": "compaction", "encrypted_content": OPAQUE}],
                    "usage": {"input_tokens": 100, "output_tokens": 5, "total_tokens": 105},
                }
                self.reply(200, json.dumps(result).encode())
                return
            if path != "/v1/responses":
                self.reply(404, b'{"error":{"message":"loopback stub route missing"}}')
                return
            rid = "resp_capture_" + str(len(self.server.records))
            reply_text = SUMMARY if self.server.phase == "compact" else ASSISTANT
            message = {
                "id": "msg_" + rid, "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": reply_text, "annotations": []}],
            }
            is_v2 = "compaction_trigger" in record["input"]["types"]
            if is_v2:
                message = {"id": "cmp_" + rid, "type": "compaction", "encrypted_content": OPAQUE}
            elif self.server.tool_history and self.server.phase == "first_turn" and len(self.server.records) == 1:
                message = {"id": "fc_capture", "type": "function_call", "status": "completed",
                           "name": TOOL_NAME, "call_id": TOOL_CALL, "arguments": "{}"}
            is_message = message["type"] == "message"
            record["stub_output_types"] = [message["type"]]
            events = [
                {"type": "response.created", "response": {"id": rid, "object": "response", "status": "in_progress", "output": []}},
                {"type": "response.output_item.added", "output_index": 0,
                 "item": {**message, "status": "in_progress", "content": []} if is_message else message},
                {"type": "response.output_item.done", "output_index": 0, "item": message},
                {"type": "response.completed", "response": {
                    "id": rid, "object": "response", "status": "completed", "output": [message],
                    "usage": {"input_tokens": 100, "output_tokens": 5, "total_tokens": 105,
                              "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}},
                }},
            ]
            if is_message:
                events.insert(2, {"type": "response.output_text.delta", "item_id": message["id"],
                                  "output_index": 0, "content_index": 0, "delta": reply_text})
            data = "".join("event: " + e["type"] + "\ndata: " + json.dumps(e) + "\n\n" for e in events)
            self.reply(200, data.encode(), "text/event-stream")
        except Exception as exc:
            self.server.records.append({"phase": self.server.phase, "stub_error": type(exc).__name__})
            self.reply(400, b'{"error":{"message":"synthetic capture failed"}}')


class Rpc:
    def __init__(self, process, tool_history=False):
        self.process = process
        self.messages = queue.Queue()
        self.notifications = []
        self.stderr = []
        self.diagnostic_codes = set()
        self.counter = 0
        self.tool_history = tool_history
        self.tool_calls = 0
        self.rejected_requests = 0
        threading.Thread(target=self.read_stdout, daemon=True).start()
        threading.Thread(target=self.read_stderr, daemon=True).start()

    def read_stdout(self):
        for line in self.process.stdout:
            try:
                self.messages.put(json.loads(line))
            except json.JSONDecodeError:
                pass
        self.messages.put({"_eof": True})

    def read_stderr(self):
        # Drain without retaining config, prompts, bearer values, or local paths.
        for line in self.process.stderr:
            for word in ("sandbox", "permission", "denied", "config", "database",
                         "sqlite", "lock", "keychain", "exec", "network", "read",
                         "write", "directory", "migration", "operation", "spawn",
                         "failed", "panic", "invalid", "logger", "telemetry"):
                if word in line.lower():
                    self.diagnostic_codes.add(word)

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def handle_server_request(self, message):
        params = message.get("params", {})
        if (self.tool_history and self.tool_calls == 0 and message["method"] == "item/tool/call"
                and params.get("tool") == TOOL_NAME and params.get("callId") == TOOL_CALL
                and params.get("arguments") == {}):
            self.tool_calls += 1
            self.send({"id": message["id"], "result": {
                "contentItems": [{"type": "inputText", "text": TOOL_OUTPUT}], "success": True,
            }})
        else:
            self.rejected_requests += 1
            self.send({"id": message["id"], "error": {
                "code": -32601, "message": "No external tool execution in capture harness",
            }})

    def wait(self, predicate, timeout=35):
        # Notifications may precede the response to turn/start or compact/start.
        for message in self.notifications:
            if predicate(message):
                return message
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                message = self.messages.get(timeout=max(0.1, deadline - time.monotonic()))
            except queue.Empty as exc:
                raise TimeoutError("RPC wait expired") from exc
            if message.get("_eof"):
                raise RuntimeError("app-server exited")
            if "method" in message:
                self.notifications.append(message)
                if "id" in message:
                    self.handle_server_request(message)
            if predicate(message):
                return message
        raise TimeoutError("RPC wait expired")

    def call(self, method, params):
        self.counter += 1
        request_id = self.counter
        self.send({"id": request_id, "method": method, "params": params})
        result = self.wait(lambda m: m.get("id") == request_id and "method" not in m)
        if "error" in result:
            raise RuntimeError("RPC request failed")
        return result.get("result", {})

    def run_turn(self, thread_id, text):
        result = self.call("turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": text, "text_elements": []}]})
        turn_id = result.get("turn", {}).get("id")
        done = self.wait(lambda m: m.get("method") == "turn/completed" and
                         (not turn_id or m.get("params", {}).get("turn", {}).get("id") == turn_id))
        turn = done.get("params", {}).get("turn", {})
        if turn.get("status") != "completed":
            raise RuntimeError("turn did not complete")


def stop_process(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
    for stream in (process.stdin, process.stdout, process.stderr):
        if stream:
            stream.close()


TOP_KEYS = {
    "model", "model_provider", "model_reasoning_effort", "model_context_window",
    "model_auto_compact_token_limit", "model_verbosity", "disable_response_storage",
    "experimental_realtime_webrtc_call_base_url", "experimental_realtime_ws_base_url",
    "model_providers", "features",
}
PROVIDER_KEYS = {
    "name", "base_url", "wire_api", "requires_openai_auth",
    "experimental_bearer_token", "supports_websockets", "capabilities",
    "request_max_retries", "stream_max_retries", "stream_idle_timeout_ms",
}


def safe_fixture_url(value):
    parsed = urlsplit(value)
    host = parsed.hostname or ""
    return (
        parsed.scheme in ("http", "https")
        and not parsed.username and not parsed.password
        and not parsed.query and not parsed.fragment and bool(host)
    )


def prepare_config(text, port):
    """Parse actual generated TOML; never infer capabilities or alter transport."""
    if len(text.encode()) > 65536:
        raise ValueError("fixture too large")
    config = tomllib.loads(text)
    if set(config) - TOP_KEYS:
        raise ValueError("unsupported fixture settings")
    provider_id = config.get("model_provider")
    providers = config.get("model_providers")
    if not isinstance(providers, dict) or set(providers) != {provider_id}:
        raise ValueError("fixture must contain exactly its selected provider")
    provider = providers[provider_id]
    if set(provider) - PROVIDER_KEYS:
        raise ValueError("unsupported provider settings")
    if provider.get("wire_api") != "responses":
        raise ValueError("fixture must use Responses")
    for name in ("name",):
        if not isinstance(provider.get(name), str) or not provider[name]:
            raise ValueError("missing provider name")
    if not isinstance(config.get("model"), str) or not config["model"]:
        raise ValueError("missing model")
    if provider.get("capabilities", {}).get("remote_compaction") != "v2":
        raise ValueError("fixture must explicitly declare remote compaction v2")
    for key, value in config.items():
        if key.endswith("_base_url") and not safe_fixture_url(value):
            raise ValueError("unsafe fixture URL")
    if not safe_fixture_url(provider.get("base_url", "")):
        raise ValueError("unsafe provider URL")
    bearer = provider.get("experimental_bearer_token")
    if bearer is not None and not re.fullmatch(
        r"(?:sk-)?(?:test|synthetic|dummy|loopback)(?:[-_][A-Za-z0-9]+)*", bearer
    ):
        raise ValueError("non-synthetic bearer rejected")
    features = config.get("features", {})
    if not isinstance(features, dict) or set(features) - {"remote_compaction_v2", "shell_snapshot"}:
        raise ValueError("unsupported fixture features")
    updated = copy.deepcopy(config)
    selected = updated["model_providers"][provider_id]
    selected["base_url"] = f"http://127.0.0.1:{port}/v1"
    if bearer is not None:
        selected["experimental_bearer_token"] = DUMMY_KEY
    return updated


def dump_toml(config):
    """Serialize the bounded generated-fixture subset, checking the round trip."""
    lines = []

    def scalar(value):
        if isinstance(value, str):
            return json.dumps(value, ensure_ascii=True)
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, int):
            return str(value)
        raise ValueError("unsupported fixture value type")

    def table(value, path=()):
        if path:
            lines.append("[" + ".".join(json.dumps(key) for key in path) + "]")
        for key, item in value.items():
            if not isinstance(item, dict):
                lines.append(json.dumps(key) + " = " + scalar(item))
        for key, item in value.items():
            if isinstance(item, dict):
                lines.append("")
                table(item, (*path, key))

    table(config)
    text = "\n".join(lines) + "\n"
    if tomllib.loads(text) != config:
        raise ValueError("fixture serialization mismatch")
    return text


def isolated_environment(root, port):
    env = {
        "PATH": "/usr/bin:/bin", "HOME": str(root / "home"),
        "CODEX_HOME": str(root / "codex"), "TMPDIR": str(root / "tmp"),
        "XDG_CONFIG_HOME": str(root / "xdg-config"),
        "XDG_DATA_HOME": str(root / "xdg-data"),
        "XDG_CACHE_HOME": str(root / "xdg-cache"),
        "NO_PROXY": "*", "no_proxy": "*", "RUST_LOG": "off",
        "LANG": "en_US.UTF-8", "TERM": "dumb", "SHELL": "/bin/sh",
        "OTEL_SDK_DISABLED": "true", "OPENAI_API_KEY": DUMMY_KEY,
    }
    # Older reqwest builds can consult macOS system proxy even with NO_PROXY.
    # This stub never forwards, including when used as an HTTP proxy.
    for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
        env[key] = f"http://127.0.0.1:{port}"
    return env


def sandbox_profile(root, binary, port):
    quote = lambda value: json.dumps(str(value))
    # Only this app-server can execute; no shell/tool child can escape the fixture.
    return "\n".join([
        "(version 1)", "(allow default)", "(deny network*)",
        f'(allow network-outbound (remote ip "localhost:{port}"))',
        '(allow network-bind (local ip "localhost:*"))',
        "(deny file-write*)",
        f"(allow file-write* (subpath {quote(root)}) (literal \"/dev/null\"))",
        f"(deny file-read* (subpath {quote(Path.home().resolve())}))",
        f"(allow file-read* (subpath {quote(root)}) (literal {quote(binary)}))",
        "(deny process-exec)",
        f"(allow process-exec (literal {quote(binary)}))",
    ]) + "\n"


def resolve_binary(path):
    binary = Path(path).resolve(strict=True)
    with binary.open("rb") as stream:
        header = stream.read(4096)
    if header.startswith(b"#!"):
        # Desktop ships a shell trampoline. Never grant a general shell executor.
        expected = b'exec "$bin_dir/../CodexCLI.app/Contents/MacOS/codex" "$@"'
        if expected not in header.splitlines():
            raise ValueError("unrecognized binary wrapper")
        binary = (binary.parent.parent / "CodexCLI.app/Contents/MacOS/codex").resolve(strict=True)
    with binary.open("rb") as stream:
        magic = stream.read(4)
    if magic not in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe",
                     b"\xfe\xed\xfa\xcf", b"\xfe\xed\xfa\xce",
                     b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        raise ValueError("expected a desktop Mach-O executable")
    return binary


def assert_capture(records, tool_history=False):
    posts = [r for r in records if r.get("method") == "POST"]
    if any("stub_error" in r or r.get("rejected_target") or r.get("method") != "POST"
           for r in records):
        raise AssertionError("unexpected fixture traffic")
    phases = {phase: [r for r in posts if r["phase"] == phase]
              for phase in ("first_turn", "compact", "next_turn")}
    if len(posts) != (4 if tool_history else 3) or any(
        len(value) != (2 if tool_history and phase == "first_turn" else 1)
        for phase, value in phases.items()
    ):
        raise AssertionError("unexpected request count per phase")
    if any(r["path"] != "/v1/responses" or not r["dummy_auth"] for r in posts):
        raise AssertionError("non-Responses path or non-dummy authentication")
    first, compact, following = (phases[p][0] for p in phases)
    if not first["input"]["has_old_user"]:
        raise AssertionError("first turn missing")
    if "compaction_trigger" not in compact["input"]["types"]:
        raise AssertionError("V2 trigger missing")
    if not compact["input"]["has_old_assistant"]:
        raise AssertionError("compaction did not receive previous assistant")
    if compact["stub_output_types"] != ["compaction"]:
        raise AssertionError("compaction output missing")
    if not following["input"]["has_stub_compaction"] or "compaction" not in following["input"]["types"]:
        raise AssertionError("continuation did not retain opaque compaction")
    if not following["input"]["has_new_user"] or not following["input"]["has_old_user"]:
        raise AssertionError("continuation user history missing")
    if following["input"]["has_old_assistant"] or following["input"]["has_local_summary"]:
        raise AssertionError("old assistant or local summary repeated after compaction")
    checks = {
        "v2_trigger": True, "compaction_output": True,
        "continuation_uses_compaction": True, "old_assistant_not_repeated": True,
        "user_history_retained": True, "dummy_auth_only": True,
        "responses_only": True,
    }
    if tool_history:
        after_tool = phases["first_turn"][1]
        if first["stub_output_types"] != ["function_call"] or after_tool["stub_output_types"] != ["message"]:
            raise AssertionError("synthetic tool then assistant sequence missing")
        if not all(item["input"]["has_tool_call"] and item["input"]["has_tool_output"]
                   for item in (after_tool, compact)):
            raise AssertionError("tool history missing before compaction")
        if following["input"]["has_tool_marker"] or any(
            kind in following["input"]["types"] for kind in ("function_call", "function_call_output")
        ):
            raise AssertionError("tool history repeated after compaction")
        checks.update(tool_history_before_compaction=True, old_tool_history_not_repeated=True)
    return checks


def run_fixture(binary, fixture, tool_history=False):
    result = {"status": "error", "scope": "synthetic-loopback-desktop-engine",
              "live_ui_verified": False, "upstream_semantics_verified": False,
              "tool_history_verified": False}
    stage = "validation"
    rpc = None
    server = None
    try:
        text = Path(fixture).read_text(encoding="utf-8")
        prepare_config(text, 12345)
        if sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").is_file():
            raise RuntimeError("OS network isolation unavailable")
        binary = resolve_binary(binary)
        with binary.open("rb") as stream:
            result["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        with ExitStack() as cleanup:
            root = Path(cleanup.enter_context(
                tempfile.TemporaryDirectory(prefix="codex-compact-regression-")
            )).resolve()
            for directory in ("home", "codex", "tmp", "workspace"):
                (root / directory).mkdir(mode=0o700)
            server = CaptureServer(tool_history=tool_history)
            cleanup.callback(server.server_close)
            serving = threading.Thread(target=server.serve_forever, daemon=True)
            serving.start()
            cleanup.callback(serving.join, 3)
            cleanup.callback(server.shutdown)
            config = prepare_config(text, server.server_port)
            home = root / "codex"
            config_path = home / "config.toml"
            config_path.write_text(dump_toml(config), encoding="utf-8")
            auth_path = home / "auth.json"
            auth_path.write_text(json.dumps({"OPENAI_API_KEY": DUMMY_KEY}), encoding="utf-8")
            config_path.chmod(0o600)
            auth_path.chmod(0o600)
            profile = root / "network.sb"
            profile.write_text(sandbox_profile(root, binary, server.server_port), encoding="utf-8")
            stage = "initialize"
            command = ["/usr/bin/sandbox-exec", "-f", str(profile), str(binary),
                       "-c", 'analytics.enabled=false', "-c", 'otel.exporter="none"',
                       "-c", 'check_for_update_on_startup=false',
                       "-c", 'cli_auth_credentials_store="file"',
                       "-c", 'features.shell_snapshot=false',
                       "-c", 'web_search="disabled"', "app-server"]
            process = subprocess.Popen(
                command, cwd=root / "workspace", env=isolated_environment(root, server.server_port),
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, bufsize=1, start_new_session=True,
            )
            cleanup.callback(stop_process, process)
            rpc = Rpc(process, tool_history=tool_history)
            rpc.call("initialize", {
                "clientInfo": {"name": "compact_loopback_regression", "version": "1.0.0"},
                "capabilities": {"experimentalApi": True},
            })
            rpc.send({"method": "initialized", "params": {}})
            stage = "thread_start"
            thread_params = {
                "model": config["model"], "modelProvider": config["model_provider"],
                "cwd": str(root / "workspace"), "approvalPolicy": "never",
                "sandbox": "read-only", "ephemeral": True,
                "baseInstructions": ("Synthetic protocol test. Only capture_echo is allowed."
                                     if tool_history else "Synthetic protocol test. Do not call any tools."),
                "developerInstructions": "Reply only with a short synthetic completion.",
            }
            if tool_history:
                thread_params["dynamicTools"] = [{
                    "name": TOOL_NAME, "description": "Return a synthetic fixture value; no external execution.",
                    "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
                }]
            started = rpc.call("thread/start", thread_params)
            thread_id = started["thread"]["id"]
            stage = server.phase = "first_turn"
            rpc.run_turn(thread_id, FIRST)
            stage = server.phase = "compact"
            rpc.call("thread/compact/start", {"threadId": thread_id})
            rpc.wait(lambda m: m.get("method") == "thread/compacted" or (
                m.get("method") == "item/completed" and
                m.get("params", {}).get("item", {}).get("type") == "contextCompaction"))
            stage = server.phase = "next_turn"
            rpc.run_turn(thread_id, SECOND)
            stage = "assertions"
            if rpc.rejected_requests or rpc.tool_calls != int(tool_history):
                raise AssertionError("unexpected dynamic tool dispatch")
            result["checks"] = assert_capture(server.records, tool_history=tool_history)
            result["tool_history_verified"] = tool_history
            result["synthetic_tool_call_count"] = rpc.tool_calls
            result["request_count"] = len(server.records)
            result["network_policy"] = "OS sandbox: selected loopback port only"
            result["config_preserved"] = {
                "provider_identity": True, "model": True, "capabilities": True,
                "websocket_settings": True, "storage_settings": True,
            }
            result["status"] = "passed"
    except Exception as exc:
        # Never emit exception text, process logs, config values, or HTTP bodies.
        result["failure_stage"] = stage
        result["error_type"] = type(exc).__name__
        if rpc is not None:
            result["diagnostic_codes"] = sorted(rpc.diagnostic_codes)
            result["synthetic_tool_call_count"] = rpc.tool_calls
            result["rejected_rpc_requests"] = rpc.rejected_requests
            result["process_exit_code"] = rpc.process.poll()
            result["event_counts"] = {
                event: sum(m.get("method") == event for m in rpc.notifications)
                for event in ("turn/started", "turn/completed", "error",
                              "item/started", "item/completed", "thread/compacted")
            }
            errors = json.dumps([m.get("params", {}).get("error") for m in rpc.notifications
                                 if m.get("method") == "error"]).lower()
            result["error_hints"] = [hint for hint in (
                "127.0.0.1", "localhost", "permission", "operation not permitted",
                "connection refused", "error sending request", "websocket",
                "unauthorized", "401", "403", "404", "certificate", "stream disconnected",
                "not logged", "api key", "unknown model", "failed to parse", "proxy",
            ) if hint in errors]
        if server is not None:
            result["request_counts"] = {
                phase: sum(r["phase"] == phase for r in server.records)
                for phase in ("startup", "first_turn", "compact", "next_turn")
            }
            result["stub_errors"] = sorted({
                r["stub_error"] for r in server.records if "stub_error" in r
            })
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-fixture", required=True,
                        help="Actual generated TOML with no real credentials; never modified")
    parser.add_argument("--binary", required=True, help="Desktop-bundled Codex executable")
    parser.add_argument("--tool-history", action="store_true",
                        help="Include one synthetic dynamic tool call; never execute external tools")
    args = parser.parse_args()
    result = run_fixture(args.binary, args.config_fixture, tool_history=args.tool_history)
    print(json.dumps(result, indent=2), flush=True)
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
