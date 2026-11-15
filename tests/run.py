#!/usr/bin/env python3
"""QEMU + OVMF tests for the loader spike. Runs inside the build image (make test).

Each case builds a FAT boot partition with mtools, boots it, syncs on the loader's and
the test initramfs's serial markers, grabs frames with QMP screendump and checks them
against the expected pixels. Frames land in out/test/<case>/.
"""
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import mksplash  # noqa: E402
from PIL import Image  # noqa: E402

OUT = os.path.join(ROOT, "out", "test")
LOADER = os.path.join(ROOT, "target", "x86_64-unknown-uefi", "release", "glassine-loader.efi")
PAYLOAD = os.path.join(ROOT, "out", "payload")
OVMF_CODE = "/usr/share/OVMF/OVMF_CODE_4M.fd"
OVMF_VARS = "/usr/share/OVMF/OVMF_VARS_4M.fd"
W, H = 1280, 800
DEPLOY_ID = "d20261101-a"
os.environ["MTOOLS_SKIP_CHECK"] = "1"

# ---------------------------------------------------------------- test pictures


def splash_px():
    """Native 1280x800: dark blue, eight colour bars, a white frame."""
    bars = [0xE04040, 0xE0A040, 0xE0E040, 0x40E040, 0x40E0E0, 0x4040E0, 0xA040E0, 0xE0E0E0]
    px = []
    for y in range(H):
        for x in range(W):
            if 100 <= y < 300 and 80 <= x < 1200:
                px.append(bars[(x - 80) * 8 // 1120])
            elif (y in (40, 41) or y in (H - 42, H - 41)) and 40 <= x < W - 40:
                px.append(0xFFFFFF)
            else:
                px.append(0x102040)
    return px


def override_px():
    """Native: a 40 px red and yellow checkerboard."""
    return [0xD02020 if ((x // 40) + (y // 40)) % 2 else 0xF0D020 for y in range(H) for x in range(W)]


def default_px():
    """Made for 640x480 (another mode): green frame on grey, a black cross."""
    w, h = 640, 480
    px = []
    for y in range(h):
        for x in range(w):
            if x < 8 or y < 8 or x >= w - 8 or y >= h - 8:
                px.append(0x20C040)
            elif abs(x - w // 2) < 4 or abs(y - h // 2) < 4:
                px.append(0x000000)
            else:
                px.append(0x909090)
    return px, w, h


def compose(px, w, h, mode, bg):
    """Python twin of splash::compose: native pixel for pixel, else fit + centre."""
    screen = [bg] * (W * H)
    native = mode == (W, H)
    if native:
        dw, dh = min(w, W), min(h, H)
    elif w * H <= h * W:
        dw, dh = max(w * H // h, 1), H
    else:
        dw, dh = W, max(h * W // w, 1)
    ox, oy = (W - dw) // 2, (H - dh) // 2
    for y in range(dh):
        sy = y if native else y * h // dh
        for x in range(dw):
            sx = x if native else x * w // dw
            screen[(oy + y) * W + ox + x] = px[sy * w + sx]
    return screen


# ---------------------------------------------------------------- boot partition


def mtools(*args):
    subprocess.run(args, check=True, capture_output=True)


def make_esp(path, files, conf):
    if os.path.exists(path):
        os.remove(path)
    with open(path, "wb") as f:
        f.truncate(64 << 20)
    mtools("mkfs.vfat", "-F", "32", "-n", "GLBOOT", path)
    for d in ["EFI", "EFI/BOOT", "glassine", "glassine/splash", "glassine/sets", "glassine/sets/a"]:
        mtools("mmd", "-i", path, "::/" + d)
    tmp = path + ".f"
    put = dict(files)
    put["EFI/BOOT/BOOTX64.EFI"] = open(LOADER, "rb").read()
    put["glassine/sets/a/vmlinuz"] = open(os.path.join(PAYLOAD, "vmlinuz"), "rb").read()
    put["glassine/sets/a/initrd"] = open(os.path.join(PAYLOAD, "initrd"), "rb").read()
    rec = {
        "kernel": "\\glassine\\sets\\a\\vmlinuz",
        "kernel_sha256": hashlib.sha256(put["glassine/sets/a/vmlinuz"]).hexdigest(),
        "initrd": "\\glassine\\sets\\a\\initrd",
        "initrd_sha256": hashlib.sha256(put["glassine/sets/a/initrd"]).hexdigest(),
        "cmdline": "console=ttyS0 quiet",
        "previous_deploy": DEPLOY_ID + " 3.24.1 (2026-11-01)",
    }
    rec.update(conf)
    put["glassine/loader.conf"] = "".join(f"{k}={v}\n" for k, v in rec.items()).encode()
    for name, data in put.items():
        with open(tmp, "wb") as f:
            f.write(data)
        mtools("mcopy", "-i", path, tmp, "::/" + name)
    os.remove(tmp)


def esp_has(path, name):
    r = subprocess.run(["mdir", "-b", "-i", path, "::/glassine/splash"], capture_output=True, text=True)
    return any(line.rstrip().lower().endswith(name.lower()) for line in r.stdout.splitlines())


# ---------------------------------------------------------------- QEMU


class VM:
    def __init__(self, case, esp, readonly=False):
        self.dir = os.path.join(OUT, case)
        os.makedirs(self.dir, exist_ok=True)
        self.serial = os.path.join(self.dir, "serial.log")
        self.qmp_path = os.path.join(self.dir, "qmp.sock")
        vars_fd = os.path.join(self.dir, "vars.fd")
        with open(OVMF_VARS, "rb") as s, open(vars_fd, "wb") as d:
            d.write(s.read())
        for p in (self.serial, self.qmp_path):
            if os.path.exists(p):
                os.remove(p)
        ro = ",readonly=on" if readonly else ""
        self.proc = subprocess.Popen([
            "qemu-system-x86_64", "-machine", "q35", "-m", "512", "-nodefaults",
            "-device", f"VGA,xres={W},yres={H},edid=on", "-display", "none",
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}",
            "-drive", f"if=pflash,format=raw,unit=1,file={vars_fd}",
            "-drive", f"file={esp},format=raw,if=virtio{ro}",
            "-serial", f"file:{self.serial}", "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-net", "none", "-no-reboot",
        ])
        self.t0 = time.time()
        for _ in range(200):
            try:
                self.sock = socket.socket(socket.AF_UNIX)
                self.sock.connect(self.qmp_path)
                break
            except OSError:
                time.sleep(0.02)
        self.rf = self.sock.makefile("r")
        self.rf.readline()
        self.cmd("qmp_capabilities")
        self.shots = 0

    def cmd(self, execute, **args):
        self.sock.sendall(json.dumps({"execute": execute, "arguments": args}).encode())
        while True:
            m = json.loads(self.rf.readline())
            if "return" in m:
                return m["return"]
            if "error" in m:
                raise RuntimeError(m["error"])

    def log(self):
        try:
            with open(self.serial, "rb") as f:
                return f.read().decode("utf-8", "replace")
        except FileNotFoundError:
            return ""

    def wait(self, pattern, timeout=90):
        end = time.time() + timeout
        while time.time() < end:
            m = re.search(pattern, self.log())
            if m:
                return m
            if self.proc.poll() is not None:
                m = re.search(pattern, self.log())
                if m:
                    return m
                break
            time.sleep(0.02)
        raise AssertionError(f"{self.dir}: no serial match for {pattern!r}")

    def shot(self, name):
        path = os.path.join(self.dir, name + ".png")
        self.cmd("screendump", filename=path, format="png")
        im = Image.open(path).convert("RGB")
        assert im.size == (W, H), im.size
        return [(r << 16) | (g << 8) | b for r, g, b in im.getdata()]

    def key(self, qcode):
        self.cmd("send-key", keys=[{"type": "qcode", "data": qcode}])

    def finish(self, timeout=120):
        try:
            self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            raise AssertionError(f"{self.dir}: QEMU did not power off")

    def kill(self):
        self.proc.kill()
        self.proc.wait()


# ---------------------------------------------------------------- frame checks


def diff(frame, want):
    xs, ys, n = [], [], 0
    for i, (a, b) in enumerate(zip(frame, want)):
        if a != b:
            n += 1
            xs.append(i % W)
            ys.append(i // W)
    if not n:
        return 0, None
    return n, (min(xs), min(ys), max(xs), max(ys))


def assert_exact(frame, want, what):
    n, box = diff(frame, want)
    assert n == 0, f"{what}: {n} pixels differ, box {box}"


def assert_hint_only(frame, want, what):
    """The frame is `want` except for one line near the bottom centre."""
    n, box = diff(frame, want)
    assert n > 0, f"{what}: no hint line drawn"
    x0, y0, x1, y1 = box
    assert y0 > H - 120 and y1 < H and x0 > W // 5 and x1 < W - W // 5, f"{what}: change outside the hint area: {box}"


def frames_during_kernel(vm, want, until="INIT_POWEROFF"):
    """Grab frames from the kernel's start until the initramfs is done; count those
    that differ from `want`."""
    vm.wait("START_KERNEL")
    seen, differ, first_diff = 0, 0, None
    while not re.search(until, vm.log()) and vm.proc.poll() is None:
        f = vm.shot(f"kernel-{seen:03d}")
        n, box = diff(f, want)
        if n:
            differ += 1
            if first_diff is None:
                first_diff = (seen, n, box)
        else:
            os.remove(os.path.join(vm.dir, f"kernel-{seen:03d}.png"))
        seen += 1
    return seen, differ, first_diff


# ---------------------------------------------------------------- cases

RESULTS = []


def case(fn):
    def run():
        t = time.time()
        try:
            fn()
            RESULTS.append((fn.__name__, "PASS", f"{time.time() - t:.0f}s"))
        except AssertionError as e:
            RESULTS.append((fn.__name__, "FAIL", str(e)))
        print(f"{RESULTS[-1][1]} {fn.__name__} {RESULTS[-1][2]}", flush=True)
    run.__name__ = fn.__name__
    return run


SPL = splash_px()
OVR = override_px()
DEF, DW, DH = default_px()
SPLASH = mksplash.encode(SPL, W, H, (W, H), 0x102040)
OVERRIDE = mksplash.encode(OVR, W, H, (W, H), 0)
DEFAULT = mksplash.encode(DEF, DW, DH, (DW, DH), 0x335577)
WANT_SPLASH = compose(SPL, W, H, (W, H), 0x102040)
WANT_OVERRIDE = compose(OVR, W, H, (W, H), 0)
WANT_DEFAULT = compose(DEF, DW, DH, (DW, DH), 0x335577)
STORE = "glassine/splash/"


def esp_path(name):
    os.makedirs(os.path.join(OUT, "esp"), exist_ok=True)
    return os.path.join(OUT, "esp", name + ".img")


@case
def override_once_then_splash():
    """011 Scenario 2: the override at the first start, gone, the splash at the next."""
    esp = esp_path("override")
    make_esp(esp, {STORE + "splash.gspl": SPLASH, STORE + "default.gspl": DEFAULT,
                   STORE + "override.gspl": OVERRIDE}, {})
    vm = VM("1-override", esp)
    vm.wait("OVERRIDE_REMOVED")
    vm.wait("SPLASH_DRAWN override")
    assert_hint_only(vm.shot("override-hint"), WANT_OVERRIDE, "override + hint")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("override"), WANT_OVERRIDE, "override after the wait")
    seen, differ, first = frames_during_kernel(vm, WANT_OVERRIDE)
    m = vm.wait(r"INIT_CMDLINE: (.*)")
    assert "glassine.deploy.once" not in m.group(1), m.group(1)
    vm.finish()
    assert differ == 0, f"kernel changed the frame: {differ}/{seen} frames, first {first}"
    assert not esp_has(esp, "override.gspl"), "override still in the store"

    vm = VM("2-splash-after-override", esp)
    vm.wait("SPLASH_DRAWN splash")
    assert "OVERRIDE" not in vm.log()
    assert_hint_only(vm.shot("splash-hint"), WANT_SPLASH, "splash + hint")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "splash after the wait")
    seen, differ, first = frames_during_kernel(vm, WANT_SPLASH)
    vm.finish()
    assert differ == 0, f"kernel changed the frame: {differ}/{seen} frames, first {first}"


@case
def recovery_key_list_and_deploy_once():
    """010 FR-039 list; FR-042 one-shot pick on the command line."""
    esp = esp_path("list")
    make_esp(esp, {STORE + "splash.gspl": SPLASH}, {})
    vm = VM("3-list", esp)
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait("LIST_SHOWN")
    f = vm.shot("list")
    n, box = diff(f, WANT_SPLASH)
    assert n > 0 and box[0] > 200 and box[2] < W - 200, f"list not drawn centred: {box}"
    vm.key("down")
    time.sleep(0.5)
    f2 = vm.shot("list-down")
    assert diff(f2, f)[0] > 0, "focus did not move"
    vm.key("ret")
    vm.wait(r"CHOICE 1 Previous deployment")
    m = vm.wait(r"INIT_CMDLINE: (.*)")
    assert f"glassine.deploy.once={DEPLOY_ID}" in m.group(1), m.group(1)
    vm.finish()


@case
def damaged_splash_falls_back_to_default():
    """011 Scenario 3 + FR-007: a bad splash is skipped; the default, made for another
    mode, is scaled to fit and centred on its background."""
    bad = bytearray(SPLASH)
    bad[200] ^= 0xFF
    esp = esp_path("damaged")
    make_esp(esp, {STORE + "splash.gspl": bytes(bad), STORE + "default.gspl": DEFAULT}, {})
    vm = VM("4-damaged", esp)
    vm.wait("skipped: PayloadCrc")
    vm.wait("SPLASH_DRAWN default")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("default-scaled"), WANT_DEFAULT, "default scaled")
    vm.wait("START_KERNEL")
    vm.finish()


@case
def override_not_removable_is_not_drawn():
    """011 FR-012: a store the loader can't write: the override is not drawn."""
    esp = esp_path("readonly")
    make_esp(esp, {STORE + "splash.gspl": SPLASH, STORE + "override.gspl": OVERRIDE}, {})
    vm = VM("5-readonly", esp, readonly=True)
    vm.wait("OVERRIDE_NOT_REMOVED")
    vm.wait("SPLASH_DRAWN splash")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "splash on a read-only store")
    vm.wait("INIT_CMDLINE")
    vm.finish()
    assert esp_has(esp, "override.gspl")


@case
def power_cut_after_removal():
    """011 SC-003 (one point): power cut right after the removal; the override is gone
    and the next start shows the splash."""
    esp = esp_path("powercut")
    make_esp(esp, {STORE + "splash.gspl": SPLASH, STORE + "override.gspl": OVERRIDE}, {})
    vm = VM("6-powercut", esp)
    vm.wait("OVERRIDE_REMOVED")
    vm.kill()
    assert not esp_has(esp, "override.gspl"), "override survived the cut"
    vm = VM("6-powercut-next", esp)
    vm.wait("SPLASH_DRAWN splash")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "splash after the cut")
    vm.kill()


@case
def leave_screen_override():
    """011 FR-014: the override says leave the screen; the loader draws nothing."""
    esp = esp_path("leave")
    make_esp(esp, {STORE + "splash.gspl": SPLASH, STORE + "override.gspl": mksplash.leave_screen()}, {})
    vm = VM("7-leave-screen", esp)
    vm.wait("LOADER_START")
    before = vm.shot("firmware")
    vm.wait("SPLASH_DRAWN leave-screen")
    vm.wait("WAIT_END")
    assert_exact(vm.shot("after-wait"), before, "screen left as the firmware drew it")
    vm.wait("START_KERNEL")
    vm.kill()
    assert not esp_has(esp, "override.gspl")


@case
def leave_screen_key_still_opens_list():
    """010 FR-017: the key works even when the loader drew nothing."""
    esp = esp_path("leave-key")
    make_esp(esp, {STORE + "splash.gspl": SPLASH, STORE + "override.gspl": mksplash.leave_screen()}, {})
    vm = VM("8-leave-screen-key", esp)
    vm.wait("LOADER_START")
    before = vm.shot("firmware")
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait("LIST_SHOWN")
    n, box = diff(vm.shot("list"), before)
    assert n > 0 and box[0] > 200 and box[2] < W - 200, f"list not drawn centred: {box}"
    vm.key("ret")
    vm.wait("CHOICE 0 GlassineOS")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("after-list"), before, "screen restored after the list")
    vm.kill()


def observe_fr016():
    """Goal 7: which command lines keep the loader's frame through the kernel start."""
    variants = [
        ("quiet", {"cmdline": "console=ttyS0 quiet"}),
        ("no-quiet", {"cmdline": "console=ttyS0"}),
        ("no-quiet-exclusive-gop", {"cmdline": "console=ttyS0", "exclusive_gop": "1"}),
        ("console-tty0", {"cmdline": "console=ttyS0 console=tty0 quiet"}),
        ("fbcon-nodefer", {"cmdline": "console=ttyS0 quiet fbcon=nodefer"}),
        ("console-tty0-noquiet", {"cmdline": "console=ttyS0 console=tty0"}),
    ]
    rows = []
    for name, conf in variants:
        esp = esp_path("fr016-" + name)
        make_esp(esp, {STORE + "splash.gspl": SPLASH}, conf)
        vm = VM("fr016-" + name, esp)
        vm.wait("HINT_HIDDEN")
        seen, differ, first = frames_during_kernel(vm, WANT_SPLASH)
        vtcon = vm.wait(r"INIT_VTCON: (.*)").group(1).strip()
        fb = vm.wait(r"INIT_FB: (.*)").group(1).strip()
        try:
            vm.finish()
        except AssertionError:
            pass
        rows.append({"variant": name, "cmdline": conf["cmdline"], "exclusive_gop": conf.get("exclusive_gop", "0"),
                     "frames": seen, "frames_changed": differ, "first_change": first, "fb": fb, "vtcon": vtcon})
        print(f"OBSERVE fr016 {name}: {differ}/{seen} frames changed; vtcon {vtcon}", flush=True)
    with open(os.path.join(OUT, "fr016.json"), "w") as f:
        json.dump(rows, f, indent=1)


def main():
    os.makedirs(OUT, exist_ok=True)
    only = sys.argv[1:]
    cases = [override_once_then_splash, recovery_key_list_and_deploy_once,
             damaged_splash_falls_back_to_default, override_not_removable_is_not_drawn,
             power_cut_after_removal, leave_screen_override,
             leave_screen_key_still_opens_list]
    for c in cases:
        if not only or c.__name__ in only:
            c()
    if not only or "observe" in only:
        observe_fr016()
    fails = [r for r in RESULTS if r[1] != "PASS"]
    print(f"{len(RESULTS) - len(fails)}/{len(RESULTS)} passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
