#!/usr/bin/env python3
"""Package layout and installer regressions, without touching the user's setup."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRATCH = ROOT / ".cache" / "packaging-tests"


def run(*args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, **kwargs)


def executable(path, contents):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(contents)
    path.chmod(0o755)


class PackagingTest(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.cww = self.bin / "cww"
        executable(self.cww, '''#!/bin/sh
if [ "$1" = --version ]; then echo 'cww 9.8.7'; exit; fi
printf '%s\\n' "$*" >> "$TEST_CALLS"
exit "${TEST_DAEMON_EXIT:-0}"
''')
        executable(self.bin / "cww-app", "#!/bin/sh\necho 'Chat with Work 9.8.7'\n")
        self.env = dict(os.environ, TEST_CALLS=str(self.work / "calls"))

    def archive(self, target="x86_64-unknown-linux-gnu"):
        result = run("bash", str(ROOT / "packaging/linux/archive.sh"),
                     str(self.cww), str(self.bin / "cww-app"), "9.8.7", target,
                     str(self.work / "release"))
        self.assertEqual(result.returncode, 0, result.stderr)
        return self.work / "release" / f"cww-app-v9.8.7-{target}.tar.gz"

    def test_complete_archive_for_each_architecture(self):
        for target in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
            with self.subTest(target=target), tarfile.open(self.archive(target)) as archive:
                base = f"cww-app-v9.8.7-{target}/"
                for name in ("cww", "cww-app", "cww-app.desktop", "cww-app.svg",
                             "packaging/systemd/cww.service", "README.md",
                             "PROTOCOL.md", "SECURITY.md", "LICENSE-MIT", "LICENSE-APACHE"):
                    self.assertTrue(archive.getmember(base + name).isfile(), name)
                for name in ("cww", "cww-app"):
                    self.assertEqual(archive.getmember(base + name).mode, 0o755)

    def test_arch_package_functions_install_gui_cli_service_and_launcher(self):
        archive = self.archive()
        source = self.work / "src"
        source.mkdir()
        with tarfile.open(archive) as tar:
            tar.extractall(source, filter="data")
        for variant in ("chatwithwork-local-agent-bin", "chatwithwork-local-agent",
                        "chatwithwork-local-agent-git"):
            with self.subTest(variant=variant):
                recipe = ROOT / "packaging/arch" / variant / "PKGBUILD.in"
                tree = source / (variant if variant.endswith("-git") else f"{variant}-9.8.7")
                if not variant.endswith("-bin"):
                    shutil.copytree(source / "cww-app-v9.8.7-x86_64-unknown-linux-gnu", tree)
                    (tree / "target/release").mkdir(parents=True)
                    shutil.copy(self.cww, tree / "target/release/cww")
                    shutil.copy(self.bin / "cww-app", tree / "target/release/cww-app")
                    (tree / "packaging/linux").mkdir(parents=True)
                    shutil.copy(ROOT / "packaging/linux/cww-app.desktop", tree / "packaging/linux")
                    (tree / "app/assets").mkdir(parents=True)
                    shutil.copy(ROOT / "app/assets/mark.svg", tree / "app/assets")
                package = self.work / variant
                result = run("bash", "-ec", 'source "$1"; pkgver=9.8.7; package',
                             "test", str(recipe), env=dict(self.env, srcdir=str(source),
                             pkgdir=str(package), CARCH="x86_64"))
                self.assertEqual(result.returncode, 0, result.stderr)
                for name in ("usr/bin/cww", "usr/bin/cww-app",
                             "usr/lib/systemd/user/cww.service",
                             "usr/share/applications/cww-app.desktop",
                             "usr/share/icons/hicolor/scalable/apps/cww-app.svg"):
                    self.assertTrue((package / name).is_file(), name)

    def installer(self, *, daemon_exit=0, uid="1000", corrupt=False, unusual_path=False):
        archive = self.archive()
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        (archive.parent / "checksums.txt").write_text(f"{checksum}  {archive.name}\n")
        if corrupt:
            archive.write_bytes(b"broken download")
        mocks = self.work / "mocks"
        executable(mocks / "curl", '''#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    https://*) name=${1##*/} ;;
    -o) shift; output=$1 ;;
  esac
  shift
done
cp "$TEST_RELEASE/$name" "$output"
''')
        executable(mocks / "uname", '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        executable(mocks / "id", f"#!/bin/sh\necho {uid}\n")
        install = self.work / ('bin spaces $cash `tick` % "quote" \\ slash' if unusual_path else "installed")
        data = self.work / "data"
        env = dict(self.env, PATH=f"{mocks}:{os.environ['PATH']}",
                   CWW_DOWNLOAD_BASE="https://example.invalid/release",
                   CWW_INSTALL_DIR=str(install), XDG_DATA_HOME=str(data),
                   XDG_CACHE_HOME=str(self.work / "cache"),
                   TEST_RELEASE=str(archive.parent), TEST_DAEMON_EXIT=str(daemon_exit))
        result = run("sh", str(ROOT / "packaging/install.sh"), env=env)
        return result, install, data

    def test_installer_starts_daemon_and_installs_desktop_entry(self):
        result, install, data = self.installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(os.access(install / "cww-app", os.X_OK))
        self.assertTrue(os.access(install / "cww", os.X_OK))
        self.assertEqual((self.work / "calls").read_text(), "daemon install\n")
        self.assertIn("will start at every login", result.stdout)
        entry = (data / "applications/cww-app.desktop").read_text()
        self.assertIn(f'Exec="{install}/cww-app"', entry)
        self.assertTrue((data / "icons/hicolor/scalable/apps/cww-app.svg").is_file())

    def test_installer_reports_failed_service_setup_without_failing_install(self):
        result, install, _ = self.installer(daemon_exit=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((install / "cww-app").is_file())
        self.assertIn("could not be started", result.stdout)
        self.assertIn("daemon install", result.stdout)
        self.assertIn("daemon run", result.stdout)

    def test_root_installer_does_not_register_a_root_daemon(self):
        result, _, _ = self.installer(uid="0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.work / "calls").exists())
        self.assertIn("without sudo", result.stdout)

    def test_bad_checksum_does_not_install_or_start_anything(self):
        result, install, data = self.installer(corrupt=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse(install.exists())
        self.assertFalse(data.exists())
        self.assertFalse((self.work / "calls").exists())

    def test_launcher_handles_spaces_and_desktop_reserved_characters(self):
        result, install, data = self.installer(unusual_path=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        entry = (data / "applications/cww-app.desktop").read_text()
        # First decode Desktop Entry string escapes, then quoted Exec escapes
        # and percent field codes. The result must be precisely the binary path.
        command = next(line[5:] for line in entry.splitlines() if line.startswith("Exec="))
        command = command.replace("\\\\", "\\")
        self.assertTrue(command.startswith('"') and command.endswith('"'))
        command = command[1:-1]
        for char in ('"', '`', '$', '\\'):
            command = command.replace("\\" + char, char)
        self.assertEqual(command.replace("%%", "%"), str(install / "cww-app"))

    def test_linux_postinstall_and_arch_upgrade_explain_per_user_startup(self):
        for command in (("sh", str(ROOT / "packaging/linux/postinstall.sh")),
                        ("bash", "-ec", 'source "$1"; post_upgrade', "test",
                         str(ROOT / "packaging/arch/cww.install"))):
            result = run(*command)
            self.assertEqual(result.returncode, 0, result.stderr)
            for text in ("applications menu", "without sudo", "cww daemon install",
                         "systemctl --user restart", "cww daemon run"):
                self.assertIn(text, result.stdout)


if __name__ == "__main__":
    unittest.main()
