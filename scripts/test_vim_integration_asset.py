from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ASSET = Path(__file__).resolve().parents[1] / "src/integration/assets/vim/herdr.vim"
BACKEND = os.environ.get("HERDR_TEST_BACKEND")
if BACKEND is None:
    candidate = ASSET.parents[4] / "target/debug/herdr"
    BACKEND = str(candidate) if candidate.is_file() else ""
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
            "import fcntl, json, os, stat, sys, time\n"
            "from pathlib import Path\n"
            "if sys.argv[1:3] == ['integration', 'vim-swap-commit'] and os.environ.get('HERDR_TEST_BACKEND'):\n"
            "    os.execv(os.environ['HERDR_TEST_BACKEND'], [os.environ['HERDR_TEST_BACKEND'], *sys.argv[1:]])\n"
            "if sys.argv[1:3] == ['integration', 'vim-process-status']:\n"
            "    if os.environ.get('HERDR_TEST_PROBE_HANG'): time.sleep(30)\n"
            "    if os.environ.get('HERDR_TEST_PROBE_COUNTER'):\n"
            "        counter = Path(os.environ['HERDR_TEST_PROBE_COUNTER'])\n"
            "        count = int(counter.read_text()) + 1 if counter.exists() else 1\n"
            "        counter.write_text(str(count))\n"
            "        if count == 2:\n"
            "            Path(os.environ['HERDR_TEST_PROBE_READY']).write_text('ready')\n"
            "            deadline = time.monotonic() + 3\n"
            "            while not Path(os.environ['HERDR_TEST_PROBE_RELEASE']).exists() and time.monotonic() < deadline: time.sleep(0.01)\n"
            "    try: os.kill(int(sys.argv[3]), 0)\n"
            "    except ProcessLookupError: print('exited')\n"
            "    except PermissionError: print('unknown')\n"
            "    else: print('running')\n"
            "    sys.exit(0)\n"
            "if sys.argv[1:3] == ['integration', 'vim-swap-commit']:\n"
            "    if os.environ.get('HERDR_TEST_COMMIT_HANG'): time.sleep(30)\n"
            "    original, archive, pid = sys.argv[3:6]\n"
            "    try:\n"
            "        with open(original, 'rb') as source:\n"
            "            try: fcntl.flock(source, fcntl.LOCK_EX | fcntl.LOCK_NB)\n"
            "            except BlockingIOError: print('busy'); sys.exit(0)\n"
            "            pinned, current = os.fstat(source.fileno()), os.lstat(original)\n"
            "            same = lambda a, b: (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)\n"
            "            if not stat.S_ISREG(current.st_mode) or current.st_uid != os.geteuid() or not same(pinned, current): print('changed'); sys.exit(0)\n"
            "            if os.path.samefile(original, archive): print('unknown'); sys.exit(0)\n"
            "            observed = source.read()\n"
            "            if not observed.startswith(b'b0VIM ') or observed != Path(archive).read_bytes(): print('changed'); sys.exit(0)\n"
            "            if int(pid) > 0:\n"
            "                try: os.kill(int(pid), 0)\n"
            "                except ProcessLookupError: pass\n"
            "                else: print('changed'); sys.exit(0)\n"
            "            if not same(pinned, os.lstat(original)): print('changed'); sys.exit(0)\n"
            "            os.unlink(original)\n"
            "            print('archived')\n"
            "    except FileNotFoundError: print('changed')\n"
            "    sys.exit(0)\n"
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
            "HERDR_TEST_BACKEND": BACKEND,
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
        swap_dir = self.root / "swap"
        swap_dir.mkdir(exist_ok=True)
        command = [editor, *flags, *initialization, "-i", "NONE", "--cmd", "let &directory = " + vim_string(str(swap_dir) + "//"), *resume, "-S", str(script_path)]
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

    @contextmanager
    def swap_owner(self, editor, dirty=True):
        source = self.root / f"swap-source-{time.time_ns()}.txt"
        source.write_text("saved\n")
        swap_dir = self.root / "swap"
        swap_dir.mkdir(exist_ok=True)
        ready = self.root / "swap-ready"
        ready.unlink(missing_ok=True)
        script = self.root / "swap-owner.vim"
        script.write_text(f"""
execute 'edit ' . fnameescape({vim_string(source)})
{'call setline(1, "unsaved")' if dirty else ''}
preserve
call writefile([swapname('%')], {vim_string(ready)})
sleep 60000m
""")
        version = subprocess.check_output([editor, "--version"], text=True)
        flags = ["--headless"] if "NVIM" in version else ["-es"]
        environment = self.environment | {"HERDR_ENV": "0"}
        with (self.root / "swap-owner.log").open("w") as output:
            process = subprocess.Popen([editor, *flags, "-u", "NONE", "-i", "NONE", "--cmd", "let &directory = " + vim_string(str(swap_dir) + "//"), "-S", str(script)], env=environment, cwd=self.root, stdout=output, stderr=output)
            try:
                deadline = time.monotonic() + 5
                while not (ready.exists() and ready.stat().st_size > 0):
                    self.assertIsNone(process.poll(), "swap fixture exited before preserving data")
                    self.assertLess(time.monotonic(), deadline, "swap fixture did not become ready")
                    time.sleep(0.02)
                swap = Path(ready.read_text().strip())
                self.assertTrue(swap.is_file())
                yield source, swap, process
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)

    def archives(self):
        return set((self.root / "state/herdr/vim/recovery").glob("*/recovery.swp"))

    def test_stale_swaps_are_preserved_without_recovery_prompt(self):
        for editor in EDITORS:
            for dirty in (False, True):
                with self.subTest(editor=editor, dirty=dirty), self.swap_owner(editor, dirty) as (source, swap, process):
                    process.send_signal(signal.SIGKILL)
                    process.wait(timeout=5)
                    original = swap.read_bytes()
                    previous = self.archives()
                    opened = self.root / "opened.json"
                    self.run_editor(editor, f"""
execute 'edit ' . fnameescape({vim_string(source)})
call writefile([json_encode([getline(1, '$'), &modified, &swapfile])], {vim_string(opened)})
call writefile(split(execute('HerdrRecoveries'), "\\n"), {vim_string(self.root / 'recoveries-list')})
sleep 200m
""", post_start=True)
                    self.assertEqual(json.loads(opened.read_text()), [["saved"], 0, 1])
                    self.assertEqual(source.read_text(), "saved\n")
                    self.assertFalse(swap.exists())
                    added = self.archives() - previous
                    if not dirty and not added:
                        continue
                    self.assertEqual(len(added), 1)
                    archive = added.pop()
                    self.assertEqual(archive.read_bytes(), original)
                    self.assertIn(str(archive), (self.root / "recoveries-list").read_text())
                    self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
                    self.assertEqual(archive.parent.stat().st_mode & 0o777, 0o700)
                    record = json.loads((archive.parent / "info.json").read_text())
                    self.assertEqual(record["file"], str(source))
                    self.assertEqual(record["info"]["dirty"], int(dirty))
                    if dirty:
                        recovered = self.root / "recovered.json"
                        self.run_editor(editor, f"call writefile([json_encode(getline(1, '$'))], {vim_string(recovered)})", ["-r", str(archive)], environment=self.environment | {"HERDR_ENV": "0"})
                        self.assertEqual(json.loads(recovered.read_text()), ["unsaved"])
                        self.assertEqual(source.read_text(), "saved\n")

    def test_installed_package_handles_a_swap_before_opening_the_first_file(self):
        for root in (self.root / ".vim", self.root / "data/nvim/site"):
            plugin = root / "pack/herdr/start/herdr/plugin/herdr.vim"
            plugin.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ASSET, plugin)
        for editor in EDITORS:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                process.kill()
                process.wait(timeout=5)
                original = swap.read_bytes()
                previous = self.archives()
                opened = self.root / "first-file.json"
                self.run_editor(editor, f"call writefile([json_encode([expand('%:p'), getline(1, '$')])], {vim_string(opened)})", [str(source)], installed=True)
                self.assertEqual(json.loads(opened.read_text()), [str(source), ["saved"]])
                added = self.archives() - previous
                self.assertEqual(len(added), 1)
                self.assertEqual(added.pop().read_bytes(), original)
                self.assertFalse(swap.exists())

    def test_swap_handler_leaves_live_editors_alone(self):
        for editor in EDITORS:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                previous = self.archives()
                decision = self.root / "swap-decision"
                self.run_editor(editor, f"""
autocmd SwapExists * call writefile([v:swapchoice], {vim_string(decision)}) | let v:swapchoice = 'q'
execute 'edit ' . fnameescape({vim_string(source)})
""")
                self.assertNotEqual(decision.read_text().strip(), "d")
                self.assertIsNone(process.poll())
                self.assertTrue(swap.is_file())
                self.assertEqual(self.archives(), previous)

    def test_failed_archive_preserves_original_swap(self):
        blocked = self.root / "state/herdr/vim/recovery"
        blocked.parent.mkdir(parents=True)
        blocked.write_text("not a directory")
        for editor in EDITORS:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                process.kill()
                process.wait(timeout=5)
                original = swap.read_bytes()
                decision = self.root / "swap-decision"
                self.run_editor(editor, f"""
autocmd SwapExists * call writefile([v:swapchoice], {vim_string(decision)}) | let v:swapchoice = 'q'
execute 'edit ' . fnameescape({vim_string(source)})
""")
                self.assertNotEqual(decision.read_text().strip(), "d")
                self.assertEqual(swap.read_bytes(), original)
                self.assertEqual(blocked.read_text(), "not a directory")

    def test_swap_handler_does_not_archive_foreign_host_recovery_data(self):
        for editor in EDITORS:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                process.kill()
                process.wait(timeout=5)
                hostname = os.uname().nodename.encode()
                original = swap.read_bytes()
                self.assertEqual(original.count(hostname + b"\0"), 1)
                foreign = original.replace(hostname + b"\0", b"x" * len(hostname) + b"\0")
                swap.write_bytes(foreign)
                previous = self.archives()
                decision = self.root / "swap-decision"
                self.run_editor(editor, f"""
autocmd SwapExists * call writefile([v:swapchoice], {vim_string(decision)}) | let v:swapchoice = 'q'
execute 'edit ' . fnameescape({vim_string(source)})
""")
                self.assertNotEqual(decision.read_text().strip(), "d")
                self.assertEqual(swap.read_bytes(), foreign)
                self.assertEqual(self.archives(), previous)

    def test_stale_swap_outside_herdr_is_not_archived(self):
        for editor in EDITORS:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                process.kill()
                process.wait(timeout=5)
                original = swap.read_bytes()
                previous = self.archives()
                self.run_editor(editor, f"""
autocmd SwapExists * let v:swapchoice = 'q'
execute 'edit ' . fnameescape({vim_string(source)})
""", environment=self.environment | {"HERDR_ENV": "0"})
                self.assertEqual(swap.read_bytes(), original)
                self.assertEqual(self.archives(), previous)

    def test_unresponsive_process_probe_preserves_swap_and_does_not_hang_vim(self):
        editors = [editor for editor in EDITORS if "NVIM" not in subprocess.check_output([editor, "--version"], text=True)]
        if not editors:
            self.skipTest("requires Vim's process-liveness helper")
        commands = self.root / "bin"
        commands.mkdir()
        (commands / "herdr").symlink_to(self.binary)
        for editor in editors:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, process):
                process.kill()
                process.wait(timeout=5)
                original = swap.read_bytes()
                decision = self.root / "swap-decision"
                self.run_editor(editor, f"""
autocmd SwapExists * call writefile([v:swapchoice], {vim_string(decision)}) | let v:swapchoice = 'q'
execute 'edit ' . fnameescape({vim_string(source)})
""", environment=self.environment | {"HERDR_TEST_PROBE_HANG": "1", "PATH": str(commands)})
                self.assertNotEqual(decision.read_text().strip(), "d")
                self.assertEqual(swap.read_bytes(), original)
                self.assertFalse(self.archives())

    def test_concurrent_reopen_never_removes_another_editors_live_swap(self):
        editors = [editor for editor in EDITORS if "NVIM" not in subprocess.check_output([editor, "--version"], text=True)]
        if not editors:
            self.skipTest("requires Vim's process-liveness helper")
        for editor in editors:
            with self.subTest(editor=editor), self.swap_owner(editor) as (source, swap, owner):
                owner.kill()
                owner.wait(timeout=5)
                pending = self.root / "probe-pending"
                release = self.root / "probe-release"
                children = []

                def launch(tag, delayed=False):
                    script = self.root / f"open-{tag}.vim"
                    ready = self.root / f"ready-{tag}"
                    script.write_text(f"""
autocmd SwapExists * if empty(v:swapchoice) | let v:swapchoice = 'e' | endif
execute 'edit ' . fnameescape({vim_string(source)})
call setline(1, 'unsaved editor {tag}')
preserve
call writefile([swapname('%')], {vim_string(ready)})
sleep 60000m
""")
                    environment = self.environment.copy()
                    if delayed:
                        environment.update(HERDR_TEST_PROBE_COUNTER=str(self.root / "probe-counter"), HERDR_TEST_PROBE_READY=str(pending), HERDR_TEST_PROBE_RELEASE=str(release))
                    with (self.root / f"open-{tag}.log").open("w") as output:
                        process = subprocess.Popen([editor, "-es", "-u", "NONE", "-i", "NONE", "--cmd", "source " + str(ASSET), "--cmd", "let &directory = " + vim_string(str(self.root / "swap") + "//"), "-S", str(script)], env=environment, cwd=self.root, stdout=output, stderr=output)
                    children.append(process)
                    return ready

                try:
                    ready_b = launch("B", delayed=True)
                    deadline = time.monotonic() + 3
                    while not pending.exists():
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(0.01)
                    ready_a = launch("A")
                    deadline = time.monotonic() + 0.35
                    while not (ready_a.exists() and ready_a.stat().st_size > 0) and time.monotonic() < deadline:
                        time.sleep(0.01)
                    release.write_text("continue")
                    deadline = time.monotonic() + 5
                    while not all(path.exists() and path.stat().st_size > 0 for path in (ready_a, ready_b)):
                        self.assertLess(time.monotonic(), deadline)
                        self.assertTrue(all(process.poll() is None for process in children))
                        time.sleep(0.01)
                    for tag, ready in (("A", ready_a), ("B", ready_b)):
                        active = Path(ready.read_text().strip())
                        self.assertTrue(active.exists(), f"editor {tag}'s live swap was removed")
                        self.assertTrue(f"unsaved editor {tag}".encode() in active.read_bytes(), f"editor {tag}'s live swap was replaced")
                    self.assertEqual(source.read_text(), "saved\n")
                finally:
                    release.touch()
                    for process in children:
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=5)

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
