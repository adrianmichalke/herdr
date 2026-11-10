import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts import install_pi_discord_preference as installer


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/fixtures/pi-discord-notify.ts"


class DiscordPreferenceInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="herdr-discord-installer-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.extension = self.root / "extensions/discord-notify.ts"
        self.extension.parent.mkdir()
        shutil.copyfile(FIXTURE, self.extension)
        self.original = self.extension.read_bytes()
        self.support = self.extension.parent / installer.SUPPORT_DIR

    def test_install_preserves_webhook_manual_commands_send_code_and_original_backup(self):
        installer.install(self.extension)
        result = self.extension.read_text(encoding="utf8")
        source = self.original.decode()
        self.assertIn('const FALLBACK_WEBHOOK_URL = "fixture-webhook";', result)
        send_code = source[source.index("async function sendDiscord"):source.index("function updateStatus")]
        self.assertIn(send_code, result)
        manual = source[source.index('\tpi.registerCommand("discord",'):source.index('\tpi.registerCommand("discord-notify",')]
        self.assertIn(manual, result)
        self.assertEqual(result[result.index('\tpi.registerTool({'):], source[source.index('\tpi.registerTool({'):])
        self.assertNotIn("appendEntry(", result)
        self.assertNotIn("getBranch()", result)
        self.assertEqual((self.support / "discord-notify.ts.original").read_bytes(), self.original)
        self.assertFalse((self.root / "preferences").exists())
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(self.extension.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE((self.support / "discord-notify.ts.original").stat().st_mode), 0o600)

    def test_repeat_install_is_idempotent_and_does_not_reset_preferences(self):
        installer.install(self.extension)
        previous = self.extension.stat().st_mtime_ns
        preferences = self.root / "preferences"
        preferences.mkdir()
        preference = preferences / "discord-notify.json"
        preference.write_text('{"enabled":true}\n', encoding="utf8")
        installer.install(self.extension)
        self.assertEqual(self.extension.stat().st_mtime_ns, previous)
        self.assertEqual(preference.read_text(encoding="utf8"), '{"enabled":true}\n')
        self.assertEqual((self.support / "discord-notify.ts.original").read_bytes(), self.original)

    def test_check_mode_does_not_write_any_files(self):
        self.assertEqual(installer.install(self.extension, check=True), self.extension)
        self.assertEqual(self.extension.read_bytes(), self.original)
        self.assertFalse(self.support.exists())

    def test_unrecognized_state_logic_is_left_untouched(self):
        source = self.original.decode().replace("return stateEntry?.data?.enabled === true;", "return true;")
        self.extension.write_text(source, encoding="utf8")
        with self.assertRaisesRegex(ValueError, "Unsupported Discord extension"):
            installer.install(self.extension)
        self.assertEqual(self.extension.read_text(encoding="utf8"), source)
        self.assertFalse(self.support.exists())

    def test_additional_legacy_state_references_are_rejected(self):
        source = self.original.decode("utf8") + "\nconst customRestore = restoreEnabled;\n"
        self.extension.write_text(source, encoding="utf8")
        with self.assertRaisesRegex(ValueError, "additional legacy"):
            installer.install(self.extension)
        self.assertEqual(self.extension.read_text(encoding="utf8"), source)
        self.assertFalse(self.support.exists())

    def test_unmanaged_companion_is_not_overwritten(self):
        self.support.mkdir()
        module = self.support / "global-boolean.ts"
        module.write_text("user-owned code\n", encoding="utf8")
        with self.assertRaisesRegex(ValueError, "unmanaged"):
            installer.install(self.extension)
        self.assertEqual(module.read_text(encoding="utf8"), "user-owned code\n")
        self.assertEqual(self.extension.read_bytes(), self.original)

    def test_concurrent_extension_edit_is_not_lost(self):
        original_write = installer.atomic_write
        changed = self.original + b"\nconst userEdit = true;\n"

        def write_with_edit(path, content, **kwargs):
            original_write(path, content, **kwargs)
            self.extension.write_bytes(changed)

        with patch.object(installer, "atomic_write", side_effect=write_with_edit):
            with self.assertRaisesRegex(ValueError, "changed during installation"):
                installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), changed)

    @unittest.skipUnless(os.name == "posix", "Unix symlink permissions")
    def test_symlinked_extensions_directory_keeps_its_target_and_companion_import(self):
        shared = self.root / "shared"
        self.extension.parent.rename(shared)
        (self.root / "extensions").symlink_to(shared, target_is_directory=True)
        installer.install(self.extension)
        self.assertTrue((self.root / "extensions").is_symlink())
        self.assertTrue((shared / installer.SUPPORT_DIR / "discord-notify.ts").is_file())
        self.assertTrue(self.extension.read_text(encoding="utf8").startswith(installer.IMPORT))

    def test_hardlinked_extension_is_rejected(self):
        try:
            os.link(self.extension, self.root / "linked.ts")
        except OSError as error:
            self.skipTest(str(error))
        with self.assertRaisesRegex(ValueError, "multiple hard links"):
            installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), self.original)

    def test_cli_uses_the_agent_directory_override_without_printing_source(self):
        environment = os.environ | {"PI_CODING_AGENT_DIR": str(self.root)}
        result = subprocess.run([sys.executable, str(ROOT / "scripts/install_pi_discord_preference.py"), "--check"], env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("fixture-webhook", result.stdout + result.stderr)
        self.assertFalse(self.support.exists())

    def test_failed_backup_is_not_published_and_retry_saves_the_full_original(self):
        with patch.object(installer.os, "fsync", side_effect=OSError("simulated write failure")):
            with self.assertRaises(OSError):
                installer.install(self.extension)
        backup = self.support / "discord-notify.ts.original"
        self.assertFalse(backup.exists())
        self.assertEqual(self.extension.read_bytes(), self.original)
        installer.install(self.extension)
        self.assertEqual(backup.read_bytes(), self.original)

    def test_stale_backup_is_not_silently_reused_after_a_conflicting_edit(self):
        self.support.mkdir()
        backup = self.support / "discord-notify.ts.original"
        backup.write_bytes(self.original)
        changed = self.original + b"\nconst userEdit = true;\n"
        self.extension.write_bytes(changed)
        with self.assertRaisesRegex(ValueError, "backup does not match"):
            installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), changed)
        self.assertEqual(backup.read_bytes(), self.original)

    def test_edit_during_final_file_flush_is_not_overwritten(self):
        original_write = installer.atomic_write
        original_sync = installer.os.fsync
        changed = self.original + b"\nconst lateUserEdit = true;\n"

        def late_sync(descriptor):
            original_sync(descriptor)
            self.extension.write_bytes(changed)

        def write_with_late_edit(path, content, **kwargs):
            if path == self.extension:
                with patch.object(installer.os, "fsync", side_effect=late_sync):
                    original_write(path, content, **kwargs)
            else:
                original_write(path, content, **kwargs)

        with patch.object(installer, "atomic_write", side_effect=write_with_late_edit):
            with self.assertRaisesRegex(ValueError, "changed during installation"):
                installer.install(self.extension)
        self.assertEqual(self.extension.read_bytes(), changed)

    def test_simultaneous_installers_are_rejected_without_changing_the_extension(self):
        self.support.mkdir()
        with installer.installation_lock(self.support):
            result = subprocess.run([sys.executable, str(ROOT / "scripts/install_pi_discord_preference.py"), "--extension", str(self.extension)], capture_output=True, encoding="utf8")
        self.assertEqual(result.returncode, 1)
        self.assertIn("installation is in progress", result.stderr)
        self.assertEqual(self.extension.read_bytes(), self.original)
        installer.install(self.extension)

    def test_typescript_preference_runtime(self):
        bun = shutil.which("bun")
        if not bun:
            self.skipTest("Bun is required for TypeScript integration tests")
        environment = os.environ | {"PI_TEST_PYTHON": sys.executable}
        result = subprocess.run([bun, "test", "src/integration/assets/pi/preferences/discord-notify.test.ts"], cwd=ROOT, env=environment, capture_output=True, encoding="utf8", timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
