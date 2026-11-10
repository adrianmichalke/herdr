import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts import install_pi_fast_preference as installer


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures"


class FastPreferenceInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="herdr-fast-installer-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.extension = self.root / "package/pi-fast-mode-extension.js"
        self.extension.parent.mkdir()
        shutil.copyfile(FIXTURES / "pi-fast-mode-legacy.js", self.extension)
        self.original = self.extension.read_bytes()
        self.support = self.extension.parent / installer.shared.SUPPORT_DIR

    def test_migrates_the_existing_entry_point_without_creating_a_duplicate_command(self):
        installer.install(self.extension)
        self.assertEqual(self.extension.read_text(encoding="utf8"), installer.WRAPPER)
        self.assertEqual((self.support / "fast-mode.original").read_bytes(), self.original)
        for name in ("global-boolean.ts", "fast-mode.ts"):
            self.assertEqual((self.support / name).read_bytes(), (installer.shared.ASSET_DIR / name).read_bytes())
        self.assertFalse((self.root / "preferences").exists())
        self.assertEqual(sorted(path.name for path in self.extension.parent.iterdir()), [installer.shared.SUPPORT_DIR, self.extension.name])
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(self.extension.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE((self.support / "fast-mode.original").stat().st_mode), 0o600)

    def test_both_existing_implementations_and_windows_line_endings_are_supported(self):
        for name in ("pi-fast-mode-legacy.js", "pi-fast-mode-legacy-simple.js"):
            source = (FIXTURES / name).read_bytes().replace(b"\r\n", b"\n")
            installer.validate_source(source)
            installer.validate_source(source.replace(b"\n", b"\r\n"))

    def test_reinstall_preserves_preferences_original_backup_and_unrelated_discord_module(self):
        installer.install(self.extension)
        timestamp = self.extension.stat().st_mtime_ns
        preference = self.root / "preferences/fast-mode.json"
        preference.parent.mkdir()
        preference.write_text('{"enabled":true}\n', encoding="utf8")
        discord = self.support / "discord-notify.ts"
        discord.write_text("unrelated Discord module", encoding="utf8")
        installer.install(self.extension)
        self.assertEqual(self.extension.stat().st_mtime_ns, timestamp)
        self.assertEqual(preference.read_text(), '{"enabled":true}\n')
        self.assertEqual((self.support / "fast-mode.original").read_bytes(), self.original)
        self.assertEqual(discord.read_text(), "unrelated Discord module")

    def test_check_mode_performs_no_writes_and_does_not_print_source(self):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/install_pi_fast_preference.py"), "--extension", str(self.extension), "--check"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("PI_CODEX_FAST_MODE", result.stdout + result.stderr)
        self.assertEqual(self.extension.read_bytes(), self.original)
        self.assertFalse(self.support.exists())

    def test_unknown_or_customized_implementation_is_not_overwritten(self):
        changed = self.original + b"\nconst customState = true;\n"
        self.extension.write_bytes(changed)
        with self.assertRaisesRegex(ValueError, "Unsupported fast-mode"):
            installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), changed)
        self.assertFalse(self.support.exists())

    def test_unmanaged_companion_and_stale_backups_are_preserved(self):
        self.support.mkdir()
        module = self.support / "fast-mode.ts"
        module.write_text("custom module", encoding="utf8")
        with self.assertRaisesRegex(ValueError, "unmanaged"):
            installer.install(self.extension)
        self.assertEqual(module.read_text(), "custom module")
        module.unlink()
        (self.support / "fast-mode.original").write_bytes(b"another original")
        for check in (True, False):
            with self.assertRaisesRegex(ValueError, "backup does not match"):
                installer.install(self.extension, check=check)
        self.assertEqual(self.extension.read_bytes(), self.original)

    def test_failed_backup_and_concurrent_edits_do_not_replace_the_entry_point(self):
        with patch.object(installer.shared.os, "fsync", side_effect=OSError("write failure")):
            with self.assertRaises(OSError):
                installer.install(self.extension)
        self.assertFalse((self.support / "fast-mode.original").exists())
        self.assertEqual(self.extension.read_bytes(), self.original)
        write = installer.shared.atomic_write
        changed = self.original + b"\nconst concurrentEdit = true;\n"

        def edit_after_asset(path, content, **kwargs):
            write(path, content, **kwargs)
            self.extension.write_bytes(changed)

        with patch.object(installer.shared, "atomic_write", side_effect=edit_after_asset):
            with self.assertRaisesRegex(ValueError, "changed during installation"):
                installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), changed)

    def test_installers_are_serialized_and_shared_support_paths_are_protected(self):
        self.support.mkdir()
        with installer.shared.installation_lock(self.support):
            with self.assertRaisesRegex(ValueError, "installation is in progress"):
                installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), self.original)
        installer.install(self.extension)

    @unittest.skipUnless(os.name == "posix", "Unix links")
    def test_symlinked_entrypoint_uses_resolved_companion_path_but_hardlinks_are_rejected(self):
        alias = self.root / "linked.js"
        alias.symlink_to(self.extension)
        installer.install(alias)
        self.assertTrue(alias.is_symlink())
        self.assertTrue((self.support / "fast-mode.ts").is_file())
        os.link(self.extension, self.root / "hardlinked.js")
        with self.assertRaisesRegex(ValueError, "multiple hard links"):
            installer.install(self.extension)

    def test_typescript_preference_runtime(self):
        bun = shutil.which("bun")
        if not bun:
            self.skipTest("Bun is required for TypeScript integration tests")
        result = subprocess.run([bun, "test", "src/integration/assets/pi/preferences/fast-mode.test.ts"], cwd=ROOT, env=os.environ | {"PI_TEST_PYTHON": sys.executable}, capture_output=True, text=True, timeout=40)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
