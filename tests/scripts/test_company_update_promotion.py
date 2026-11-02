"""Offline checks for the production update-manifest promotion script."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("company_promotion", ROOT / "scripts/publish-company-update.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
TAG = "v4.0.24-company.1"
COMMIT = "a" * 40


class PromotionTests(unittest.TestCase):
    def manifest(self):
        prefix = f"https://github.com/{MODULE.REPOSITORY}/releases/download/{TAG}/"
        platforms = {key: {"signature": "fixture", "url": prefix + key + ".pkg"}
                     for key in ("windows-x86_64", "darwin-aarch64", "darwin-x86_64")}
        return {"version": TAG[1:], "build_sha": COMMIT, "platforms": platforms,
                "installers": {key: {"url": entry["url"], "sha256": "b" * 64}
                               for key, entry in platforms.items()}}

    def test_only_release_settings_change_and_both_channels_align(self):
        original = "# retained\nOTHER_VALUE='unchanged'\nCC_SWITCH_STABLE_VERSION=4.0.10\nCC_SWITCH_STABLE_TAG=v4.0.10-company.4\nCC_SWITCH_CANARY_VERSION=3.20.2\nCC_SWITCH_CANARY_TAG=v3.20.2-company.2\n"
        changed = MODULE.updated_environment(original, TAG)
        self.assertTrue(changed.startswith("# retained\nOTHER_VALUE='unchanged'\n"))
        self.assertIn("CC_SWITCH_STABLE_VERSION=4.0.24\n", changed)
        self.assertIn(f"CC_SWITCH_STABLE_TAG={TAG}\n", changed)
        self.assertIn(f"CC_SWITCH_CANARY_TAG={TAG}\n", changed)
        self.assertEqual(MODULE.updated_environment(changed, TAG), changed)

    def test_missing_settings_append_without_joining_existing_line(self):
        changed = MODULE.updated_environment("UNRELATED=keep", TAG)
        self.assertTrue(changed.startswith("UNRELATED=keep\n"))
        self.assertEqual(len(changed.splitlines()), 5)

    def test_duplicate_settings_fail_before_writing(self):
        with self.assertRaises(ValueError):
            MODULE.updated_environment("CC_SWITCH_STABLE_TAG=one\nCC_SWITCH_STABLE_TAG=two\n", TAG)

    def test_invalid_release_tags_are_rejected(self):
        for tag in ("latest", "v4.0.24", "v4.0.24-company.0", "v4.0.24-company.65536", TAG + "\nOTHER=value"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                MODULE.release_version(tag)

    def test_all_signed_platforms_and_exact_build_required(self):
        data = self.manifest()
        MODULE.validate_manifest(data, TAG, COMMIT)
        for field, value in (("version", "4.0.10-company.4"), ("build_sha", "c" * 40)):
            invalid = dict(data, **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                MODULE.validate_manifest(invalid, TAG, COMMIT)
        for missing in ("windows-x86_64", "darwin-aarch64", "darwin-x86_64"):
            invalid = json.loads(json.dumps(data))
            del invalid["platforms"][missing]
            with self.subTest(missing=missing), self.assertRaises(ValueError):
                MODULE.validate_manifest(invalid, TAG, COMMIT)

    def test_unsigned_or_cross_release_artifacts_are_rejected(self):
        for field, value in (("signature", ""), ("url", "https://example.invalid/other")):
            data = self.manifest()
            data["platforms"]["darwin-aarch64"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                MODULE.validate_manifest(data, TAG, COMMIT)

    def test_atomic_replace_uses_specified_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.env"
            MODULE.atomic_replace(path, b"OTHER=before\n", 0o600)
            MODULE.atomic_replace(path, b"OTHER=after\n", 0o600)
            self.assertEqual(path.read_bytes(), b"OTHER=after\n")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(path.parent.iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
