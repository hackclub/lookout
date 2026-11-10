#!/usr/bin/env python3
"""Rehearse the Tauri -> Lookout Native takeover on a CI runner.

Installs a build of the Tauri app (v0.3.14 rebuilt with a local updater
endpoint and a throwaway key), signs in through a lookout:// link, lets the
Tauri updater find and download the native build from a local latest.json,
quits, relaunches (0.3.14 auto-installs an update downloaded in an earlier
session), and checks that Lookout Native replaced it and kept the sign-in.

Usage: takeover_rehearsal.py --tauri <setup.exe | Lookout.app> --native <artifact>
         --tool <lookout-update-tool> --key-dir <dir with test.key> --out <dir>
Exits non-zero when a required check fails. Everything it saw goes to --out.
"""

import argparse
import base64
import hashlib
import http.server
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import threading
import time
from pathlib import Path

PORT = 8765
TOKEN = "rehearsal-token-0123456789abcdef"
APP_ID = "com.hackclub.lookout"
IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"


def log(msg):
    print(f"[rehearsal {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def wait_for(what, pred, timeout, every=2.0):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            log(f"ok: {what}")
            return True
        time.sleep(every)
    log(f"timed out after {timeout}s: {what}")
    return False


# ── Update server ───────────────────────────────────────────────────────

class Server:
    def __init__(self, root):
        self.requests = []
        requests = self.requests

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *a, **kw):
                super().__init__(*a, directory=str(root), **kw)

            def log_message(self, fmt, *args):
                line = fmt % args
                requests.append(line)
                log(f"http: {line}")

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def served(self, name):
        return any(f"GET /{name} " in r and '" 200 ' in r for r in self.requests)


def make_feed(native, tool, key_dir, root):
    """Signs `native` with the test key and writes latest.json next to it."""
    m = re.match(r"Lookout-native_(.+?)_(darwin-aarch64|darwin-x86_64|windows-x86_64|windows-aarch64)", native.name)
    if not m:
        sys.exit(f"unexpected native artifact name: {native.name}")
    version, platform = m.group(1), m.group(2)
    root.mkdir(parents=True, exist_ok=True)
    served = root / native.name
    shutil.copy2(native, served)
    env = dict(os.environ, TAURI_SIGNING_PRIVATE_KEY=(key_dir / "test.key").read_text(), TAURI_SIGNING_PRIVATE_KEY_PASSWORD="")
    subprocess.run([str(tool), "sign", str(served)], check=True, env=env)
    sig = base64.b64encode((root / f"{native.name}.minisig").read_bytes()).decode()
    feed = {
        "version": f"v{version}",
        "notes": "takeover rehearsal",
        "pub_date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        # The same shape release.yml's takeover step writes.
        "platforms": {platform: {"url": f"http://127.0.0.1:{PORT}/{native.name}", "signature": sig}},
    }
    (root / "latest.json").write_text(json.dumps(feed, indent=2))
    log(f"latest.json: v{version} {platform} -> {native.name}")
    return version


# ── Platform specifics ──────────────────────────────────────────────────

def config_dir():
    if IS_WIN:
        return Path(os.environ["APPDATA"]) / APP_ID
    return Path.home() / "Library/Application Support" / APP_ID


class Windows:
    def __init__(self):
        self.dir = Path(os.environ["LOCALAPPDATA"]) / "Lookout"
        self.exe = self.dir / "lookout.exe"

    def install(self, setup):
        log(f"installing Tauri: {setup.name} /S")
        subprocess.run([str(setup), "/S"], check=True)
        if not wait_for("Tauri installed", self.exe.is_file, 120):
            sys.exit("the Tauri installer didn't produce lookout.exe")

    def launch(self, *args):
        subprocess.Popen([str(self.exe), *args], close_fds=True)

    def sign_in(self):
        self.launch(f"lookout://session/?token={TOKEN}")
        time.sleep(20)
        # A second launch forwards the link to the running instance, in case
        # the first one came up before the frontend listened for it.
        self.launch(f"lookout://session/?token={TOKEN}")

    def quit(self):
        subprocess.run(["taskkill", "/IM", "lookout.exe", "/F"], capture_output=True)

    def running(self):
        r = subprocess.run(["tasklist", "/FI", "IMAGENAME eq lookout.exe", "/FO", "CSV", "/NH"], capture_output=True, text=True)
        return "lookout.exe" in r.stdout.lower()

    def installed_matches(self, native):
        # lookout.exe installs itself, byte for byte.
        return self.exe.is_file() and sha256(self.exe) == sha256(native)

    def installed_version(self):
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Lookout") as k:
                return winreg.QueryValueEx(k, "DisplayVersion")[0]
        except OSError as e:
            return f"<unreadable: {e}>"

    def extra_checks(self, check):
        check("Tauri's uninstall.exe was removed", not (self.dir / "uninstall.exe").exists(), required=False)

    def storage_listing(self):
        return Path(os.environ["LOCALAPPDATA"]) / APP_ID / "EBWebView/Default/Local Storage"

    def logs(self):
        return [Path(os.environ["LOCALAPPDATA"]) / APP_ID / "logs", Path(os.environ["APPDATA"]) / APP_ID / "logs"]

    def screenshot(self, path):
        ps = (
            "Add-Type -AssemblyName System.Windows.Forms,System.Drawing;"
            "$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds;"
            "$i=New-Object System.Drawing.Bitmap $b.Width,$b.Height;"
            "$g=[System.Drawing.Graphics]::FromImage($i);"
            "$g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size);"
            f"$i.Save('{path}')"
        )
        subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True)


class Mac:
    def __init__(self):
        self.app = Path("/Applications/Lookout.app")
        self.exe = self.app / "Contents/MacOS/lookout"

    def install(self, app):
        log(f"installing Tauri: {app} -> {self.app}")
        if self.app.exists():
            shutil.rmtree(self.app)
        subprocess.run(["ditto", str(app), str(self.app)], check=True)
        # Register the lookout:// scheme with Launch Services.
        subprocess.run(["/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister", "-f", str(self.app)])

    def launch(self, *args):
        subprocess.run(["open", "-n", str(self.app), *(["--args", *args] if args else [])], check=True)

    def sign_in(self):
        self.launch()
        time.sleep(20)
        subprocess.run(["open", f"lookout://session/?token={TOKEN}"])

    def quit(self):
        subprocess.run(["osascript", "-e", f'tell application id "{APP_ID}" to quit'], capture_output=True)
        time.sleep(5)
        subprocess.run(["pkill", "-x", "lookout"], capture_output=True)

    def running(self):
        return subprocess.run(["pgrep", "-f", str(self.exe)], capture_output=True).returncode == 0

    def installed_matches(self, native):
        with tarfile.open(native) as t:
            member = t.extractfile("Lookout.app/Contents/MacOS/lookout")
            expected = hashlib.sha256(member.read()).hexdigest()
        return self.exe.is_file() and sha256(self.exe) == expected

    def installed_version(self):
        try:
            with open(self.app / "Contents/Info.plist", "rb") as f:
                return plistlib.load(f).get("CFBundleShortVersionString")
        except OSError as e:
            return f"<unreadable: {e}>"

    def extra_checks(self, check):
        pass

    def storage_listing(self):
        return Path.home() / "Library/WebKit" / APP_ID

    def logs(self):
        return [Path.home() / "Library/Logs" / APP_ID]

    def screenshot(self, path):
        subprocess.run(["screencapture", "-x", str(path)], capture_output=True)


# ── Main ────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tauri", required=True, type=Path)
    ap.add_argument("--native", required=True, type=Path)
    ap.add_argument("--tool", required=True, type=Path)
    ap.add_argument("--key-dir", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    os_ = Windows() if IS_WIN else Mac()

    version = make_feed(a.native, a.tool, a.key_dir, a.out / "feed")
    server = Server(a.out / "feed")
    os_.install(a.tauri)

    # Session 1: sign in; the updater finds the update and downloads it.
    log("session 1: launch Tauri and sign in")
    os_.sign_in()
    time.sleep(5)
    os_.screenshot(a.out / "1-tauri-signed-in.png")
    downloaded = wait_for("Tauri downloaded the native build", lambda: server.served(a.native.name), 300)
    # The pending-update marker and the token have to reach disk before quitting.
    time.sleep(45)
    os_.screenshot(a.out / "2-tauri-update-downloaded.png")
    os_.quit()
    time.sleep(5)

    # Session 2: 0.3.14 installs an update downloaded in an earlier session by itself.
    log("session 2: relaunch Tauri; it should install the update and relaunch as native")
    os_.launch()
    time.sleep(8)
    os_.screenshot(a.out / "3-tauri-relaunched.png")
    state = config_dir() / "native-state.json"
    wait_for("Lookout Native ran (native-state.json written)", state.is_file, 360)
    time.sleep(15)
    os_.screenshot(a.out / "4-native-after-takeover.png")

    failures = []

    def check(name, ok, required=True):
        log(f"{'PASS' if ok else ('FAIL' if required else 'WARN')}: {name}")
        if not ok and required:
            failures.append(name)

    check("Tauri downloaded the update from latest.json", downloaded)
    check("the installed app is the native build", os_.installed_matches(a.native))
    installed = os_.installed_version()
    check(f"installed version is {version} (found {installed})", installed == version)
    try:
        st = json.loads(state.read_text())
        (a.out / "native-state.json").write_text(json.dumps(st, indent=2))
    except (OSError, ValueError):
        st = {}
    check("Lookout Native launched after the update", state.is_file())
    check("native imported the Tauri app's storage", st.get("importedTauri") is True)
    check("the sign-in survived (token imported)", any(t.get("token") == TOKEN for t in st.get("tokens", [])))
    check("Lookout Native is still running", os_.running(), required=False)
    os_.extra_checks(check)
    time.sleep(20)
    os_.screenshot(a.out / "5-native-settled.png")

    # Evidence for whoever reads the run.
    listing = os_.storage_listing()
    with open(a.out / "webview-storage.txt", "w") as f:
        for p in sorted(listing.rglob("*")) if listing.exists() else []:
            f.write(f"{p.relative_to(listing)}\n")
    for d in os_.logs():
        if d.exists():
            shutil.copytree(d, a.out / "logs" / d.parent.name / d.name, dirs_exist_ok=True)
    (a.out / "http.log").write_text("\n".join(server.requests))
    os_.quit()

    if failures:
        log(f"{len(failures)} required check(s) failed: {failures}")
        sys.exit(1)
    log("takeover rehearsal passed")


if __name__ == "__main__":
    main()
