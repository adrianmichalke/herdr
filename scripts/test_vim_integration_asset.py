import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ASSET = Path(__file__).resolve().parents[1] / "src/integration/assets/vim/herdr.vim"
EDITORS = list(dict.fromkeys(
    path for name in (os.environ.get("HERDR_TEST_VIM", "vim"), os.environ.get("HERDR_TEST_NVIM", "nvim"))
    if (path := shutil.which(name))
))


def vim_string(value):
    return json.dumps(str(value), ensure_ascii=False)


@unittest.skipUnless(os.name == "posix" and EDITORS, "Unix Vim or Neovim is required")
class VimIntegrationAssetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="herdr-vim-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.log = self.root / "reports.jsonl"
        self.binary = self.root / "herdr-reporter"
        self.binary.write_text(
            f"#!{sys.executable}\n"
            "import json, os, sys, time\n"
            "fd = os.open(os.environ['HERDR_TEST_LOG'], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)\n"
            "os.write(fd, (json.dumps(sys.argv[1:]) + '\\n').encode())\n"
            "os.close(fd)\n"
            "if os.environ.get('HERDR_TEST_HANG'): time.sleep(30)\n"
        )
        self.binary.chmod(0o700)
        self.environment = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
        self.environment.update({
            "HOME": str(self.root),
            "XDG_STATE_HOME": str(self.root / "state"),
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "NVIM_APPNAME": "nvim",
            "HERDR_ENV": "1",
            "HERDR_BIN_PATH": str(self.binary),
            "HERDR_PANE_ID": "test-pane",
            "HERDR_SOCKET_PATH": str(self.root / "test.sock"),
            "HERDR_TEST_LOG": str(self.log),
        })

    def run_editor(self, editor, script, resume=(), environment=None, installed=False, post_start=False):
        script_path = self.root / "commands.vim"
        if post_start:
            script = "function! HerdrTestRun(timer) abort\n" + script + "\nqa!\nendfunction\nautocmd VimEnter * call timer_start(10, function('HerdrTestRun'))\n"
        else:
            script += "\nqa!\n"
        script_path.write_text(script)
        version = subprocess.check_output([editor, "--version"], text=True)
        flags = ["--headless"] if "NVIM" in version else (["--not-a-term"] if post_start else ["-es"])
        initialization = ["-u", "NORC"] if installed else ["-u", "NONE", "--cmd", f"source {ASSET}"]
        command = [editor, *flags, *initialization, "-n", "-i", "NONE", *resume, "-S", str(script_path)]
        if post_start and "NVIM" not in version:
            master, slave = os.openpty()
            try:
                with subprocess.Popen(command, env=environment or self.environment, cwd=self.root, stdin=slave, stdout=slave, stderr=slave) as process:
                    try:
                        self.assertEqual(process.wait(timeout=10), 0)
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()
            finally:
                os.close(slave)
                os.close(master)
        else:
            result = subprocess.run(command, env=environment or self.environment, cwd=self.root, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def reports(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def session(self):
        reports = [report for report in self.reports() if report[1] == "report-agent"]
        self.assertTrue(reports, "no session reported")
        report = reports[-1]
        return report[report.index("--") + 1:]

    def create_layout(self, editor):
        files = [self.root / name for name in ("one file.txt", "two'λ.txt", "three.txt")]
        for file in files:
            file.write_text("\n".join(f"line {i}" for i in range(1, 21)) + "\n")
        kept_session = self.root / "user-session.vim"
        script = f"""
execute 'edit ' . fnameescape({vim_string(files[0])})
normal! 7G3|
execute 'vsplit ' . fnameescape({vim_string(files[1])})
normal! 5G2|
tabnew
execute 'edit ' . fnameescape({vim_string(files[2])})
normal! 9G2|
let &sessionoptions = 'buffers,curdir'
let v:this_session = {vim_string(kept_session)}
silent doautocmd VimEnter
sleep 400m
call writefile([json_encode([v:this_session, &sessionoptions])], {vim_string(self.root / 'preserved.json')})
"""
        self.run_editor(editor, script)
        self.assertEqual(json.loads((self.root / "preserved.json").read_text()), [str(kept_session), "buffers,curdir"])
        return files

    def test_checkpoint_restore_and_release(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                files = self.create_layout(editor)
                resume = self.session()
                self.assertEqual(resume[1], "-S")
                checkpoint = Path(resume[2])
                self.assertTrue(checkpoint.is_file())
                self.assertEqual(checkpoint.stat().st_mode & 0o777, 0o600)
                self.assertEqual(checkpoint.parent.stat().st_mode & 0o777, 0o700)
                reports = self.reports()
                self.assertEqual(reports[-1][1], "release-agent")
                sequences = [int(report[report.index("--seq") + 1]) for report in reports]
                self.assertEqual(sequences, sorted(set(sequences)))
                self.run_editor(editor, f"""
call writefile([json_encode(map(getwininfo(), {{_, win -> [fnamemodify(bufname(win.bufnr), ':p'), win.tabnr]}})), json_encode([line('.'), col('.')])], {vim_string(self.root / 'restored.json')})
silent doautocmd VimEnter
sleep 400m
""", resume[1:])
                layout, cursor = [json.loads(line) for line in (self.root / "restored.json").read_text().splitlines()]
                self.assertEqual(sorted(layout), sorted([[str(files[0]), 1], [str(files[1]), 1], [str(files[2]), 2]]))
                self.assertEqual(cursor, [9, 2])
                self.assertNotEqual(self.session(), resume, "each process must own a separate checkpoint")
                self.assertTrue(checkpoint.is_file(), "restore must preserve the original checkpoint")

    def test_outside_herdr_and_nested_editors_do_nothing(self):
        scope = hashlib.sha256((self.environment["HERDR_SOCKET_PATH"] + "\n" + self.environment["HERDR_PANE_ID"]).encode()).hexdigest()
        for editor in EDITORS:
            for overrides in ({"HERDR_ENV": ""}, {"HERDR_PANE_ID": ""}, {"HERDR_SOCKET_PATH": ""}, {"HERDR_VIM_ACTIVE": scope}, {"HERDR_BIN_PATH": str(self.root / "missing")}):
                with self.subTest(editor=editor, overrides=overrides):
                    self.run_editor(editor, "silent doautocmd VimEnter\nsleep 50m", environment=self.environment | overrides)
                    self.assertEqual(self.reports(), [])
        self.assertFalse((self.root / "state/herdr/vim").exists())

    def test_unwritable_checkpoint_is_not_reported(self):
        state = self.root / "state/herdr"
        state.mkdir(parents=True)
        (state / "vim").write_text("not a directory")
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.run_editor(editor, "silent doautocmd VimEnter\nsleep 100m")
                self.assertEqual(self.reports(), [])

    def test_failed_checkpoint_preserves_last_valid_snapshot_and_unsaved_file(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                original = self.root / "source.txt"
                original.write_text("saved\n")
                before = self.root / "before.vim"
                after = self.root / "after.vim"
                self.run_editor(editor, f"""
execute 'edit ' . fnameescape({vim_string(original)})
call setline(1, 'unsaved')
silent doautocmd VimEnter
sleep 400m
let snapshot = json_decode(readfile($HERDR_TEST_LOG)[-1])[-1]
call writefile(readfile(snapshot), {vim_string(before)})
call mkdir(snapshot . '.tmp')
split
silent doautocmd WinEnter
sleep 2300m
call writefile(readfile(snapshot), {vim_string(after)})
""")
                self.assertEqual(before.read_bytes(), after.read_bytes())
                self.assertEqual(original.read_text(), "saved\n")

    def test_apostrophe_in_state_directory_has_safe_resume_arguments(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                self.environment["XDG_STATE_HOME"] = str(self.root / "O'Neil state")
                self.create_layout(editor)
                resume = self.session()
                self.assertFalse(any("'" in arg or any(ord(char) < 32 for char in arg) for arg in resume))
                self.run_editor(editor, "silent doautocmd VimEnter\nsleep 400m", resume[1:])
                self.assertNotEqual(self.session(), resume)

    def test_distinct_pane_is_not_suppressed_by_inherited_marker(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                self.run_editor(editor, "silent doautocmd VimEnter\nsleep 200m", environment=self.environment | {"HERDR_VIM_ACTIVE": "another-pane"})
                self.assertTrue(self.reports())

    def test_installed_package_loads_without_modifying_editor_configuration(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                for root in (self.root / ".vim", self.root / "data/nvim/site"):
                    plugin = root / "pack/herdr/start/herdr/plugin/herdr.vim"
                    plugin.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ASSET, plugin)
                self.run_editor(editor, "silent doautocmd VimEnter\nsleep 300m", installed=True)
                self.assertTrue(self.reports())

    def test_checkpoint_does_not_trigger_user_optionset_handlers(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.run_editor(editor, f"""
let g:option_events = 0
augroup UserOptionTest
  autocmd!
  autocmd OptionSet sessionoptions let g:option_events += 1
augroup END
sleep 300m
split
sleep 2200m
call writefile([string(g:option_events)], {vim_string(self.root / 'option-events')})
""", post_start=True)
                self.assertEqual((self.root / "option-events").read_text().strip(), "0")

    def test_changes_after_initial_checkpoint_are_saved(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                hidden = self.root / "hidden.txt"
                hidden.write_text("hidden buffer\n")
                self.run_editor(editor, f"""
silent doautocmd VimEnter
sleep 300m
execute 'badd ' . fnameescape({vim_string(hidden)})
vsplit
vertical resize 25
sleep 2300m
call writefile([json_encode(map(getwininfo(), {{_, win -> win.width}}))], {vim_string(self.root / 'widths-before.json')})
""")
                resume = self.session()
                self.assertIn("hidden.txt", Path(resume[-1]).read_text())
                self.run_editor(editor, f"call writefile([json_encode(map(getwininfo(), {{_, win -> win.width}}))], {vim_string(self.root / 'widths-after.json')})", resume[1:])
                self.assertEqual((self.root / "widths-before.json").read_text(), (self.root / "widths-after.json").read_text())

    def test_unresponsive_reporter_does_not_block_exit(self):
        for editor in EDITORS:
            with self.subTest(editor=editor):
                self.log.unlink(missing_ok=True)
                self.run_editor(editor, "silent doautocmd VimEnter\nsleep 2200m", environment=self.environment | {"HERDR_TEST_HANG": "1"})
                self.assertTrue(self.reports())
                self.assertEqual(self.reports()[-1][1], "release-agent")


if __name__ == "__main__":
    unittest.main()
