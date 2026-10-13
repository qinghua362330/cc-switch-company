"""Offline regressions: workflow snippets, mocked installer functions, synthetic history."""
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]

def workflow_step(file, name):
    text = (ROOT / '.github/workflows' / file).read_text()
    block = text.split('      - name: ' + name + '\n', 1)[1]
    lines = []
    for line in block.split('        run: |\n', 1)[1].splitlines():
        if line.strip() and not line.startswith('          '):
            break
        lines.append(line)
    return textwrap.dedent('\n'.join(lines)).rstrip() + '\n'

class SafetyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='cc-company-safety-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.env = dict(os.environ, CC_SWITCH_TEST_HOME=str(self.home))
        self.env['GITHUB_OUTPUT'] = str(self.home / 'output')
        (self.home / 'tmp').mkdir()
        (self.home / 'Applications').mkdir()
        self.env.update(TMPDIR=str(self.home / 'tmp'), CC_SWITCH_TEST_MODE='1',
                        CC_SWITCH_APP_DIR=str(self.home / 'Applications'))

    def mock_command(self, name, body):
        bin_dir = self.home / 'bin'; bin_dir.mkdir(exist_ok=True)
        executable = bin_dir / name
        executable.write_text('#!/usr/bin/env bash\n' + body + '\n')
        executable.chmod(0o755)
        self.env['PATH'] = str(bin_dir) + os.pathsep + os.environ['PATH']

    def bash(self, code, **env):
        return subprocess.run(['bash', '-c', code], cwd=self.home, env=dict(self.env, **env), text=True, capture_output=True)

    def test_merge_failure_is_not_clean(self):
        code = workflow_step('upstream-sync.yml', 'Attempt merge')
        result = self.bash('git() { return 1; }; export -f git\n' + code)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('clean=true', (self.home / 'output').read_text() if (self.home / 'output').exists() else '')

    def test_prepare_does_not_push_main(self):
        text = (ROOT / '.github/workflows/upstream-sync.yml').read_text()
        push = text.split('      - name: Push merge commit\n')[1].split('\n      - name:', 1)[0]
        self.assertIn("== 'auto'", push)
        self.assertIn('HEAD:refs/heads/', text)
        self.assertNotIn('PR 创建失败', text)

    def test_dispatch_has_permission_and_real_status(self):
        text = (ROOT / '.github/workflows/upstream-sync.yml').read_text()
        self.assertIn('actions: write', text)
        self.assertIn('gh workflow run company-release.yml', text)
        self.assertIn('release-dispatched', text)
        self.assertNotIn('RESULT="released"', text)

    def test_ci_runs_regressions_before_bounded_vitest_and_stops_on_failure(self):
        self.mock_command('python3', 'printf "%s\\n" "$*" >> "$CC_SWITCH_TEST_HOME/python-calls"; exit "${PYTHON_RESULT:-0}"')
        self.mock_command('pnpm', 'printf "%s\\n" "$*" >> "$CC_SWITCH_TEST_HOME/pnpm-calls"')
        code = workflow_step('upstream-sync.yml', 'Unit tests')
        result = self.bash(code, PYTHON_RESULT='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.home / 'pnpm-calls').exists())
        result = self.bash(code, PYTHON_RESULT='0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('-m unittest discover -s tests/scripts -p test_company_release_safety.py -v',
                      (self.home / 'python-calls').read_text())
        self.assertEqual((self.home / 'pnpm-calls').read_text().strip(),
                         'vitest run --maxWorkers=2 --minWorkers=1 --testTimeout=15000 --hookTimeout=15000')

    def test_release_pins_tag_sha_and_preserves_iteration(self):
        text = (ROOT / '.github/workflows/company-release.yml').read_text()
        self.assertIn('needs.resolve.outputs.build_sha', text)
        self.assertIn('refs/tags/', text)
        self.assertIn('company_iteration', text)
        self.assertIn('Prepare company build version', text)
        self.assertNotRegex(text, r'key_password\s*=.*\bor\s+["\'][^"\']+["\']')

    def install_functions(self):
        # Intercept BEFORE evaluating the installer header or installing its EXIT trap.
        # No intercepted command delegates to its real executable.
        mocks = r'''
record_mock() {
  python3 - "$@" <<'PY_MOCK'
import json, os, pathlib, sys
root = pathlib.Path(os.environ['CC_SWITCH_TEST_HOME']).resolve()
lexical_roots = {pathlib.Path(os.path.abspath(os.environ['CC_SWITCH_TEST_HOME'])), root}
name, args = sys.argv[1], sys.argv[2:]
paths = []
if name in {'rm', 'xattr', 'open', 'lipo'}:
    paths = args[-1:]
elif name == 'ditto':
    paths = args[-2:]
elif name == 'sudo':
    if not args or args[0] not in {'rm', 'ditto', 'xattr'}:
        raise SystemExit('Unexpected sudo command in fixture')
    paths = args[-2:] if args[0] == 'ditto' else args[-1:]
for value in paths:
    lexical = pathlib.Path(os.path.abspath(value))
    if not any(lexical.is_relative_to(base) and lexical != base for base in lexical_roots):
        raise SystemExit('Mock rejected destination outside test home: ' + value)
    if not lexical.resolve().is_relative_to(root):
        raise SystemExit('Mock rejected symlink escape: ' + value)
with (root / 'installer-calls.jsonl').open('a') as output:
    output.write(json.dumps({'command': name, 'args': args,
                             'paths': [str(pathlib.Path(p).resolve()) for p in paths]}) + '\n')
PY_MOCK
}
rm() { record_mock rm "$@"; }
ditto() { record_mock ditto "$@"; }
sudo() { record_mock sudo "$@" && "$@"; }
xattr() { record_mock xattr "$@"; }
open() { record_mock open "$@"; }
osascript() { record_mock osascript "$@"; }
sleep() { record_mock sleep "$@"; }
hdiutil() { return 97; }
curl() { return 97; }
uname() { if [[ "$1" == '-s' ]]; then printf 'Darwin\n'; else printf 'x86_64\n'; fi; }
lipo() { record_mock lipo "$@" && printf '%s\n' "${FIXTURE_BINARY_ARCHES:-x86_64 arm64}"; }
'''
        return mocks + (ROOT / 'release/cc-switch-update/install.sh').read_text().rsplit('\nmain "$@"', 1)[0]

    def fixture_app(self):
        app = self.home / 'fixture/CC Switch.app'
        (app / 'Contents/MacOS').mkdir(parents=True)
        (app / 'Contents/MacOS/cc-switch').write_text('fixture')
        return app

    def installer_calls(self):
        path = self.home / 'installer-calls.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def assert_confined_calls(self):
        for call in self.installer_calls():
            for value in call['paths']:
                self.assertTrue(Path(value).is_relative_to(self.home.resolve()), call)

    def test_mac_never_chooses_opposite_arch(self):
        metadata = self.home / 'metadata.json'
        for arch, opposite in [('x86_64', 'aarch64'), ('aarch64', 'x86_64')]:
            metadata.write_text(json.dumps({'installers': {f'darwin-{opposite}': {'url':'https://fixture.invalid/wrong.tar.gz'}}}))
            code = self.install_functions() + '''
resolve_from_release_assets() { return 1; }
curl() { local out; while [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then out="$2"; shift; fi; shift; done; cp "$FIXTURE" "$out"; }
resolve_from_metadata "$ARCH"
'''
            self.assertNotEqual(self.bash(code, FIXTURE=str(metadata), ARCH=arch).returncode, 0)

    def test_mac_checks_binary_before_removal(self):
        app = self.fixture_app()
        result = self.bash(self.install_functions() + '\ncopy_app "$FIXTURE_APP"\n',
                           FIXTURE_APP=str(app), FIXTURE_BINARY_ARCHES='arm64')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('安装包不支持当前 Mac 架构', result.stderr)
        self.assertFalse({'osascript', 'sleep', 'ditto', 'sudo', 'open', 'xattr'} &
                         {call['command'] for call in self.installer_calls()})
        self.assert_confined_calls()

    def test_mac_accepted_arch_uses_destination_seam_and_confined_mocks(self):
        app = self.fixture_app()
        code = self.install_functions() + '\ncopy_app "$FIXTURE_APP"\n'
        result = self.bash(code, FIXTURE_APP=str(app), FIXTURE_BINARY_ARCHES='x86_64')
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.installer_calls()
        destination = str((self.home / 'Applications/CC Switch.app').resolve())
        self.assertTrue(any(c['command'] == 'ditto' and c['paths'][-1] == destination for c in calls))
        self.assertTrue({'osascript','sleep','open','rm','ditto'} <= {c['command'] for c in calls})
        self.assertFalse(Path(destination).exists())  # ditto never executes for real.
        self.assert_confined_calls()

    def test_mac_sudo_branch_uses_confined_mocks(self):
        app = self.fixture_app()
        destination_dir = self.home / 'nonexistent-privileged'
        result = self.bash(self.install_functions() + '\ncopy_app "$FIXTURE_APP"\n',
                           FIXTURE_APP=str(app), CC_SWITCH_APP_DIR=str(destination_dir))
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.installer_calls()
        self.assertTrue(any(c['command'] == 'sudo' for c in calls))
        self.assertFalse(destination_dir.exists())
        self.assert_confined_calls()

    def test_mac_test_guard_rejects_missing_or_external_destination_before_setup(self):
        code = self.install_functions() + '\nprintf "guard-passed\\n"\n'
        for destination in ['', str(self.home.parent / (self.home.name + '-outside'))]:
            result = self.bash(code, CC_SWITCH_APP_DIR=destination)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('test guard:', result.stderr)
            self.assertNotIn('guard-passed', result.stdout)
            self.assertEqual(self.installer_calls(), [])
            self.assertEqual(list((self.home / 'tmp').iterdir()), [])

    def test_mac_test_guard_requires_home_and_auto_enables_when_home_present(self):
        result = self.bash(self.install_functions(), CC_SWITCH_TEST_HOME='')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('test guard:', result.stderr)
        self.assertEqual(self.installer_calls(), [])
        result = self.bash(self.install_functions(), CC_SWITCH_TEST_MODE='0',
                           CC_SWITCH_APP_DIR=str(self.home.parent / (self.home.name + '-outside')))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('test guard:', result.stderr)
        self.assertEqual(self.installer_calls(), [])

    def test_mac_test_guard_rejects_symlink_escape(self):
        outside = self.home.parent / (self.home.name + '-outside')
        link = self.home / 'escaped'; link.symlink_to(outside, target_is_directory=True)
        result = self.bash(self.install_functions(), CC_SWITCH_APP_DIR=str(link))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('test guard:', result.stderr)
        self.assertEqual(self.installer_calls(), [])

    def test_mac_test_guard_rejects_external_tmpdir(self):
        result = self.bash(self.install_functions(), TMPDIR=str(self.home.parent))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('test guard:', result.stderr)
        self.assertEqual(self.installer_calls(), [])

    def test_mac_copy_guard_rechecks_mutated_destination_before_process_control(self):
        app = self.fixture_app()
        outside = self.home.parent / (self.home.name + '-outside.app')
        code = self.install_functions() + '\nINSTALL_PATH="$OUTSIDE"\ncopy_app "$FIXTURE_APP"\n'
        result = self.bash(code, FIXTURE_APP=str(app), OUTSIDE=str(outside))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('test guard:', result.stderr)
        self.assertFalse({'osascript','sleep','ditto','sudo','open','xattr'} &
                         {call['command'] for call in self.installer_calls()})
        self.assert_confined_calls()

    def test_powershell_optional_fields_are_safe(self):
        text = (ROOT / 'release/cc-switch-update/install.ps1').read_text()
        self.assertNotRegex(text, r'\$installer\.(status|sha256|kind)')
        self.assertIn("Read-CCSwitchProperty -Object $installer -Name 'status'", text)

    def test_prepare_and_dispatch_mock_commands(self):
        self.mock_command('git', 'printf "%s\\n" "$*" >> "$CC_SWITCH_TEST_HOME/git-calls"')
        self.mock_command('gh', 'printf "%s\\n" "$*" >> "$CC_SWITCH_TEST_HOME/gh-calls"; exit "${GH_RESULT:-0}"')
        result = self.bash(workflow_step('upstream-sync.yml', 'Push preparation branch'), GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='2')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'git-calls').read_text().strip(), 'push origin HEAD:refs/heads/sync/upstream-123-2')
        code = workflow_step('upstream-sync.yml', 'Dispatch company release')
        self.assertEqual(self.bash(code, TAG='v4.0.10-company.2').returncode, 0)
        self.assertIn('workflow run company-release.yml --ref main -f tag=v4.0.10-company.2', (self.home / 'gh-calls').read_text())
        self.assertNotEqual(self.bash(code, TAG='v4.0.10-company.2', GH_RESULT='1').returncode, 0)

    def test_summary_reports_actual_step_results(self):
        code = workflow_step('upstream-sync.yml', 'Summarize outcome')
        expressions = {'steps.typecheck.outcome':'success', 'steps.unittests.outcome':'success',
                       'steps.cargocheck.outcome':'success', 'steps.merge.outcome':'success',
                       'steps.need.outputs.skip':'false', 'steps.versions.outputs.package_version':'4.0.10',
                       'steps.tag.outputs.tag':'v4.0.10-company.2', 'steps.versions.outputs.upstream_version':'4.0.10'}
        code = re.sub(r'\$\{\{\s*(.*?)\s*\}\}', lambda m: expressions[m[1]], code)
        good = dict(MODE='auto', PUSH='success', PUSH_TAG='success', DISPATCH='success', PREPARE='skipped', PR='skipped')
        for overrides, expected in [({}, 'release-dispatched'), ({'PUSH':'failure'}, 'push-failed'),
                                    ({'PUSH_TAG':'failure'}, 'tag-failed'), ({'DISPATCH':'failure'}, 'dispatch-failed'),
                                    ({'MODE':'prepare','PREPARE':'success','PR':'success'}, 'prepared'),
                                    ({'MODE':'prepare','PREPARE':'success','PR':'failure'}, 'prepare-failed')]:
            output = self.home / 'output'; output.unlink(missing_ok=True)
            result = self.bash(code, **dict(good, **overrides))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f'result={expected}\n', output.read_text())

    def test_tag_resolution_binds_iteration_and_sha(self):
        (self.home / 'package.json').write_text('{"version":"4.0.10"}')
        self.mock_command('git', 'if [ "$2" = HEAD ]; then echo fixture-sha; else echo "${TAG_SHA:-fixture-sha}"; fi')
        code = workflow_step('company-release.yml', 'Resolve release tag')
        for iteration in [1, 2, 10]:
            output = self.home / 'output'; output.unlink(missing_ok=True)
            result = self.bash(code, RELEASE_TAG=f'v4.0.10-company.{iteration}')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f'version=4.0.10-company.{iteration}\n', output.read_text())
            self.assertIn('build_sha=fixture-sha\n', output.read_text())
        for tag in ['v4.0.9-company.1', 'v4.0.10-company.0', 'v4.0.10-company.65536']:
            self.assertNotEqual(self.bash(code, RELEASE_TAG=tag).returncode, 0)
        self.assertNotEqual(self.bash(code, RELEASE_TAG='v4.0.10-company.2', TAG_SHA='moved-sha').returncode, 0)

    def test_runner_version_rewrites_only_fixture_app_package(self):
        (self.home / 'src-tauri').mkdir()
        (self.home / 'package.json').write_text('{"version":"4.0.10"}')
        (self.home / 'src-tauri/tauri.conf.json').write_text('{"version":"4.0.10","bundle":{"windows":{"wix":{"template":"keep.wxs"}}}}')
        (self.home / 'src-tauri/Cargo.toml').write_text('[package]\nname = "cc-switch"\nversion = "4.0.10"\n[dependencies]\nother = "1.2.3"\n')
        (self.home / 'src-tauri/Cargo.lock').write_text('[[package]]\nname = "cc-switch"\nversion = "4.0.10"\n\n[[package]]\nname = "other"\nversion = "1.2.3"\n')
        result = self.bash(workflow_step('company-release.yml', 'Prepare company build version'), VERSION='4.0.10-company.2', UPSTREAM_VERSION='4.0.10', COMPANY_ITERATION='2')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.home / 'package.json').read_text())['version'], '4.0.10-company.2')
        conf = json.loads((self.home / 'src-tauri/tauri.conf.json').read_text())
        self.assertEqual(conf['bundle']['windows']['wix'], {'template':'keep.wxs','version':'4.0.10.2'})
        self.assertIn('version = "4.0.10-company.2"', (self.home / 'src-tauri/Cargo.toml').read_text())
        self.assertIn('name = "other"\nversion = "1.2.3"', (self.home / 'src-tauri/Cargo.lock').read_text())

    def test_runner_version_accepts_windows_crlf_without_changing_dependencies(self):
        (self.home / 'src-tauri').mkdir()
        (self.home / 'package.json').write_text('{"version":"4.0.10"}')
        (self.home / 'src-tauri/tauri.conf.json').write_text('{"version":"4.0.10","bundle":{"windows":{"wix":{}}}}')
        toml = '[package]\r\nname = "cc-switch"\r\nversion = "4.0.10"\r\n'
        lock = '[[package]]\r\nname = "cc-switch"\r\nversion = "4.0.10"\r\n\r\n[[package]]\r\nname = "other"\r\nversion = "1.2.3"\r\n'
        (self.home / 'src-tauri/Cargo.toml').write_bytes(toml.encode())
        (self.home / 'src-tauri/Cargo.lock').write_bytes(lock.encode())
        result = self.bash(workflow_step('company-release.yml', 'Prepare company build version'), VERSION='4.0.10-company.2', UPSTREAM_VERSION='4.0.10', COMPANY_ITERATION='2')
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = lock.replace('version = "4.0.10"', 'version = "4.0.10-company.2"')
        self.assertEqual((self.home / 'src-tauri/Cargo.lock').read_bytes(), expected.encode())
        self.assertIn(b'version = "4.0.10-company.2"', (self.home / 'src-tauri/Cargo.toml').read_bytes())

    def test_publish_requires_successful_ci_for_release_commit(self):
        self.mock_command('gh', 'printf "%s\\n" "$*" >> "$CC_SWITCH_TEST_HOME/gh-calls"\nif [ "$2" = list ]; then echo "${CI_RUN_ID:-123}"; else exit "${CI_EXIT:-0}"; fi')
        code = workflow_step('company-release.yml', 'Require successful CI for release source')
        result = self.bash(code, BUILD_SHA='fixture-sha', GITHUB_REPOSITORY='fixture/repository')
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.home / 'gh-calls').read_text()
        self.assertIn('--commit fixture-sha --event push', calls)
        self.assertIn('run watch 123 --repo fixture/repository --exit-status', calls)
        self.assertNotEqual(self.bash(code, BUILD_SHA='fixture-sha', GITHUB_REPOSITORY='fixture/repository', CI_EXIT='1').returncode, 0)
        self.assertNotEqual(self.bash(code, BUILD_SHA='fixture-sha', GITHUB_REPOSITORY='fixture/repository', CI_RUN_ID=' ').returncode, 0)

    def test_windows_build_embeds_common_controls_for_tests_without_duplicate_app_resources(self):
        build = (ROOT / 'src-tauri/build.rs').read_text()
        self.assertIn('cargo:rustc-link-arg=/MANIFEST:EMBED', build)
        self.assertIn('/MANIFESTINPUT:', build)
        self.assertIn('cargo:rustc-link-arg-bins=/MANIFEST:NO', build)
        self.assertIn('CARGO_MANIFEST_DIR', build)
        manifest = (ROOT / 'src-tauri/common-controls.manifest').read_text()
        self.assertIn('Microsoft.Windows.Common-Controls', manifest)
        self.assertIn('version="6.0.0.0"', manifest)

    def test_manifest_fixture_contains_complete_company_identity(self):
        assets = self.home / 'release-assets'; assets.mkdir()
        for name in ['fixture-Windows-company-x64.msi', 'fixture-macOS-company-universal.tar.gz']:
            (assets / name).write_text('fixture payload')
            (assets / (name + '.sig')).write_text('fixture-signature')
        code = 'sha256sum() { printf "fixture-sha256  %s\\n" "$1"; }\n' + workflow_step('company-release.yml', 'Generate latest-company.json')
        result = self.bash(code, TAG='v4.0.10-company.2', VERSION='4.0.10-company.2', UPSTREAM_VERSION='4.0.10', COMPANY_ITERATION='2', BUILD_SHA='fixture-sha', REPO='fixture/repository')
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((assets / 'latest-company.json').read_text())
        self.assertEqual({k:manifest[k] for k in ['version','upstream_version','company_iteration','release_tag','build_sha']},
                         dict(version='4.0.10-company.2', upstream_version='4.0.10', company_iteration=2, release_tag='v4.0.10-company.2', build_sha='fixture-sha'))
        self.assertNotIn('status', manifest['installers']['windows-x86_64'])
        self.assertIn('install-v4.0.10-company.2.ps1', manifest['installers']['windows-x86_64']['script_url'])
        if shutil.which('pwsh'):
            script = f". '{ROOT / 'release/cc-switch-update/install.ps1'}'\n" + '''
function Get-CCSwitchLatestMetadata { return Get-Content "$env:CC_SWITCH_TEST_HOME/release-assets/latest-company.json" -Raw | ConvertFrom-Json }
$info = Get-CCSwitchInstallerInfo -Repo fixture/repository -Arch x86_64
if ($info.Status -ne 'ready' -or $info.Kind -ne 'msi') { throw 'Unexpected fixture installer info' }
'''
            result = subprocess.run(['pwsh','-NoProfile','-Command',script], cwd=self.home, env=self.env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def diagnostic(self, repair=False):
        codex = self.home / 'codex'; codex.mkdir(exist_ok=True)
        ccs = self.home / 'ccs'; ccs.mkdir(exist_ok=True)
        (codex / 'config.toml').write_text('model_provider = "custom"\n')
        return subprocess.run(['bash', str(ROOT / 'scripts/diagnose-codex-history-buckets.sh'), '--codex-dir', str(codex), '--cc-switch-dir', str(ccs)] + (['--repair'] if repair else []), env=self.env, text=True, capture_output=True)

    def test_two_db_backups_are_distinct(self):
        codex = self.home / 'codex'; (codex / 'sqlite').mkdir(parents=True)
        for relative, row in [('state_5.sqlite', 'root'), ('sqlite/state_5.sqlite','nested')]:
            with sqlite3.connect(codex / relative) as conn:
                conn.execute('CREATE TABLE threads(id TEXT, model_provider TEXT)')
                conn.execute('INSERT INTO threads VALUES (?, ?)', (row,'openai'))
        result = self.diagnostic(True)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        backups = list((self.home / 'ccs/backups').rglob('state_5.sqlite'))
        self.assertEqual(len(backups), 2)
        self.assertEqual({sqlite3.connect(p).execute('SELECT id, model_provider FROM threads').fetchone() for p in backups}, {('root','openai'),('nested','openai')})

    def test_invalid_history_and_db_are_nonzero(self):
        codex = self.home / 'codex'; (codex / 'sessions').mkdir(parents=True)
        (codex / 'sessions/rollout-bad.jsonl').write_text('{"type":"session_meta","payload":{"model_provider": invalid}}\n')
        (codex / 'state_5.sqlite').write_text('bad db')
        for repair in (False, True):
            result = self.diagnostic(repair)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('OK:', result.stdout)

    def diagnostic_namespace(self):
        self.diagnostic()
        code = (ROOT / 'scripts/diagnose-codex-history-buckets.sh').read_text().split("<<'PY'\n",1)[1].rsplit('\nPY',1)[0]
        ns = {}
        with patch('sys.argv', ['fixture','diagnose',str(self.home/'codex'),str(self.home/'ccs'),'custom','openai']):
            exec(compile(code, 'diagnostic-fixture', 'exec'), ns)
        return ns

    def test_active_jsonl_is_refused(self):
        ns = self.diagnostic_namespace()
        path = self.home / 'codex/sessions/rollout-live.jsonl'; path.parent.mkdir()
        path.write_text('{"type":"session_meta","payload":{"model_provider":"openai"}}\n')
        ns['has_open_handles'] = lambda _: True
        with self.assertRaisesRegex(Exception, 'active|open|changed'):
            ns['rewrite_jsonl_files'](self.home / 'backup')
        self.assertIn('openai', path.read_text())

    def test_append_during_backup_is_preserved(self):
        ns = self.diagnostic_namespace()
        path = self.home / 'codex/sessions/rollout-race.jsonl'; path.parent.mkdir()
        original = '{"type":"session_meta","payload":{"model_provider":"openai"}}\n'; path.write_text(original)
        ns['has_open_handles'] = lambda _: False
        real_copy = shutil.copy2
        def append_after_copy(src, dst):
            result = real_copy(src, dst)
            with Path(src).open('a') as f: f.write('{"type":"event_msg"}\n')
            return result
        with patch('shutil.copy2', append_after_copy):
            with self.assertRaisesRegex(Exception, 'changed|active'):
                ns['rewrite_jsonl_files'](self.home / 'backup')
        self.assertEqual(path.read_text(), original + '{"type":"event_msg"}\n')

    def test_open_handle_check_fails_closed(self):
        ns = self.diagnostic_namespace()
        path = self.home / 'codex/sessions/rollout-check.jsonl'
        with patch('shutil.which', return_value='/fixture/lsof'):
            with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 0, '123\n', '')):
                self.assertTrue(ns['has_open_handles'](path))
            with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 1, '', '')):
                self.assertFalse(ns['has_open_handles'](path))
            with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 2, '', 'failed')):
                with self.assertRaisesRegex(RuntimeError, 'Unable to check'):
                    ns['has_open_handles'](path)
        with patch('shutil.which', return_value=None):
            with self.assertRaisesRegex(RuntimeError, 'lsof is required'):
                ns['has_open_handles'](path)

    def test_jsonl_repair_backs_up_and_preserves_event_lines(self):
        self.mock_command('lsof', 'exit 1')
        path = self.home / 'codex/sessions/nested/rollout-fixture.jsonl'; path.parent.mkdir(parents=True)
        original = b'{"type":"session_meta","payload":{"model_provider":"openai"}}\r\n{"type":"event_msg","payload":{"text":"keep"}}\r\n'
        path.write_bytes(original)
        result = self.diagnostic(True)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        backups = list((self.home / 'ccs/backups').rglob('rollout-fixture.jsonl'))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), original)
        self.assertTrue(str(backups[0]).endswith('jsonl/sessions/nested/rollout-fixture.jsonl'))
        self.assertEqual(json.loads(path.read_bytes().splitlines()[0])['payload']['model_provider'], 'custom')
        self.assertTrue(path.read_bytes().endswith(original.split(b'\r\n',1)[1]))

    def test_active_jsonl_preflight_is_nonzero_and_unmodified(self):
        self.mock_command('lsof', 'echo 123; exit 0')
        path = self.home / 'codex/sessions/rollout-fixture.jsonl'; path.parent.mkdir(parents=True)
        original = '{"type":"session_meta","payload":{"model_provider":"openai"}}\n'; path.write_text(original)
        result = self.diagnostic(True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('active/open', result.stderr)
        self.assertEqual(path.read_text(), original)
        self.assertFalse((self.home / 'ccs/backups').exists())

if __name__ == '__main__':
    unittest.main()
