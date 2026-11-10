import argparse
import hashlib
from pathlib import Path
import stat
import sys

if __package__:
    from . import install_pi_discord_preference as shared
else:
    import install_pi_discord_preference as shared


WRAPPER = shared.MODULE_MARKER + 'export { default } from "./.herdr-pi-preferences/fast-mode.ts";\n'
LEGACY_DIGESTS = {
    "ba15ee7b01e25383dd8be084a8c5b9d7d70013170c0b11257d64425209f1e2db",
    "1dc8c6c7d71775323e8581797ac611dec5eddf5fa3942a00ca36ae3024b074dc",
}


def validate_source(source):
    source = source.replace(b"\r\n", b"\n")
    if source == WRAPPER.encode("utf8"):
        return
    if hashlib.sha256(source).hexdigest() not in LEGACY_DIGESTS:
        raise ValueError("Unsupported fast-mode extension; no extension changes made. Preserve custom code and migrate it manually")


def validate_backup(backup, original):
    if backup.is_symlink() or (backup.exists() and (not backup.is_file() or backup.stat().st_nlink != 1)):
        raise ValueError("Fast-mode backup must be a regular file without multiple hard links")
    if original != WRAPPER.encode("utf8") and backup.exists() and backup.read_bytes() != original:
        raise ValueError("Existing fast-mode backup does not match the unpatched extension; inspect the private backup before retrying")


def install(extension, check=False):
    target = extension.expanduser().resolve(strict=True)
    metadata = target.stat()
    if target.suffix not in (".js", ".ts") or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or metadata.st_size > 1024 * 1024:
        raise ValueError("Fast-mode extension must be a regular JavaScript or TypeScript file without multiple hard links")
    original = target.read_bytes()
    validate_source(original)
    support = target.parent / shared.SUPPORT_DIR
    if support.is_symlink() or (support.exists() and not support.is_dir()):
        raise ValueError("Preference companion directory must not be a symlink or a file")
    assets = {name: (shared.ASSET_DIR / name).read_bytes() for name in ("global-boolean.ts", "fast-mode.ts")}
    for name in assets:
        path = support / name
        if path.is_symlink() or (path.exists() and (not path.is_file() or not path.read_text(encoding="utf8").startswith(shared.MODULE_MARKER))):
            raise ValueError(f"Refusing to replace an unmanaged companion module: {name}")
    backup = support / "fast-mode.original"
    validate_backup(backup, original)
    if check:
        return target
    support.mkdir(mode=0o700, exist_ok=True)
    with shared.installation_lock(support, label="Fast-mode"):
        original = target.read_bytes()
        validate_source(original)
        validate_backup(backup, original)
        patched = WRAPPER.encode("utf8")
        if original != patched:
            if not backup.exists():
                shared.atomic_write(backup, original, label="Fast-mode")
        for name, content in assets.items():
            path = support / name
            if not path.exists() or path.read_bytes() != content:
                shared.atomic_write(path, content, label="Fast-mode")
        if original != patched:
            shared.atomic_write(target, patched, expected=original, label="Fast-mode")
    return target


def main():
    parser = argparse.ArgumentParser(description="Migrate an existing Pi fast-mode extension to the global opt-in preference without adding a duplicate /fast command")
    parser.add_argument("--extension", type=Path, required=True, help="existing Pi fast-mode entry point, including one loaded from a Pi package")
    parser.add_argument("--check", action="store_true", help="validate without changing files")
    args = parser.parse_args()
    try:
        target = install(args.extension, args.check)
    except (OSError, UnicodeError, ValueError) as error:
        if isinstance(error, (OSError, UnicodeError)):
            print("Could not access or update the fast-mode extension; verify its path and permissions", file=sys.stderr)
        else:
            print(str(error), file=sys.stderr)
        return 1
    print(f"{'Validated' if args.check else 'Installed global fast-mode preference support for'} {target}")
    if not args.check:
        print("Reload the extension once in existing Pi sessions. Later changes synchronize without restarting agents.")
        print("Fast mode defaults off. /fast on requests the priority tier; higher pricing may apply. No request was sent.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
