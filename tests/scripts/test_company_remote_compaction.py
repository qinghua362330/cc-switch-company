"""Company CI offline checks; do not launch Codex or contact any network."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import tempfile
import tomllib
import unittest
from email.message import Message
from types import SimpleNamespace
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "remote_compaction_harness", ROOT / "scripts/test-codex-remote-compaction.py"
)
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)

FIXTURE = """
model_provider = "custom"
model = "gpt-5.6-sol"
model_reasoning_effort = "high"
experimental_realtime_ws_base_url = "https://company.example/v1"
[model_providers.custom]
name = "Company Codex"
base_url = "https://company.example/v1"
wire_api = "responses"
requires_openai_auth = true
[model_providers.custom.capabilities]
remote_compaction = "v2"
"""


def records():
    def record(phase, items, output):
        return {
            "method": "POST", "phase": phase, "path": "/v1/responses",
            "dummy_auth": True,
            "input": HARNESS.summarize_input({"input": items}),
            "stub_output_types": [output],
        }
    user = {"type": "message", "role": "user", "content": HARNESS.FIRST}
    assistant = {"type": "message", "role": "assistant", "content": HARNESS.ASSISTANT}
    compact = {"type": "compaction", "encrypted_content": HARNESS.OPAQUE}
    return [
        record("first_turn", [user], "message"),
        record("compact", [user, assistant, {"type": "compaction_trigger"}], "compaction"),
        record("next_turn", [user, compact, {
            "type": "message", "role": "user", "content": HARNESS.SECOND,
        }], "message"),
    ]


def tool_records():
    trace = records()
    after_tool = copy.deepcopy(trace[0])
    trace[0]["stub_output_types"] = ["function_call"]
    tool_items = [
        {"type": "function_call", "name": HARNESS.TOOL_NAME, "call_id": HARNESS.TOOL_CALL, "arguments": "{}"},
        {"type": "function_call_output", "call_id": HARNESS.TOOL_CALL, "output": HARNESS.TOOL_OUTPUT},
    ]
    tool_summary = HARNESS.summarize_input({"input": tool_items})
    for record in (after_tool, trace[1]):
        record["input"]["types"].extend(tool_summary["types"])
        for key in ("has_tool_call", "has_tool_output", "has_tool_marker"):
            record["input"][key] = tool_summary[key]
    trace.insert(1, after_tool)
    return trace


class RemoteCompactionHarnessTests(unittest.TestCase):
    def test_real_generated_shape_only_replaces_base_url(self):
        original = tomllib.loads(FIXTURE)
        actual = HARNESS.prepare_config(FIXTURE, 43210)
        expected = copy.deepcopy(original)
        expected["model_providers"]["custom"]["base_url"] = "http://127.0.0.1:43210/v1"
        self.assertEqual(actual, expected)
        self.assertEqual(tomllib.loads(HARNESS.dump_toml(actual)), expected)
        self.assertNotIn("supports_websockets", actual["model_providers"]["custom"])
        self.assertNotIn("disable_response_storage", actual)

    def test_existing_ws_and_storage_settings_are_unchanged(self):
        text = FIXTURE.replace(
            'model_provider = "custom"',
            'disable_response_storage = true\nmodel_provider = "custom"',
        ).replace(
            'requires_openai_auth = true',
            'requires_openai_auth = true\nsupports_websockets = false',
        )
        data = HARNESS.prepare_config(text, 43210)
        self.assertTrue(data["disable_response_storage"])
        self.assertFalse(data["model_providers"]["custom"]["supports_websockets"])

    def test_only_synthetic_bearer_is_accepted_and_replaced(self):
        def with_bearer(value):
            return FIXTURE.replace(
                'requires_openai_auth = true',
                'requires_openai_auth = true\nexperimental_bearer_token = ' + json.dumps(value),
            )
        data = HARNESS.prepare_config(with_bearer("test-placeholder"), 43210)
        self.assertEqual(data["model_providers"]["custom"]["experimental_bearer_token"],
                         HARNESS.DUMMY_KEY)
        with self.assertRaises(ValueError):
            HARNESS.prepare_config(with_bearer("not-an-approved-fixture"), 43210)

    def test_does_not_infer_v2_from_display_name(self):
        for value in ("unsupported", ""):
            with self.subTest(value=value), self.assertRaises(ValueError):
                HARNESS.prepare_config(FIXTURE.replace('"v2"', json.dumps(value)), 43210)

    def test_rejects_external_tools_profiles_and_extra_providers(self):
        for extra in (
            '\n[mcp_servers.demo]\ncommand = "echo"',
            '\n[profiles.demo]\nmodel = "other"',
            '\n[model_providers.other]\nbase_url = "https://other.example"',
            '\n[features]\nunknown_feature = true',
        ):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                HARNESS.prepare_config(FIXTURE + extra, 43210)

    def test_rejects_credentials_and_queries_in_urls(self):
        for url in ("https://user:password@company.example/v1",
                    "https://company.example/v1?key=test", "file:///tmp/fixture"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                HARNESS.prepare_config(FIXTURE.replace("https://company.example/v1", url), 43210)

    def test_environment_does_not_inherit_auth_or_proxy(self):
        with patch.dict(os.environ, {"HTTPS_PROXY": "https://proxy.example",
                                     "EXTRA_SECRET": "fixture"}):
            env = HARNESS.isolated_environment(Path("/tmp/isolated"), 43210)
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            self.assertEqual(env[key], "http://127.0.0.1:43210")
        self.assertNotIn("EXTRA_SECRET", env)
        self.assertEqual(env["OPENAI_API_KEY"], HARNESS.DUMMY_KEY)
        self.assertEqual(env["HOME"], "/tmp/isolated/home")
        self.assertEqual(env["CODEX_HOME"], "/tmp/isolated/codex")

    def test_sandbox_limits_network_and_writes_and_child_execution(self):
        policy = HARNESS.sandbox_profile(Path("/tmp/isolated"), Path("/Applications/codex"), 43210)
        self.assertIn("(deny network*)", policy)
        self.assertEqual(policy.count("(allow network-outbound"), 1)
        self.assertNotIn("(allow network-inbound", policy)
        self.assertIn('(remote ip "localhost:43210")', policy)
        self.assertEqual(policy.count("(allow network-bind"), 1)
        self.assertIn('(allow network-bind (local ip "localhost:*"))', policy)
        self.assertNotIn('(remote ip "localhost:*")', policy)
        self.assertIn("(deny file-write*)", policy)
        self.assertIn("(deny process-exec)", policy)

    def handler(self, target, method="POST", host="127.0.0.1:43210"):
        handler = object.__new__(HARNESS.CaptureHandler)
        handler.server = SimpleNamespace(server_port=43210, phase="first_turn", records=[], tool_history=False)
        handler.path = target
        handler.command = method
        handler.headers = Message()
        handler.headers["Host"] = host
        body = json.dumps({"input": [{"role": "user", "content": HARNESS.FIRST}]}).encode()
        handler.headers["Content-Length"] = str(len(body))
        handler.headers["Authorization"] = "Bearer " + HARNESS.DUMMY_KEY
        handler.rfile = io.BytesIO(body)
        handler.reply = Mock()
        return handler

    def test_stub_accepts_origin_and_own_absolute_proxy_requests(self):
        for target in ("/v1/responses", "http://127.0.0.1:43210/v1/responses"):
            with self.subTest(target=target):
                handler = self.handler(target)
                handler.do_POST()
                self.assertEqual(handler.reply.call_args.args[0], 200)
                record = handler.server.records[0]
                self.assertEqual(record["path"], "/v1/responses")
                self.assertTrue(record["dummy_auth"])
                self.assertTrue(record["input"]["has_old_user"])

    def test_stub_rejects_foreign_proxy_targets_before_reading_body(self):
        for target in ("http://outside.example/v1/responses", "//outside.example/v1/responses",
                       "http://127.0.0.1:43211/v1/responses", "https://127.0.0.1:43210/v1/responses",
                       "http://user@127.0.0.1:43210/v1/responses", "/v1/responses?key=test",
                       "/v1/responses#fragment", "http://127.0.0.1:43210.outside.example/v1/responses"):
            with self.subTest(target=target):
                handler = self.handler(target)
                handler.do_POST()
                self.assertEqual(handler.reply.call_args.args[0], 403)
                self.assertEqual(handler.rfile.tell(), 0)
                self.assertTrue(handler.server.records[0]["rejected_target"])
                self.assertNotIn(target, json.dumps(handler.server.records))

    def test_stub_rejects_foreign_host_duplicate_host_and_connect(self):
        for method in ("GET", "POST", "CONNECT"):
            handler = self.handler("/v1/responses", method, "outside.example")
            getattr(handler, "do_" + method)()
            self.assertEqual(handler.reply.call_args.args[0], 403)
        handler = self.handler("/v1/responses")
        handler.headers["Host"] = "outside.example"
        handler.do_POST()
        self.assertEqual(handler.reply.call_args.args[0], 403)
        handler = self.handler("127.0.0.1:43210", "CONNECT")
        handler.do_CONNECT()
        self.assertEqual(handler.reply.call_args.args[0], 403)

    def test_missing_os_isolation_fails_without_launching_process(self):
        with tempfile.TemporaryDirectory() as temp:
            fixture = Path(temp) / "config.toml"
            fixture.write_text(FIXTURE)
            with patch.object(HARNESS.sys, "platform", "unsupported"), \
                    patch.object(HARNESS.subprocess, "Popen") as launch:
                result = HARNESS.run_fixture(__file__, fixture)
            launch.assert_not_called()
            self.assertEqual(result["status"], "error")
            self.assertNotIn(str(fixture), json.dumps(result))
            self.assertNotIn("company.example", json.dumps(result))

    def test_resolves_desktop_wrapper_without_executing_shell(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            wrapper = root / "bin/codex"
            wrapper.parent.mkdir()
            wrapper.write_text('#!/bin/sh\nexec "$bin_dir/../CodexCLI.app/Contents/MacOS/codex" "$@"\n')
            engine = root / "CodexCLI.app/Contents/MacOS/codex"
            engine.parent.mkdir(parents=True)
            engine.write_bytes(b"\xcf\xfa\xed\xfe" + b"fixture")
            self.assertEqual(HARNESS.resolve_binary(wrapper), engine.resolve())
            wrapper.write_text("#!/bin/sh\nexit 0\n")
            with self.assertRaises(ValueError):
                HARNESS.resolve_binary(wrapper)

    def test_valid_trace_proves_v2_and_history_retention(self):
        checks = HARNESS.assert_capture(records())
        self.assertTrue(all(checks.values()))

    def test_tool_mode_emits_function_call_then_assistant(self):
        first = self.handler("/v1/responses")
        first.server.tool_history = True
        first.do_POST()
        self.assertEqual(first.reply.call_args.args[0], 200)
        self.assertEqual(first.server.records[0]["stub_output_types"], ["function_call"])
        wire = first.reply.call_args.args[1].decode()
        self.assertIn(HARNESS.TOOL_CALL, wire)
        self.assertNotIn("response.output_text.delta", wire)
        following = self.handler("/v1/responses")
        following.server = first.server
        following.do_POST()
        self.assertEqual(following.server.records[1]["stub_output_types"], ["message"])

    def test_tool_trace_requires_history_before_but_not_after_compaction(self):
        self.assertTrue(all(HARNESS.assert_capture(tool_records(), tool_history=True).values()))
        for phase, key, value in ((1, "has_tool_output", False), (2, "has_tool_output", False),
                                  (2, "has_tool_call", False), (3, "has_tool_marker", True)):
            trace = tool_records()
            trace[phase]["input"][key] = value
            with self.subTest(phase=phase, key=key), self.assertRaises(AssertionError):
                HARNESS.assert_capture(trace, tool_history=True)
        with self.assertRaises(AssertionError):
            HARNESS.assert_capture(records(), tool_history=True)
        with self.assertRaises(AssertionError):
            HARNESS.assert_capture(tool_records())

    def test_rpc_only_answers_one_exact_synthetic_tool_request(self):
        def rpc(enabled=True):
            instance = HARNESS.Rpc.__new__(HARNESS.Rpc)
            instance.tool_history = enabled
            instance.tool_calls = instance.rejected_requests = 0
            instance.send = Mock()
            return instance

        request = {"id": 9, "method": "item/tool/call", "params": {
            "tool": HARNESS.TOOL_NAME, "callId": HARNESS.TOOL_CALL, "arguments": {},
        }}
        instance = rpc()
        instance.messages = queue.Queue()
        instance.notifications = []
        instance.messages.put(request)
        instance.messages.put({"id": 10, "result": {}})
        instance.wait(lambda message: message.get("id") == 10)
        instance.send.assert_called_once_with({"id": 9, "result": {
            "contentItems": [{"type": "inputText", "text": HARNESS.TOOL_OUTPUT}], "success": True,
        }})
        self.assertEqual(instance.tool_calls, 1)
        instance.handle_server_request(request)
        self.assertIn("error", instance.send.call_args.args[0])
        self.assertEqual(instance.rejected_requests, 1)
        for key, value in (("tool", "other"), ("callId", "other"), ("arguments", {"command": "noop"})):
            changed = copy.deepcopy(request)
            changed["params"][key] = value
            instance = rpc()
            instance.handle_server_request(changed)
            self.assertEqual(instance.tool_calls, 0)
            self.assertIn("error", instance.send.call_args.args[0])
        instance = rpc(False)
        instance.handle_server_request(request)
        self.assertIn("error", instance.send.call_args.args[0])

    def test_rejected_proxy_traffic_cannot_be_ignored_in_success_report(self):
        for method in ("CONNECT", "GET", "POST"):
            trace = records() + [{"phase": "startup", "method": method, "rejected_target": True}]
            with self.subTest(method=method), self.assertRaises(AssertionError):
                HARNESS.assert_capture(trace)

    def test_rejects_missing_trigger_old_assistant_and_missing_compaction(self):
        for phase, key, value in (
            (1, "types", ["message"]), (2, "has_old_assistant", True),
            (2, "has_stub_compaction", False), (2, "has_new_user", False),
            (2, "has_local_summary", True),
        ):
            trace = records()
            trace[phase]["input"][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                HARNESS.assert_capture(trace)

    def test_rejects_legacy_endpoint_wrong_auth_and_retries(self):
        for key, value in (("path", "/v1/responses/compact"), ("dummy_auth", False)):
            trace = records()
            trace[1][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                HARNESS.assert_capture(trace)
        trace = records()
        trace.append(copy.deepcopy(trace[2]))
        with self.assertRaises(AssertionError):
            HARNESS.assert_capture(trace)

    def test_rpc_accepts_completion_arriving_before_start_response(self):
        rpc = HARNESS.Rpc.__new__(HARNESS.Rpc)
        notification = {"method": "turn/completed", "params": {"turn": {"id": "fixture"}}}
        rpc.notifications = [notification]
        rpc.messages = queue.Queue()
        self.assertEqual(rpc.wait(lambda m: m.get("method") == "turn/completed"), notification)


if __name__ == "__main__":
    unittest.main()
