import argparse
from contextlib import contextmanager
import os
from pathlib import Path
import re
import stat
import sys
import tempfile


ASSET_DIR = Path(__file__).resolve().parents[1] / "src/integration/assets/pi/preferences"
SUPPORT_DIR = ".herdr-pi-preferences"
IMPORT = 'import { DiscordNotificationPreference } from "./.herdr-pi-preferences/discord-notify.ts";\n'
MODULE_MARKER = "// HERDR_PI_PREFERENCES=1\n"

LEGACY_STATUS = '''function updateStatus(ctx: ExtensionContext, enabled: boolean): void {
	ctx.ui.setStatus("discord-notify", enabled ? ctx.ui.theme.fg("accent", "discord:on") : undefined);
}

function restoreEnabled(ctx: ExtensionContext): boolean {
	const stateEntry = ctx.sessionManager
		.getBranch()
		.filter((entry: { type: string; customType?: string }) => entry.type === "custom" && entry.customType === "discord-notify")
		.pop() as { data?: DiscordNotifyState } | undefined;
	return stateEntry?.data?.enabled === true;
}

'''
LEGACY_STATE = '''	let notificationsEnabled = false;

	function persistState() {
		pi.appendEntry("discord-notify", { enabled: notificationsEnabled });
	}

	function syncState(ctx: ExtensionContext) {
		notificationsEnabled = restoreEnabled(ctx);
		updateStatus(ctx, notificationsEnabled);
	}
'''
GLOBAL_STATE = '''	const preference = new DiscordNotificationPreference();
	let notificationsEnabled = false;

	function syncState(ctx: ExtensionContext) {
		notificationsEnabled = preference.sync(ctx);
	}
'''
PERSIST = '''			try {
				preference.set(notificationsEnabled);
			} catch {
				syncState(ctx);
				ctx.ui.notify("Could not save the global Discord notification preference; no change was applied", "error");
				return;
			}
			syncState(ctx);'''


def replace_once(source, old, new, label):
    if source.count(old) != 1:
        raise ValueError(f"Unsupported Discord extension: {label} does not match; no extension changes made")
    return source.replace(old, new, 1)


def patch_extension(source):
    if IMPORT in source:
        if GLOBAL_STATE not in source or 'preference.start(ctx)' not in source or 'preference.stop()' not in source:
            raise ValueError("Incomplete global Discord integration; inspect the extension before reinstalling")
        return source
    source = replace_once(source, 'type DiscordNotifyState = {\n\tenabled?: boolean;\n};\n\n', "", "legacy state type")
    source = replace_once(source, LEGACY_STATUS, "", "legacy state restore")
    source = replace_once(source, LEGACY_STATE, GLOBAL_STATE, "legacy persistence")
    source = replace_once(source, 'pi.on("session_start", async (_event, ctx) => {\n\t\tsyncState(ctx);', 'pi.on("session_start", async (_event, ctx) => {\n\t\tnotificationsEnabled = preference.start(ctx);', "startup handler")
    source = replace_once(source, '\t\t\tconst action = args.trim().toLowerCase();', '\t\t\tsyncState(ctx);\n\t\t\tconst action = args.trim().toLowerCase();', "command handler")
    source = replace_once(source, '\t\t\t\tupdateStatus(ctx, notificationsEnabled);', '\t\t\t\tsyncState(ctx);', "status refresh")
    source = replace_once(source, '\t\t\tpersistState();\n\t\t\tupdateStatus(ctx, notificationsEnabled);', PERSIST, "command persistence")
    source = replace_once(source, 'notificationsEnabled ? "Discord notifications are enabled" : "Discord notifications are disabled"', 'preference.describe()', "status feedback")
    source = replace_once(source, 'notificationsEnabled ? "Discord notifications enabled" : "Discord notifications disabled"', 'preference.describe()', "command feedback")
    source = replace_once(source, 'description: "Enable or disable automatic Discord notifications"', 'description: "Control the global automatic Discord notification preference"', "command description")
    source = replace_once(source, 'pi.on("agent_end", async (_event, ctx) => {\n\t\tif (!notificationsEnabled) return;', 'pi.on("agent_end", async (_event, ctx) => {\n\t\tsyncState(ctx);\n\t\tif (!notificationsEnabled) return;', "notification gate")
    source = replace_once(source, '\tpi.registerTool({\n\t\tname: "discord_send",', '\tpi.on("session_shutdown", async () => {\n\t\tpreference.stop();\n\t});\n\n\tpi.registerTool({\n\t\tname: "discord_send",', "shutdown cleanup")
    if re.search(r"\b(?:restoreEnabled|persistState|updateStatus)\b", source):
        raise ValueError("Unsupported additional legacy Discord state references; no extension changes made")
    return IMPORT + source


@contextmanager
def installation_lock(support, label="Discord"):
    path = support / ".install.lock"
    if path.is_symlink() or (path.exists() and (not path.is_file() or path.stat().st_nlink != 1)):
        raise ValueError(f"Invalid {label} preference installer lock")
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        try:
            if os.name == "nt":
                import msvcrt
                if os.fstat(descriptor).st_size == 0:
                    os.write(descriptor, b"\0")
                os.lseek(descriptor, 0, os.SEEK_SET)
                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise ValueError(f"Another {label} preference installation is in progress; retry when it finishes") from None
        yield
    finally:
        os.close(descriptor)


def atomic_write(path, content, expected=None, label="Discord"):
    descriptor, temporary = tempfile.mkstemp(prefix=".herdr-preference-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as file:
            file.write(content)
            file.flush()
            os.fsync(file.fileno())
        if expected is not None:
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or path.read_bytes() != expected:
                raise ValueError(f"{label} extension changed during installation; retry after the other edit finishes")
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def install(extension, check=False):
    target = extension.expanduser().resolve(strict=True)
    metadata = target.stat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or metadata.st_size > 1024 * 1024:
        raise ValueError("Discord extension must be a regular file without multiple hard links")
    original = target.read_bytes()
    patched = patch_extension(original.decode("utf8")).encode("utf8")
    support = target.parent / SUPPORT_DIR
    if support.is_symlink() or (support.exists() and not support.is_dir()):
        raise ValueError("Preference companion directory must not be a symlink or a file")
    assets = {name: (ASSET_DIR / name).read_bytes() for name in ("global-boolean.ts", "discord-notify.ts")}
    for name in assets:
        path = support / name
        if path.is_symlink() or (path.exists() and (not path.is_file() or not path.read_text(encoding="utf8").startswith(MODULE_MARKER))):
            raise ValueError(f"Refusing to replace an unmanaged companion module: {name}")
    backup = support / "discord-notify.ts.original"
    if backup.is_symlink() or (backup.exists() and (not backup.is_file() or backup.stat().st_nlink != 1)):
        raise ValueError("Discord extension backup must be a regular file without multiple hard links")
    if check:
        return target
    support.mkdir(mode=0o700, exist_ok=True)
    with installation_lock(support):
        original = target.read_bytes()
        patched = patch_extension(original.decode("utf8")).encode("utf8")
        if patched != original:
            if backup.exists() and backup.read_bytes() != original:
                raise ValueError("Existing Discord backup does not match the unpatched extension; inspect the private backup before retrying")
            if not backup.exists():
                atomic_write(backup, original)
        for name, content in assets.items():
            path = support / name
            if not path.exists() or path.read_bytes() != content:
                atomic_write(path, content)
        if patched != original:
            atomic_write(target, patched, expected=original)
    return target


def main():
    parser = argparse.ArgumentParser(description="Use a global preference in the existing Pi Discord extension without replacing its webhook or send functions")
    parser.add_argument("--agent-dir", type=Path, default=Path(os.environ.get("PI_CODING_AGENT_DIR") or Path.home() / ".pi/agent"))
    parser.add_argument("--extension", type=Path)
    parser.add_argument("--check", action="store_true", help="validate without changing files")
    args = parser.parse_args()
    extension = args.extension or args.agent_dir.expanduser() / "extensions/discord-notify.ts"
    try:
        target = install(extension, args.check)
    except (OSError, UnicodeError, ValueError) as error:
        if isinstance(error, (UnicodeError, OSError)):
            print("Could not access or update the Discord extension; verify its path and permissions", file=sys.stderr)
        else:
            print(str(error), file=sys.stderr)
        return 1
    print(f"{'Validated' if args.check else 'Installed global Discord preference support for'} {target}")
    if not args.check:
        print("Reload the extension once in existing Pi sessions. Later preference changes synchronize without reloading.")
        print("Automatic notifications default to off. Existing webhook configuration and manual sends are unchanged.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
