#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""QEMU tests of the loader, with OVMF (x86-64) and AAVMF (arm64) from the build image's
Debian packages. Runs inside the build image (make test).

Each case writes a GPT disk whose one partition is the boot partition, written by the
installer's own code (tests/mkboot, GlassineOS/installer pinned), changes it with
mtools where the case needs another state, boots it, syncs on the loader's serial
markers and the test kernel's (tests/stub), grabs frames with QMP screendump and
checks them pixel for pixel, and reads the loader record back after the start.
Frames and serial logs land in out/test/<case>/.

  run.py [--arch x86_64|aarch64] [CASE ...]
"""
import argparse
import hashlib
import json
import os
import re
import socket
import struct
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import mksplash  # noqa: E402
from PIL import Image  # noqa: E402

OUT = os.path.join(ROOT, "out", "test")
MKBOOT = os.path.join(ROOT, "tests", "mkboot", "target", "release", "glassine-test-mkboot")
GSPL_ENCODE = os.path.join(ROOT, "target", "release", "gspl-encode")
PAYLOAD = os.path.join(ROOT, "out", "payload")
W, H = 1280, 800
SECTOR = 512
PART_OFF = 1 << 20
PART_SIZE = 256 << 20
PARTUUID = "3b5c2e9a-6f1d-4e8b-9a2c-7d4e1f0b8c6a"
SYSTEM = "0f3e26a1-5c7b-4a55-9b8d-2c4f6e8a1b3d"
USERDATA = "6d1a5e2b-8c4f-4b3a-a9e7-1f2d3c4b5a69"
# The test kernel holds the frame a moment for the captures, then powers off.
BOARD = b'CMDLINE="console=ttyS0 quiet stub.hold=1500"\n'
RECOVERY_CMDLINE = "quiet recovery stub.hold=500"
os.environ["MTOOLS_SKIP_CHECK"] = "1"

ARCHES = {
    "x86_64": {
        "target": "x86_64-unknown-uefi",
        "name": "x86-64",
        "qemu": ["qemu-system-x86_64", "-machine", "q35", "-m", "1024"],
        "code": "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "vars": "/usr/share/OVMF/OVMF_VARS_4M.fd",
        "display": ["-device", f"VGA,id=vga0,xres={W},yres={H},edid=on"],
        "kvm": os.uname().machine == "x86_64",
    },
    "aarch64": {
        "target": "aarch64-unknown-uefi",
        "name": "arm64",
        "qemu": ["qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "1024"],
        "code": "/usr/share/AAVMF/AAVMF_CODE.no-secboot.fd",
        "vars": "/usr/share/AAVMF/AAVMF_VARS.fd",
        "display": ["-device", f"virtio-gpu-pci,id=vga0,xres={W},yres={H}", "-device", "qemu-xhci", "-device", "usb-kbd"],
        "kvm": os.uname().machine == "aarch64",
    },
}
A = ARCHES["x86_64"]


def efi(name):
    return os.path.join(ROOT, "target", A["target"], "release", name + ".efi")


def sha(b):
    return hashlib.sha256(b).hexdigest()


# ---------------------------------------------------------------- records (format 1)


def checked(body):
    return body + f"check={sha(body.encode())}\n"


def parse_record(b):
    """The record's lines, if it is valid (installer pc_boot::parse_record)."""
    try:
        t = b.decode()
    except UnicodeDecodeError:
        return None
    if not t.endswith("\n") or "\r" in t:
        return None
    end = t[:-1].rfind("\n") + 1
    body, last = t[:end], t[end:]
    if not last.startswith("check=") or last[6:-1] != sha(body.encode()):
        return None
    lines = []
    for i, line in enumerate(body.split("\n")[:-1]):
        if "=" not in line:
            return None
        k, v = line.split("=", 1)
        if not k or k in dict(lines) or (i == 0 and line != "format=1"):
            return None
        lines.append((k, v))
    return lines or None


def read_slot(b):
    if b is None:
        return None
    s = b.rstrip(b"\n")
    if len(s) == len(b) or not s:
        return None
    lines = parse_record(s + b"\n")
    if not lines or not dict(lines).get("seq", "").isdigit():
        return None
    return int(dict(lines)["seq"]), lines


def slot_bytes(lines):
    b = checked("".join(f"{k}={v}\n" for k, v in lines)).encode()
    assert len(b) <= 16384
    return b + b"\n" * (16384 - len(b))


def set_record(lines, changes):
    lines = list(lines)
    for k, v in changes.items():
        for i, (x, _) in enumerate(lines):
            if x == k:
                lines[i] = (k, v)
                break
        else:
            lines.append((k, v))
    return lines


def set_rec(kernel, initrd, base, release="test"):
    """A set's id, set.rec and command line, as installer pc_boot::KernelSet makes them."""
    ks = sha(kernel)
    isha = sha(initrd) if initrd is not None else ""
    sid = sha(f"{ks}\n{isha}\n{base}\n".encode())[:16]
    words = ([base] if base else []) + [f"glassine.system=PARTUUID={SYSTEM}", f"glassine.userdata=PARTUUID={USERDATA}", f"glassine.img={sid}"]
    cmd = " ".join(words)
    isize = str(len(initrd)) if initrd is not None else ""
    rec = checked(f"format=1\nid={sid}\nrelease={release}\nkernel_sha256={ks}\nkernel_size={len(kernel)}\n"
                  f"initrd_sha256={isha}\ninitrd_size={isize}\ncmdline={cmd}\ninitramfs=\n").encode()
    return sid, rec, cmd


# ---------------------------------------------------------------- the boot partition


def run(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, **kw)


class Disk:
    """A GPT disk with the boot partition at 1 MiB, written by the installer's code."""

    def __init__(self, path, loader=None, kernel=None, initramfs=b"glassine test initramfs\n", board=BOARD,
                 recovery=None, splash=None):
        self.path = path
        if os.path.exists(path):
            os.remove(path)
        with open(path, "wb") as f:
            f.truncate(PART_OFF + PART_SIZE + (1 << 20))
        run("sgdisk", "-o", "-n", f"1:{PART_OFF // SECTOR}:+{PART_SIZE // SECTOR}", "-t", "1:ef00",
            "-c", "1:glassine-boot", "-u", f"1:{PARTUUID}", path)
        tmp = path + ".d"
        os.makedirs(tmp, exist_ok=True)
        files = {}
        for name, data in [("loader", open(loader or efi("glassine-loader"), "rb").read()),
                           ("kernel", kernel or open(efi("glassine-test-kernel"), "rb").read()),
                           ("initramfs", initramfs), ("board", board),
                           ("recovery", recovery or open(efi("glassine-test-kernel"), "rb").read())]:
            files[name] = os.path.join(tmp, name)
            with open(files[name], "wb") as f:
                f.write(data)
        sp = "-"
        if splash is not None:
            sp = os.path.join(tmp, "splash")
            with open(sp, "wb") as f:
                f.write(splash)
        out = run(MKBOOT, path, str(PART_OFF), str(PART_SIZE), PARTUUID, A["name"], files["loader"], files["kernel"],
                  files["initramfs"], files["board"], files["recovery"], "-", RECOVERY_CMDLINE, sp, SYSTEM, USERDATA,
                  text=True).stdout
        self.info = dict(line.split("=", 1) for line in out.splitlines())
        for p in list(files.values()) + ([sp] if sp != "-" else []):
            os.remove(p)
        os.rmdir(tmp)
        self.kernel = open(efi("glassine-test-kernel"), "rb").read() if kernel is None else kernel
        # Our generator of set records makes the installer's bytes.
        got = self.get(f"glassine/sets/{self.info['set']}/set.rec")
        assert sha(got) == self.info["set_rec_sha256"]
        initrd = self.get(f"glassine/sets/{self.info['set']}/initrd")
        assert sha(initrd) == self.info["initrd_sha256"]
        sid, rec, cmd = set_rec(self.kernel, initrd, board.decode().split('"')[1])
        assert (sid, rec, cmd) == (self.info["set"], got, self.info["cmdline"]), "set.rec generator differs from the installer's"

    def img(self):
        return f"{self.path}@@{PART_OFF}"

    def get(self, p):
        r = subprocess.run(["mtype", "-i", self.img(), "::/" + p], capture_output=True)
        return r.stdout if r.returncode == 0 else None

    def put(self, p, data):
        parts = p.split("/")
        for i in range(1, len(parts)):
            subprocess.run(["mmd", "-D", "s", "-i", self.img(), "::/" + "/".join(parts[:i])], capture_output=True)
        tmp = self.path + ".put"
        with open(tmp, "wb") as f:
            f.write(data)
        run("mcopy", "-o", "-i", self.img(), tmp, "::/" + p)
        os.remove(tmp)

    def rm(self, p):
        run("mdel", "-i", self.img(), "::/" + p)

    def has(self, p):
        return self.get(p) is not None

    def slots(self):
        return [read_slot(self.get(f"glassine/loader.{s}")) for s in "ab"]

    def record(self):
        """(slot, seq, keys, both valid) of the record in use."""
        a, b = self.slots()
        if a and b:
            return (1, b[0], dict(b[1]), True) if b[0] > a[0] else (0, a[0], dict(a[1]), True)
        if a or b:
            i, s = (0, a) if a else (1, b)
            return i, s[0], dict(s[1]), False
        return None

    def write_both(self, changes, seq=1):
        """Both slots the same record (as the installer writes them), with `changes`."""
        a = read_slot(self.get("glassine/loader.a"))
        lines = set_record(a[1], dict(changes, seq=str(seq)))
        for s in "ab":
            self.put(f"glassine/loader.{s}", slot_bytes(lines))

    def add_set(self, initrd, base="console=ttyS0 quiet stub.hold=1500", kernel=None, where="sets"):
        kernel = kernel or self.kernel
        sid, rec, cmd = set_rec(kernel, initrd, base)
        d = f"glassine/{where}/{sid}"
        self.put(f"{d}/vmlinuz", kernel)
        if initrd is not None:
            self.put(f"{d}/initrd", initrd)
        self.put(f"{d}/set.rec", rec)
        return {"id": sid, "rec": sha(rec), "cmdline": cmd, "initrd": initrd}

    def digest(self):
        h = hashlib.sha256()
        with open(self.path, "rb") as f:
            f.seek(PART_OFF)
            h.update(f.read(PART_SIZE))
        return h.hexdigest()


# ---------------------------------------------------------------- QEMU


class VM:
    def __init__(self, case, disk, readonly=False, code=None, vars_fd=None, machine_extra=(), display=None):
        self.dir = os.path.join(OUT, case)
        os.makedirs(self.dir, exist_ok=True)
        self.serial = os.path.join(self.dir, "serial.log")
        self.qmp_path = os.path.join(self.dir, "qmp.sock")
        vf = os.path.join(self.dir, "vars.fd")
        with open(vars_fd or A["vars"], "rb") as s, open(vf, "wb") as d:
            d.write(s.read())
        for p in (self.serial, self.qmp_path):
            if os.path.exists(p):
                os.remove(p)
        ro = ",readonly=on" if readonly else ""
        kvm = A["kvm"] and os.access("/dev/kvm", os.R_OK | os.W_OK)
        cmd = list(A["qemu"]) + list(machine_extra) + [
            "-accel", "kvm" if kvm else "tcg", "-nodefaults", "-display", "none",
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code or A['code']}",
            "-drive", f"if=pflash,format=raw,unit=1,file={vf}",
            "-drive", f"file={disk.path},format=raw,if=none,id=d0{ro}", "-device", "virtio-blk-pci,drive=d0",
            "-serial", f"file:{self.serial}", "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-net", "none", "-no-reboot",
        ] + list(display if display is not None else A["display"])
        self.proc = subprocess.Popen(cmd, stdout=open(os.path.join(self.dir, "qemu.out"), "wb"), stderr=subprocess.STDOUT)
        for _ in range(500):
            try:
                self.sock = socket.socket(socket.AF_UNIX)
                self.sock.connect(self.qmp_path)
                break
            except OSError:
                time.sleep(0.02)
        self.rf = self.sock.makefile("r")
        self.rf.readline()
        self.cmd("qmp_capabilities")

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

    def wait(self, pattern, timeout=150):
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

    def shot(self, name, device=None):
        path = os.path.join(self.dir, name + ".png")
        args = {"filename": path, "format": "png"}
        if device:
            args["device"] = device
        self.cmd("screendump", **args)
        im = Image.open(path).convert("RGB")
        assert im.size == (W, H), im.size
        return [(r << 16) | (g << 8) | b for r, g, b in im.getdata()]

    def key(self, qcode):
        self.cmd("send-key", keys=[{"type": "qcode", "data": qcode}])
        time.sleep(0.3)

    def finish(self, timeout=150):
        try:
            self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            raise AssertionError(f"{self.dir}: QEMU did not power off")

    def kill(self):
        self.proc.kill()
        self.proc.wait()


def stub(vm):
    """The report of the test kernel that ran last: its command line and its initrd."""
    vm.wait("STUB_POWEROFF")
    log = vm.log()
    cmd = re.findall(r"STUB_CMDLINE: ([^\r\n]*)", log)[-1]
    ird = re.findall(r"STUB_INITRD: ([^\r\n]*)", log)[-1]
    return cmd, ird


def initrd_line(b):
    return f"size={len(b)} sha256={sha(b)}" if b is not None else "none"


# ---------------------------------------------------------------- pictures


def splash_px():
    """Native 1280x800: dark blue, eight colour bars, a white frame."""
    bars = [0xE04040, 0xE0A040, 0xE0E040, 0x40E040, 0x40E0E0, 0x4040E0, 0xA040E0, 0xE0E0E0]
    row_bars = [bars[(x - 80) * 8 // 1120] if 80 <= x < 1200 else 0x102040 for x in range(W)]
    row_bg = [0x102040] * W
    row_line = [0xFFFFFF if 40 <= x < W - 40 else 0x102040 for x in range(W)]
    px = []
    for y in range(H):
        px += row_bars if 100 <= y < 300 else row_line if y in (40, 41, H - 42, H - 41) else row_bg
    return px


def override_px():
    """Native: a 40 px red and yellow checkerboard."""
    a = [0xD02020 if (x // 40) % 2 else 0xF0D020 for x in range(W)]
    b = [0xF0D020 if (x // 40) % 2 else 0xD02020 for x in range(W)]
    px = []
    for y in range(H):
        px += a if (y // 40) % 2 == 0 else b
    return px


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


def compose(px, w, h, mode, bg, W=W, H=H):
    """The Python twin of gspl::compose, on a W x H screen (QEMU's by default)."""
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
        row = px[sy * w:(sy + 1) * w]
        if native:
            screen[(oy + y) * W + ox:(oy + y) * W + ox + dw] = row[:dw]
        else:
            screen[(oy + y) * W + ox:(oy + y) * W + ox + dw] = [row[x * w // dw] for x in range(dw)]
    return screen


def diff(frame, want):
    xs = [i for i, (a, b) in enumerate(zip(frame, want)) if a != b]
    if not xs:
        return 0, None
    cols = [i % W for i in xs]
    return len(xs), (min(cols), xs[0] // W, max(cols), xs[-1] // W)


def wait_frame(vm, name, ok, timeout=30):
    """Frames until `ok(frame)` or the time is up; the last frame."""
    end = time.time() + timeout
    while True:
        f = vm.shot(name)
        if ok(f) or time.time() > end:
            return f
        time.sleep(0.2)


def assert_exact(frame, want, what):
    n, box = diff(frame, want)
    assert n == 0, f"{what}: {n} pixels differ, box {box}"


def assert_hint_only(frame, want, what):
    n, box = diff(frame, want)
    assert n > 0, f"{what}: no key line drawn"
    x0, y0, x1, y1 = box
    assert y0 > H - 120 and x0 > W // 5 and x1 < W - W // 5, f"{what}: change outside the key line: {box}"


def assert_centred_box(frame, want, what):
    n, box = diff(frame, want)
    assert n > 0 and box[0] > 200 and box[2] < W - 200, f"{what}: nothing drawn in the centre: {box}"


def luma(p):
    """boot-partition.md, "Pixel layouts": a writer's grey of a colour, drawn as grey."""
    r, g, b = p >> 16, (p >> 8) & 255, p & 255
    y = (77 * r + 150 * g + 29 * b + 128) >> 8
    return y * 0x010101


def encode2(px, w, h, mode, bg, layout, compression="best", filt=None):
    """A format 2 image, written by the writers' own tool (tools/gspl-encode)."""
    import tempfile
    with tempfile.TemporaryDirectory() as t:
        src, out = os.path.join(t, "in.ppm"), os.path.join(t, "out.gspl")
        with open(src, "wb") as f:
            f.write(b"P6\n%d %d\n255\n" % (w, h))
            f.write(b"".join(bytes(((p >> 16) & 255, (p >> 8) & 255, p & 255)) for p in px))
        args = [GSPL_ENCODE, "--layout", layout, "--compression", compression, "--mode", f"{mode[0]}x{mode[1]}",
                "--bg", f"{bg:06x}"] + (["--filter", filt] if filt else []) + [src, out]
        run(*args)
        return open(out, "rb").read()


SPL = SPLASH = WANT_SPLASH = OVR = OVERRIDE = WANT_OVERRIDE = DEFAULT = WANT_DEFAULT = None


def pictures():
    global SPL, SPLASH, WANT_SPLASH, OVR, OVERRIDE, WANT_OVERRIDE, DEFAULT, WANT_DEFAULT
    SPL = splash_px()
    SPLASH = mksplash.encode(SPL, W, H, (W, H), 0x102040)
    WANT_SPLASH = SPL
    OVR = override_px()
    OVERRIDE = mksplash.encode(OVR, W, H, (W, H), 0)
    WANT_OVERRIDE = OVR
    d, dw, dh = default_px()
    DEFAULT = mksplash.encode(d, dw, dh, (dw, dh), 0x335577)
    WANT_DEFAULT = compose(d, dw, dh, (dw, dh), 0x335577)


# ---------------------------------------------------------------- cases

RESULTS = []
CASES = []


def case(*arches):
    def wrap(fn):
        def run_case():
            t = time.time()
            try:
                fn()
                RESULTS.append((fn.__name__, "PASS", f"{time.time() - t:.0f}s"))
            except Exception as e:  # noqa: BLE001
                RESULTS.append((fn.__name__, "FAIL", f"{type(e).__name__}: {e}"))
            print(f"{RESULTS[-1][1]} {fn.__name__} {RESULTS[-1][2]}", flush=True)
        run_case.__name__ = fn.__name__
        run_case.arches = arches or ("x86_64",)
        CASES.append(run_case)
        return run_case
    return wrap


def disk_path(name):
    d = os.path.join(OUT, "disks")
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, name + ".img")


def installed(name, **kw):
    return Disk(disk_path(name), splash=DEFAULT, **kw)


def boot_once(name, d, **kw):
    vm = VM(name, d, **kw)
    cmd, ird = stub(vm)
    vm.finish()
    return vm, cmd, ird


@case("x86_64", "aarch64")
def default_start_reaches_the_kernel():
    """010 FR-006/FR-007/FR-011, Scenario 1: the installer's partition; the default set
    starts from checked memory with its own command line; the start is counted."""
    d = installed("default")
    vm = VM("default", d)
    vm.wait("HINT_SHOWN")
    assert_hint_only(vm.shot("hint"), WANT_DEFAULT, "default splash + key line")
    vm.wait("HINT_HIDDEN")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("kernel"), WANT_DEFAULT, "the default splash, scaled, through the start")
    cmd, ird = stub(vm)
    vm.finish()
    assert cmd == d.info["cmdline"], cmd
    assert ird == f"size={d.info['initrd_size']} sha256={d.info['initrd_sha256']}", ird
    slot, seq, r, both = d.record()
    assert (slot, seq, r["unmarked"], r["started"], r["started_why"], both) == (1, 2, "1", d.info["set"], "default", True), (slot, seq, r)
    assert d.get("glassine/loader.a") == slot_bytes(read_slot(d.get("glassine/loader.a"))[1]), "slot a untouched"


@case()
def three_unmarked_starts_then_previous_then_recovery():
    """010 FR-011, FR-015, Scenario 5: three counted starts, then the previous set
    (started_why=limit), then the recovery, which writes nothing."""
    d = installed("limit")
    prev = d.add_set(b"previous initrd")
    d.write_both({"previous": prev["id"], "previous_rec": prev["rec"]})
    for n in (1, 2, 3):
        _, cmd, _ = boot_once(f"limit-{n}", d)
        assert cmd == d.info["cmdline"], (n, cmd)
        assert d.record()[2]["unmarked"] == str(n)
    _, cmd, ird = boot_once("limit-4-previous", d)
    assert cmd == prev["cmdline"], cmd
    assert ird == initrd_line(prev["initrd"]), ird
    r = d.record()[2]
    assert (r["unmarked"], r["started"], r["started_why"]) == ("4", prev["id"], "limit"), r
    before = d.digest()
    vm, cmd, ird = boot_once("limit-5-recovery", d)
    assert cmd == d.info["recovery_cmdline"], cmd
    assert ird == "none", ird
    assert d.digest() == before, "a recovery start wrote something"
    assert "WAIT_START 5000 ms" in vm.log(), "the longer wait after unmarked starts"


@case()
def trial_request():
    """010 FR-009, FR-010, Scenario 3: request=trial is cleared durably, then the trial
    set starts, uncounted; the next start is the default set."""
    d = installed("trial")
    t = d.add_set(b"trial initrd")
    d.write_both({"trial": t["id"], "trial_rec": t["rec"], "request": "trial", "unmarked": "1"}, seq=7)
    vm, cmd, ird = boot_once("trial-1", d)
    assert cmd == t["cmdline"] and ird == initrd_line(t["initrd"]), (cmd, ird)
    log = vm.log()
    assert log.index("REQUEST_TAKEN trial") < log.index(f"CHECK trial {t['id']} ok"), "the request was cleared after the trial set was read"
    slot, seq, r, _ = d.record()
    assert (seq, r["request"], r["started"], r["started_why"], r["unmarked"]) == (8, "", t["id"], "request", "1"), r
    _, cmd, _ = boot_once("trial-2", d)
    assert cmd == d.info["cmdline"], cmd


@case()
def recovery_request():
    """010 FR-024, boot-partition "Keys": request=recovery is cleared and the recovery
    starts, uncounted."""
    d = installed("recreq")
    d.write_both({"request": "recovery", "unmarked": "2"})
    _, cmd, ird = boot_once("recovery-request", d)
    assert cmd == d.info["recovery_cmdline"] and ird == "none", (cmd, ird)
    slot, seq, r, _ = d.record()
    assert (slot, seq, r["request"], r["unmarked"]) == (1, 2, "", "2"), r


@case()
def damaged_slot_b_leaves_slot_a():
    """010 FR-008: slot b with a higher seq but a failed check counts as absent; slot a
    is used without its request, and the loader's write drops the request."""
    d = installed("slotb")
    t = d.add_set(b"trial initrd")
    d.write_both({"trial": t["id"], "trial_rec": t["rec"], "request": "trial"}, seq=4)
    a = read_slot(d.get("glassine/loader.a"))[1]
    b = bytearray(slot_bytes(set_record(a, {"seq": "5", "unmarked": "0"})))
    b[40] ^= 0x01
    d.put("glassine/loader.b", bytes(b))
    vm, cmd, _ = boot_once("slot-b-damaged", d)
    assert cmd == d.info["cmdline"], f"not the default set: {cmd}"
    assert "not taken: only one slot is valid" in vm.log()
    slot, seq, r, both = d.record()
    assert (slot, seq, r["request"], r["unmarked"], both) == (1, 5, "", "1", True), (slot, seq, r)


@case()
def bad_kernel_sha_falls_back():
    """010 FR-007: a kernel whose bytes don't match its record is not started; the
    previous set is; with that broken too, the recovery; with nothing that checks, a
    message names the boot partition and the loader returns to the firmware."""
    d = installed("badsha")
    prev = d.add_set(b"previous initrd")
    d.write_both({"previous": prev["id"], "previous_rec": prev["rec"]})
    k = bytearray(d.get(f"glassine/sets/{d.info['set']}/vmlinuz"))
    k[1000] ^= 0xFF
    d.put(f"glassine/sets/{d.info['set']}/vmlinuz", bytes(k))
    vm, cmd, _ = boot_once("bad-sha-1", d)
    assert cmd == prev["cmdline"], cmd
    assert re.search(r"CHECK_FAILED default \w+: .*vmlinuz: sha256", vm.log())
    r = d.record()[2]
    assert (r["started"], r["started_why"], r["unmarked"]) == (prev["id"], "limit", "1"), r
    d.rm(f"glassine/sets/{prev['id']}/initrd")
    _, cmd, _ = boot_once("bad-sha-2", d)
    assert cmd == d.info["recovery_cmdline"], cmd
    d.rm(f"glassine/recovery/{d.info['recovery']}/vmlinuz")
    vm = VM("bad-sha-3-nothing", d)
    vm.wait("NOTHING_STARTED")
    m = vm.wait(r"MESSAGE ([^\r\n]*)\r?\n").group(1)
    assert PARTUUID in m and "GLASSINE" in m, m
    f = wait_frame(vm, "message", lambda g: diff(g, WANT_DEFAULT)[0] > 0)
    assert_centred_box(f, WANT_DEFAULT, "the message")
    vm.kill()
    assert "STUB_START" not in vm.log()


@case()
def kernel_that_returns_gives_the_next_choice():
    """010 FR-007, FR-016: a kernel that returns; the next choice starts with its own
    initramfs (nothing of the first left installed), counted once."""
    d = installed("returns", board=b'CMDLINE="console=ttyS0 quiet stub.return"\n')
    prev = d.add_set(b"previous initrd")
    d.write_both({"previous": prev["id"], "previous_rec": prev["rec"]})
    vm, cmd, ird = boot_once("returns", d)
    assert "STUB_RETURN" in vm.log()
    assert cmd == prev["cmdline"] and ird == initrd_line(prev["initrd"]), (cmd, ird)
    slot, seq, r, _ = d.record()
    assert (seq, r["unmarked"], r["started"]) == (2, "1", d.info["set"]), r


@case()
def override_drawn_once_then_gone():
    """011 FR-012, Scenario 2, SC-002: the override is removed and flushed, then drawn,
    kept through the kernel start (apart from the key line), gone at the next start,
    which shows the splash."""
    d = installed("override")
    d.put("glassine/splash/splash.gspl", SPLASH)
    d.put("glassine/splash/override.gspl", OVERRIDE)
    vm = VM("override-1", d)
    vm.wait("OVERRIDE_REMOVED")
    vm.wait("HINT_SHOWN")
    assert_hint_only(vm.shot("override-hint"), WANT_OVERRIDE, "override + key line")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("override-after-wait"), WANT_OVERRIDE, "override after the wait")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("override-kernel"), WANT_OVERRIDE, "override at the kernel's start")
    stub(vm)
    vm.finish()
    log = vm.log()
    assert log.index("OVERRIDE_REMOVED") < log.index("SPLASH_DRAWN override")
    assert not d.has("glassine/splash/override.gspl"), "override still in the store"
    vm = VM("override-2", d)
    vm.wait("SPLASH_DRAWN splash")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "the splash at the next start")
    stub(vm)
    vm.finish()
    assert "OVERRIDE" not in vm.log()


@case()
def override_on_a_read_only_store_is_not_drawn():
    """011 FR-012, 010 FR-009: a store the loader can't write: no override drawn, no
    count, no request taken; the default set starts."""
    d = installed("readonly")
    d.put("glassine/splash/splash.gspl", SPLASH)
    d.put("glassine/splash/override.gspl", OVERRIDE)
    d.write_both({"request": "recovery"})
    before = d.digest()
    vm = VM("readonly", d, readonly=True)
    vm.wait("OVERRIDE_NOT_REMOVED")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "splash on a read-only store")
    cmd, _ = stub(vm)
    vm.finish()
    assert cmd == d.info["cmdline"], cmd
    assert "REQUEST_NOT_TAKEN" in vm.log()
    assert d.digest() == before


@case("x86_64", "aarch64")
def key_line_list_and_one_shot_pick():
    """010 FR-039 to FR-042, 011 FR-016a: the key opens the list over the picture; the
    previous deployment, and one under "More", start with glassine.deploy.once=; the
    picture is restored when the list closes; nothing is written for the pick."""
    d = installed("list")
    d.put("glassine/splash/splash.gspl", SPLASH)
    d.write_both({"deploys": "3", "deploy1": "d20261101-a previous 3.24.1 2026-11-01",
                  "deploy2": "d20261110-b candidate 3.24.2 2026-11-10", "deploy3": "d20261020-k kept 3.23.4 2026-10-20"})
    vm = VM("list-previous", d)
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait("LIST_SHOWN Start options")
    f = vm.shot("list")
    assert_centred_box(f, WANT_SPLASH, "the list")
    vm.key("down")
    assert diff(wait_frame(vm, "list-down", lambda g: diff(g, f)[0] > 0), f)[0] > 0, "focus did not move"
    vm.key("ret")
    vm.wait(r"CHOICE 1 Previous deployment: 3\.24\.1 2026-11-01")
    vm.wait("LIST_CLOSED")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("after-list"), WANT_SPLASH, "the picture after the list")
    cmd, _ = stub(vm)
    vm.finish()
    assert cmd == d.info["cmdline"] + " glassine.deploy.once=d20261101-a", cmd
    r = d.record()[2]
    assert (r["unmarked"], r["started_why"]) == ("1", "default"), r

    vm = VM("list-more", d)
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait("LIST_SHOWN Start options")
    for k in ("down", "down", "ret"):
        vm.key(k)
    vm.wait("LIST_SHOWN Deployments")
    vm.key("down")
    vm.key("ret")
    vm.wait("CHOICE more 1 Kept: 3.23.4 2026-10-20")
    cmd, _ = stub(vm)
    vm.finish()
    assert cmd == d.info["cmdline"] + " glassine.deploy.once=d20261020-k", cmd


@case()
def previous_kernel_by_key():
    """010 FR-039: "Previous kernel" starts the previous set, counted, started_why=choice;
    a pending request stays."""
    d = installed("prevkey")
    prev = d.add_set(b"previous initrd")
    d.write_both({"previous": prev["id"], "previous_rec": prev["rec"]})
    vm = VM("previous-kernel-key", d)
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait(r"LIST_SHOWN Start options \(3 entries\)")
    vm.key("down")
    vm.key("ret")
    vm.wait("CHOICE 1 Previous kernel")
    cmd, ird = stub(vm)
    vm.finish()
    assert cmd == prev["cmdline"] and ird == initrd_line(prev["initrd"]), (cmd, ird)
    r = d.record()[2]
    assert (r["started"], r["started_why"], r["unmarked"]) == (prev["id"], "choice", "1"), r


@case()
def leave_screen_override():
    """011 FR-014: an override that says to leave the screen: nothing is drawn, no key
    line, through the kernel's start; it is used up."""
    d = installed("leave")
    d.put("glassine/splash/override.gspl", mksplash.leave_screen())
    vm = VM("leave-screen", d)
    vm.wait("WAIT_START")
    before = vm.shot("at-wait-start")
    vm.wait("WAIT_END")
    assert_exact(vm.shot("after-wait"), before, "the screen as the firmware left it")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("kernel"), before, "the screen at the kernel's start")
    stub(vm)
    vm.finish()
    assert "SPLASH_DRAWN leave-screen" in vm.log() and "HINT_SHOWN" not in vm.log()
    assert not d.has("glassine/splash/override.gspl")


@case()
def leave_screen_key_still_opens_list():
    """010 FR-017: with nothing drawn, the key still opens the list, over what the
    screen shows, and the screen is restored after it."""
    d = installed("leave-key")
    d.put("glassine/splash/override.gspl", mksplash.leave_screen())
    vm = VM("leave-screen-key", d)
    vm.wait("WAIT_START")
    before = vm.shot("at-wait-start")
    vm.key("spc")
    vm.wait("LIST_SHOWN Start options")
    assert_centred_box(wait_frame(vm, "list", lambda g: diff(g, before)[0] > 0), before, "the list")
    vm.key("ret")
    vm.wait("CHOICE 0 GlassineOS")
    vm.wait("START_KERNEL")
    assert_exact(vm.shot("after-list"), before, "the screen after the list")
    cmd, _ = stub(vm)
    vm.finish()
    assert cmd == d.info["cmdline"], cmd


@case()
def recovery_by_key():
    """010 Scenario 4, FR-017: the key, then "Recovery": the recovery starts, nothing
    is written."""
    d = installed("reckey")
    before = d.digest()
    vm = VM("recovery-key", d)
    vm.wait("WAIT_START")
    vm.key("spc")
    vm.wait("LIST_SHOWN Start options")
    vm.key("down")
    vm.key("ret")
    vm.wait("CHOICE 1 Recovery")
    cmd, _ = stub(vm)
    vm.finish()
    assert cmd == d.info["recovery_cmdline"], cmd
    assert d.digest() == before


@case()
def no_record_starts_the_recovery():
    """010 FR-008, boot-partition "What the loader does with it": both slots gone; the
    recovery is found by its own record."""
    d = installed("norecord")
    d.rm("glassine/loader.a")
    d.put("glassine/loader.b", b"\n" * 16384)
    before = d.digest()
    vm, cmd, _ = boot_once("no-record", d)
    assert cmd == d.info["recovery_cmdline"], cmd
    assert "NO_RECORD" in vm.log()
    assert d.digest() == before


@case()
def power_cut_after_each_write():
    """Constitution "Tests and hardware": the power cut right after the count write and
    right after the override's removal; the next start reads the new record and shows
    the splash."""
    d = installed("cut")
    d.put("glassine/splash/splash.gspl", SPLASH)
    d.put("glassine/splash/override.gspl", OVERRIDE)
    vm = VM("cut-1", d)
    vm.wait("OVERRIDE_REMOVED")
    vm.kill()
    assert not d.has("glassine/splash/override.gspl")
    vm = VM("cut-2", d)
    vm.wait(r"WRITE slot=b seq=2 ok")
    vm.kill()
    slot, seq, r, _ = d.record()
    assert (slot, seq, r["unmarked"]) == (1, 2, "1"), r
    vm = VM("cut-3", d)
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), WANT_SPLASH, "the splash after the cuts")
    stub(vm)
    vm.finish()
    assert d.record()[:2] == (0, 3)
    # The state a cut right after the request's clearing leaves (the write is too quick
    # to kill QEMU in between; the host tests cut every byte of it): the request
    # cleared, the trial set still named. The next start is the default set.
    t = d.add_set(b"trial initrd")
    cur = d.record()
    lines = set_record(read_slot(d.get("glassine/loader.a"))[1], {"trial": t["id"], "trial_rec": t["rec"], "request": "", "seq": str(cur[1] + 1)})
    d.put("glassine/loader.b", slot_bytes(lines))
    vm, cmd, _ = boot_once("cut-4-after-request", d)
    assert cmd == d.info["cmdline"], cmd


@case()
def two_displays_no_override():
    """011 FR-010: with two displays the splash is drawn on each and no override; the
    override is used up."""
    d = installed("twodisp")
    d.put("glassine/splash/splash.gspl", SPLASH)
    d.put("glassine/splash/override.gspl", OVERRIDE)
    disp = A["display"] + ["-device", f"secondary-vga,id=vga1,xres={W},yres={H},edid=on"]
    vm = VM("two-displays", d, display=disp)
    vm.wait("DISPLAY 1 ")
    vm.wait("override not drawn: 2 displays")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("display0", "vga0"), WANT_SPLASH, "display 0")
    assert_exact(vm.shot("display1", "vga1"), WANT_SPLASH, "display 1")
    stub(vm)
    vm.finish()
    assert not d.has("glassine/splash/override.gspl")


def secure_boot_vars(dirname):
    """OVMF variables with a key of this test run enrolled (generated now, never kept)
    and Secure Boot on; the loader signed with it."""
    os.makedirs(dirname, exist_ok=True)
    key, crt = os.path.join(dirname, "test.key"), os.path.join(dirname, "test.crt")
    run("openssl", "req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=GlassineOS loader test key",
        "-keyout", key, "-out", crt)
    guid = "a5b1e9c3-0d7e-4b2f-8c61-2f3e4d5c6b7a"
    vars_fd = os.path.join(dirname, "vars-sb.fd")
    run("virt-fw-vars", "-i", SB_VARS, "-o", vars_fd, "--set-pk", guid, crt, "--add-kek", guid, crt,
        "--add-db", guid, crt, "--secure-boot")
    signed = os.path.join(dirname, "loader-signed.efi")
    run("sbsign", "--key", key, "--cert", crt, "--output", signed, efi("glassine-loader"))
    os.remove(key)
    return vars_fd, signed


# the build image's (Debian's) Secure Boot firmware, also when QTEST_FW_* names another
SB_CODE = "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd"
SB_VARS = "/usr/share/OVMF/OVMF_VARS_4M.fd"
SB_MACHINE = ["-machine", "smm=on", "-global", "driver=cfi.pflash01,property=secure,value=on"]


@case()
def secure_boot_on_is_refused():
    """010 FR-036, SC-016: an OVMF enforcing Secure Boot with Microsoft's keys doesn't
    start the unsigned loader at all; one that trusts a signed loader gets a refusal
    on screen, nothing read or written, and the loader returns to the firmware."""
    d = installed("sb-ms")
    before = d.digest()
    vm = VM("secure-boot-ms", d, code=SB_CODE, vars_fd="/usr/share/OVMF/OVMF_VARS_4M.ms.fd", machine_extra=SB_MACHINE)
    vm.wait(r"BdsDxe: failed to load Boot\d+ .*: Access Denied", timeout=240)
    vm.kill()
    assert "LOADER_START" not in vm.log(), "the firmware started the unsigned loader"
    assert d.digest() == before

    vars_fd, signed = secure_boot_vars(os.path.join(OUT, "secure-boot-own"))
    d = installed("sb-own", loader=signed)
    d.put("glassine/splash/override.gspl", OVERRIDE)
    before = d.digest()
    vm = VM("secure-boot-own", d, code=SB_CODE, vars_fd=vars_fd, machine_extra=SB_MACHINE)
    vm.wait("LOADER_START")
    vm.wait("SECURE_BOOT on: refusing")
    vm.wait("MESSAGE GlassineOS starts only with Secure Boot off")
    # The message, centred, over what the firmware left on screen.
    blank = [0] * (W * H)
    f = wait_frame(vm, "refusal", lambda g: (diff(g, blank)[1] or (0, 0, 0, 0))[3] > H // 3)
    n, box = diff(f, blank)
    assert n and box[3] > H // 3, f"no message on screen: {box}"
    vm.kill()
    assert "STUB_START" not in vm.log() and "RECORD" not in vm.log()
    assert d.digest() == before, "the boot partition changed"


@case()
def linux_kernel_gets_its_command_line():
    """A real Linux EFI-stub kernel (Alpine linux-lts) and its initramfs, started from
    checked memory: /proc/cmdline is the set's command line."""
    vmlinuz = os.path.join(PAYLOAD, "vmlinuz")
    if not os.path.exists(vmlinuz):
        raise AssertionError("no payload: run make payload")
    d = installed("linux", kernel=open(vmlinuz, "rb").read(), initramfs=open(os.path.join(PAYLOAD, "initrd"), "rb").read())
    vm = VM("linux", d)
    m = vm.wait(r"INIT_CMDLINE: ([^\r\n]*)\r?\n", timeout=300)
    vm.finish(300)
    assert m.group(1).strip() == d.info["cmdline"], m.group(1)
    assert "/etc/glassine-boot/board.conf: " + BOARD.decode().strip() in vm.log(), "board archive not in the initramfs"


@case("x86_64")
def linux_kernel_with_a_large_initramfs():
    """The same with an initramfs the size of a release's (24 MiB of ballast that doesn't
    compress): the loader holds both files in memory and the firmware still loads the
    kernel from its buffer."""
    vmlinuz = os.path.join(PAYLOAD, "vmlinuz")
    if not os.path.exists(os.path.join(PAYLOAD, "initrd-large")):
        raise AssertionError("no payload: run make payload")
    ird = open(os.path.join(PAYLOAD, "initrd-large"), "rb").read()
    assert len(ird) > 20 << 20, len(ird)
    d = installed("linux-large", kernel=open(vmlinuz, "rb").read(), initramfs=ird)
    vm = VM("linux-large", d)
    m = vm.wait(r"INIT_CMDLINE: ([^\r\n]*)\r?\n", timeout=300)
    vm.finish(300)
    assert m.group(1).strip() == d.info["cmdline"], m.group(1)
    assert "/etc/glassine-boot/board.conf: " + BOARD.decode().strip() in vm.log(), "board archive not in the initramfs"


FORMAT2 = [(l, c, f) for l in ("grey8", "rgb888", "bgrx") for c in ("none", "rle", "lz4") for f in ("none", "up")]


@case()
def splash_format_2_every_layout_compression_and_filter():
    """boot-partition.md, "The image format" (format 2): a splash of every pixel layout,
    compression and filter is drawn pixel for pixel (grey as grey), through the start."""
    d = installed("format2")
    for layout, comp, filt in FORMAT2:
        name = f"format2-{layout}-{comp}-{filt}"
        img = encode2(SPL, W, H, (W, H), 0x102040, layout, comp, filt)
        assert (img[8], img[28], img[36], img[38]) == (2, ("bgrx", "rgb888", "grey8").index(layout) + 1,
                                                       ("none", "rle", "lz4").index(comp), ("none", "up").index(filt)), name
        d.put("glassine/splash/splash.gspl", img)
        want = [luma(p) for p in SPL] if layout == "grey8" else SPL
        vm = VM(name, d)
        vm.wait("SPLASH_DRAWN splash")
        vm.wait("HINT_HIDDEN")
        assert_exact(vm.shot("splash"), want, name)
        stub(vm)
        vm.finish()


@case("x86_64", "aarch64")
def splash_format_2_grey_panel_picture_scaled():
    """011 FR-007 with format 2: a grey8 picture of the Note Air's 1404x1872 panel (LZ4 and
    the filter, as the writers' best) on another mode: scaled, centred on its background."""
    w, h = 1404, 1872
    px = [0xF4F2EC if (x // 117 + y // 156) % 2 else 0x202020 for y in range(h) for x in range(w)]
    img = encode2(px, w, h, (w, h), 0x808080, "grey8")
    assert (img[8], img[28]) == (2, 3)
    d = installed("format2-scaled")
    d.put("glassine/splash/splash.gspl", img)
    vm = VM("format2-scaled", d)
    vm.wait("SPLASH_DRAWN splash")
    vm.wait("HINT_HIDDEN")
    assert_exact(vm.shot("splash"), compose([luma(p) for p in px], w, h, (w, h), 0x808080), "grey8, scaled")
    stub(vm)
    vm.finish()


@case()
def splash_format_2_cut_or_unknown_is_skipped():
    """011 SC-005 with format 2: a cut LZ4 stream (its lengths and CRCs made to match)
    and an unknown compression are skipped: the default splash is drawn."""
    import struct
    good = encode2(SPL, W, H, (W, H), 0x102040, "rgb888", "lz4")
    cut = bytearray(good[:64 + (len(good) - 64) // 2])
    cut[40:48] = struct.pack("<Q", len(cut) - 64)
    cut[48:52] = struct.pack("<I", crc32(bytes(cut[64:])))
    cut[52:56] = struct.pack("<I", crc32(bytes(cut[:52])))
    unknown = bytearray(good)
    unknown[36] = 3
    unknown[52:56] = struct.pack("<I", crc32(bytes(unknown[:52])))
    d = installed("format2-bad")
    for name, img, why in (("cut", cut, "Decode"), ("unknown", unknown, "Encoding")):
        d.put("glassine/splash/splash.gspl", bytes(img))
        vm = VM(f"format2-bad-{name}", d)
        vm.wait("SPLASH_DRAWN default")
        vm.wait("HINT_HIDDEN")
        assert_exact(vm.shot("splash"), WANT_DEFAULT, f"the default splash, {name}")
        stub(vm)
        vm.finish()
        assert re.search(r"IMAGE_SKIPPED [^\r\n]*splash\.gspl: " + why, vm.log()), f"{name}: no skip logged"


# ---------------------------------------------------------------- under a parent (boot spec 012)
#
# The disk: an ESP with the stand-in parent (tests/parent) as the removable-media path,
# and the boot partition `glassine-boot` in its raw form (a region table, boot
# docs/boot-partition.md, "The raw form") or its FAT form. The parent starts the loader
# from it as the Note Air's ABL does; its serial lines say what it would boot.

HB_PARTUUID = "8a1f3c5e-2b4d-4e6f-9a0b-1c2d3e4f5a6b"
ESP_OFF, ESP_SIZE = 1 << 20, 32 << 20
HB_OFF, HB_SIZE = ESP_OFF + ESP_SIZE, 16 << 20
HB_FAT_SIZE = 40 << 20  # FAT32 with 512-byte sectors needs about 33 MiB
K, M = 1024, 1 << 20
# boot-partition.md, "Sizes": the 16 MiB table.
REGIONS = {"record": (64 * K, 32 * K), "probe": (128 * K, 64 * K), "loader": (M, 3 * M),
           "splash-default": (4 * M, 3 * M), "splash": (7 * M, 3 * M), "override": (10 * M, 3 * M)}
SET_A, SET_B = "0123456789abcdef", "fedcba9876543210"


def crc32(b):
    import zlib
    return zlib.crc32(b) & 0xFFFFFFFF


def loader_header(img):
    import struct
    h = b"GLLOADER" + struct.pack("<IIQI", 1, 0, len(img), crc32(img))
    h += struct.pack("<I", crc32(h))
    return h + b"\0" * (4096 - len(h))


def region_table(part, regions):
    """The region table (boot-partition.md, "The region table"), as the installer writes it."""
    import struct
    t = bytearray(4096)
    t[0:24] = b"GLREGION" + struct.pack("<IIQ", 1, len(regions), part)
    for i, (name, (off, size)) in enumerate(regions.items()):
        e = name.encode().ljust(32, b"\0") + struct.pack("<QQI", off, size, 0)
        e += struct.pack("<I", crc32(e)) + b"\0" * 8
        t[64 + 64 * i:128 + 64 * i] = e
    t[4092:] = struct.pack("<I", crc32(bytes(t[:4092])))
    return bytes(t)


def hb_record(**keys):
    base = [("format", "1"), ("seq", "1")] + [(k, "") for k in (
        "default", "default_rec", "previous", "previous_rec", "trial", "trial_rec", "recovery", "recovery_rec",
        "request")] + [("unmarked", "0"), ("started", ""), ("started_why", ""), ("deploys", "0")]
    return set_record(base, keys)


class HandbackDisk:
    """`fat`: the FAT form; else raw. `table`: None for the installer's table, False for
    none, or bytes for the primary copy (the backup stays the installer's)."""

    def __init__(self, path, loader=None, header=None, slots=None, cfg="", fat=False, table=None, images=None):
        self.path, self.fat = path, fat
        size = HB_FAT_SIZE if fat else HB_SIZE
        self.size = size
        if os.path.exists(path):
            os.remove(path)
        with open(path, "wb") as f:
            f.truncate(HB_OFF + size + (1 << 20))
        run("sgdisk", "-o", "-n", f"1:{ESP_OFF // SECTOR}:+{ESP_SIZE // SECTOR}", "-t", "1:ef00", "-c", "1:esp",
            "-n", f"2:{HB_OFF // SECTOR}:+{size // SECTOR}", "-t", "2:8300", "-c", "2:glassine-boot",
            "-u", f"2:{HB_PARTUUID}", path)
        esp = f"{path}@@{ESP_OFF}"
        run("mformat", "-i", esp, "-T", str(ESP_SIZE // SECTOR), "::")  # FAT16: 32 MiB is too few clusters for FAT32
        run("mmd", "-i", esp, "::/EFI", "::/EFI/BOOT")
        boot = "BOOTAA64.EFI" if A["target"].startswith("aarch64") else "BOOTX64.EFI"
        run("mcopy", "-i", esp, efi("glassine-test-parent"), f"::/EFI/BOOT/{boot}")
        if cfg:
            self.mput(esp, "parent.cfg", (cfg + "\n").encode())
        img = open(efi("glassine-loader"), "rb").read() if loader is None else loader
        if fat:
            part = self.img()
            run("mformat", "-i", part, "-F", "-T", str(size // SECTOR), "-v", "GLASSINE", "::")
            run("mmd", "-i", part, "::/EFI", "::/EFI/glassine", "::/glassine")
            if img is not False:
                self.mput(part, "EFI/glassine/loader.efi", img)
            for i, s in enumerate(slots or []):
                self.mput(part, f"glassine/loader.{'ab'[i]}", s)
            return
        if table is not False:
            self.put(0, region_table(size, REGIONS) if table is None else table)
            self.put(size - 4096, region_table(size, REGIONS))
        if img is not False:
            self.put(REGIONS["loader"][0] + 4096, img)
            self.put(REGIONS["loader"][0], loader_header(img) if header is None else header)
        for i, s in enumerate(slots or []):
            self.put(REGIONS["record"][0] + i * 16384, s)
        for name, data in (images or {}).items():
            self.put(REGIONS[name][0], data)

    def img(self):
        return f"{self.path}@@{HB_OFF}"

    def mput(self, image, name, data):
        tmp = self.path + ".put"
        with open(tmp, "wb") as f:
            f.write(data)
        run("mcopy", "-o", "-i", image, tmp, "::/" + name)
        os.remove(tmp)

    def mget(self, name):
        r = subprocess.run(["mtype", "-i", self.img(), "::/" + name], capture_output=True)
        return r.stdout if r.returncode == 0 else None

    def put(self, off, data):
        with open(self.path, "r+b") as f:
            f.seek(HB_OFF + off)
            f.write(data)

    def at(self, off, n):
        with open(self.path, "rb") as f:
            f.seek(HB_OFF + off)
            return f.read(n)

    def region(self, name):
        off, size = REGIONS[name]
        return self.at(off, size)

    def slots(self):
        if self.fat:
            return self.mget("glassine/loader.a"), self.mget("glassine/loader.b")
        r = REGIONS["record"][0]
        return self.at(r, 16384), self.at(r + 16384, 16384)

    def record(self):
        a, b = (read_slot(x) for x in self.slots())
        if a and b:
            return (b[0], dict(b[1]), True) if b[0] > a[0] else (a[0], dict(a[1]), True)
        s = a or b
        return (s[0], dict(s[1]), False) if s else None

    def probe(self):
        b = self.mget("glassine/probe.txt") if self.fat else self.region("probe")
        return parse_record(b.rstrip(b"\n") + b"\n") if b and b.strip(b"\0") else None


def hb_disk(name, record=None, **kw):
    slots = [slot_bytes(record)] * 2 if record is not None else None
    return HandbackDisk(disk_path(name), slots=slots, **kw)


def parent_run(name, d):
    vm = VM(name, d)
    vm.wait("PARENT_POWEROFF")
    vm.finish()
    log = vm.log()
    boot = re.findall(r"PARENT_BOOT ([^\r\n]*)", log)[-1]
    floor = re.findall(r"PARENT_FLOOR ([^\r\n]*)", log)
    ans = re.findall(r"PARENT_ANSWER ([^\r\n]*)", log)
    return log, boot, (floor[-1] if floor else None), (ans[-1] if ans else None)


def gspl(w, h, fill):
    """A raw picture of the image format (boot-partition.md, "The image format")."""
    import struct
    payload = bytes([fill]) * (w * h * 4)
    hd = b"GLSPLASH" + struct.pack("<HHIIIIIIIQI", 1, 0, w, h, w, h, 1, 0, 0, len(payload), crc32(payload))
    hd += struct.pack("<I", crc32(hd)) + b"\0" * 8
    return hd + payload


@case("x86_64", "aarch64")
def handback_ordinary_start_counts_and_answers_the_baseline():
    """012 Scenario 1, FR-008 to FR-011, FR-015a: started from memory by a parent from the
    raw form, headless; the start is counted in the region `record`; no probe without
    probe=1; the splash store's regions are left byte for byte."""
    images = {"splash": gspl(16, 8, 0x55), "override": gspl(16, 8, 0x44)}
    d = hb_disk("hb-ordinary", hb_record(), images=images)
    store = {n: d.region(n) for n in ("splash-default", "splash", "override", "probe")}
    log, boot, floor, _ = parent_run("hb-ordinary", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    assert "PARENT_FORM raw" in log and "PARENT_TABLE at=0" in log
    assert "HANDBACK form=raw table=Primary regions=6" in log, "the loader didn't read the table"
    assert "HANDBACK_ANSWER format=1 boot=baseline why=default" in log, "no answer logged"
    assert "DISPLAY" not in log and "HINT_SHOWN" not in log, "a headless loader opened a display"
    seq, r, both = d.record()
    assert (seq, r["unmarked"], r["started_why"], both) == (2, "1", "default", True), (seq, r)
    for n, b in store.items():
        assert d.region(n) == b, f"region {n} changed"


@case("x86_64", "aarch64")
def handback_requests_and_the_limit():
    """012 Scenarios 2 to 4: a recovery request once, a trial by id, the limit."""
    d = hb_disk("hb-recovery", hb_record(request="recovery"))
    _, boot, floor, _ = parent_run("hb-recovery", d)
    assert (boot, floor) == ("recovery", None), (boot, floor)
    seq, r, _ = d.record()
    assert (seq, r["request"], r["unmarked"]) == (2, "", "0"), (seq, r)
    _, boot, _, _ = parent_run("hb-recovery-2", d)
    assert boot == "baseline", "the request was taken twice"

    d = hb_disk("hb-trial", hb_record(request="trial", trial=SET_B, trial_rec="ab" * 32))
    _, boot, floor, ans = parent_run("hb-trial", d)
    assert ans == f"set:{SET_B} rec={'ab' * 32}" and floor is None, (ans, floor)
    seq, r, _ = d.record()
    assert (r["request"], r["started"], r["started_why"], r["unmarked"]) == ("", SET_B, "request", "0"), r

    d = hb_disk("hb-limit", hb_record(unmarked="3"))
    _, boot, _, _ = parent_run("hb-limit", d)
    assert boot == "recovery", boot
    assert d.record()[0] == 1, "a start of the recovery is counted"


@case("x86_64", "aarch64")
def handback_floor_cases():
    """012 Scenario 5, FR-003: every way the loader can't help ends in the baseline."""
    rec = hb_record(request="recovery")
    img = open(efi("glassine-loader"), "rb").read()
    stub_img = open(efi("glassine-test-kernel"), "rb").read()
    bad_primary = bytearray(region_table(HB_SIZE, REGIONS))
    bad_primary[70] ^= 1
    cases = [
        ("hb-missing", dict(loader=False), "header Magic"),
        ("hb-badheader", dict(header=loader_header(img)[:28] + b"\0\0\0\0" + b"\0" * 4064), "header HeaderCrc"),
        ("hb-imagecrc", dict(), "image crc"),
        ("hb-notpe", dict(loader=b"this is not a PE image" * 100), "LoadImage"),
        ("hb-garbage", dict(loader=stub_img, cfg="stub.answer=garbage"), "answer Prefix"),
        ("hb-badcrc", dict(loader=stub_img, cfg="stub.answer=badcrc"), "answer Crc"),
        ("hb-unknown", dict(loader=stub_img, cfg="stub.answer=unknown"), "answer UnknownBoot"),
        ("hb-returns", dict(loader=stub_img, cfg="stub.return"), "answer NoData"),
        ("hb-noloader-region", dict(table=region_table(HB_SIZE, {k: v for k, v in REGIONS.items() if k != "loader"})),
         None),
    ]
    for name, kw, want in cases:
        d = hb_disk(name, rec, **kw)
        if name == "hb-imagecrc":
            o = REGIONS["loader"][0] + 4096 + len(img) // 2
            d.put(o, bytes([img[len(img) // 2] ^ 0xFF]))
        if name == "hb-noloader-region":
            # Only the backup names the loader: break it too, so no copy has a `loader`.
            d.put(HB_SIZE - 4096, region_table(HB_SIZE, {k: v for k, v in REGIONS.items() if k != "loader"}))
            want = "no loader region"
        _, boot, floor, _ = parent_run(name, d)
        assert boot == "baseline" and floor is not None and floor.startswith(want), (name, boot, floor)
        assert d.record()[1]["request"] == "recovery", f"{name}: the record changed"
    # No table at all: the parent's floor, before any loader runs.
    d = hb_disk("hb-notable", rec, table=False)
    d.put(HB_SIZE - 4096, b"\0" * 4096)
    _, boot, floor, _ = parent_run("hb-notable", d)
    assert (boot, floor) == ("baseline", "no region table"), (boot, floor)
    # A damaged primary: parent and loader both use the backup.
    d = hb_disk("hb-backup", rec, table=bytes(bad_primary))
    log, boot, floor, _ = parent_run("hb-backup", d)
    assert (boot, floor) == ("recovery", None), (boot, floor)
    assert f"PARENT_TABLE at={HB_SIZE - 4096}" in log and "table=Backup" in log, "the backup wasn't used"
    assert d.at(0, 4096) == bytes(bad_primary), "a reader wrote the table"
    # A table with no record region, the loader present: it runs, finds no record and
    # writes nothing.
    t = region_table(HB_SIZE, {"loader": REGIONS["loader"]})
    d = hb_disk("hb-norecord", table=t)
    d.put(HB_SIZE - 4096, t)
    before = d.at(0, HB_SIZE)
    log, boot, floor, _ = parent_run("hb-norecord", d)
    assert (boot, floor) == ("baseline", None) and "why=no-record" in log, (boot, floor)
    assert d.at(0, HB_SIZE) == before, "a loader with no record wrote"


@case("x86_64", "aarch64")
def handback_probe_reports_the_firmware():
    """012 Scenario 6, FR-016 to FR-019: probe=1 writes a checked report with the GOP
    modes QEMU offers and the file systems; the answer is unchanged; probe stays set."""
    d = hb_disk("hb-probe", hb_record(probe="1"))
    log, boot, floor, _ = parent_run("hb-probe", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    p = d.probe()
    assert p, "no valid probe report"
    keys = dict(p)
    with open(os.path.join(OUT, "hb-probe", "probe.txt"), "w") as f:
        f.write("".join(f"{k}={v}\n" for k, v in p))
    assert int(keys["handles"]) > 0 and int(keys["protocols"]) > 0, keys
    assert int(keys["gops"]) >= 1, keys
    gop = keys["gop1"]
    cur = re.search(r"current=(\d+)", gop).group(1)
    assert f"{W}x{H}" in keys[f"gop1.mode{cur}"], (gop, keys.get(f"gop1.mode{cur}"))
    n = int(re.search(r"modes=(\d+)", gop).group(1))
    listed = [k for k in keys if re.fullmatch(r"gop1\.mode\d+", k)]
    assert n > 1 and (len(listed) == n or keys.get("truncated") == "1"), (n, len(listed))
    assert any(v.endswith(" graphics-output") for k, v in p if k.startswith("proto")), "GOP not named"
    assert any(k.startswith("log") and "HANDBACK start" in v for k, v in p), "no log lines"
    assert "conout" in keys and "conin" in keys
    # Raw form: no file system on the boot partition; the ESP has one.
    assert keys["boot_fs"] == "0" and int(keys["fs_handles"]) >= 1, keys
    seq, r, _ = d.record()
    assert (r["probe"], r["unmarked"]) == ("1", "1"), r
    assert "PROBE_WRITTEN ok=1" in log


def faketouch_calls(log):
    m = re.findall(r"PARENT_FAKETOUCH calls ([^\r\n]*)", log)
    assert m, "the stand-in touch protocols reported no calls"
    return dict(kv.split("=") for kv in m[-1].split())


def faketouch_seq(log):
    m = re.findall(r"PARENT_FAKETOUCH seq ?([^\r\n]*)", log)
    assert m, "the stand-in touch protocols reported no sequence"
    return m[-1].split()


@case("x86_64", "aarch64")
def handback_probe_reads_the_i2c_and_gpio_protocols():
    """005 Scenario 1, FR-001 to FR-003: without the firmware's I2C and GPIO protocols the
    report says none; with the stand-in, their words, check=ok and the four touch pins'
    levels, from the pin read alone: no bus opened, no pin configured or set."""
    d = hb_disk("hb-probe-noi2c", hb_record(probe="1"))
    log, boot, floor, _ = parent_run("hb-probe-noi2c", d)
    keys = dict(d.probe())
    assert (keys["i2c"], keys["gpio"]) == ("none", "none"), keys
    d = hb_disk("hb-probe-i2c", hb_record(probe="1"), cfg="parent.faketouch=1")
    log, boot, floor, _ = parent_run("hb-probe-i2c", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    keys = dict(d.probe())
    with open(os.path.join(OUT, "hb-probe-i2c", "probe.txt"), "w") as f:
        f.write("".join(f"{k}={v}\n" for k, v in d.probe()))
    for k, n in (("i2c", 6), ("gpio", 8)):
        assert keys[k].startswith("handle=") and "iface=0x" in keys[k], keys[k]
        assert len(keys[f"{k}.words"].split()) == n, keys[f"{k}.words"]
        assert keys[f"{k}.check"] == "ok", (k, keys[f"{k}.check"])
        ins = keys[f"{k}.in"].split()
        assert len(ins) == n - 1 and len(set(ins)) == 1 and ins[0].startswith("image"), ins
    assert keys["i2c.words"].split()[0] == "0x10000" and keys["gpio.words"].split()[0] == "0x1"
    assert [keys[f"gpio.pin{n}"] for n in (61, 62, 66, 67)] == ["0", "0", "0", "1"], keys
    c = faketouch_calls(log)
    assert c["pin_read"] == "4", c
    assert all(c[k] == "0" for k in ("configure", "group", "set", "other", "open", "read", "write")), c
    assert faketouch_seq(log) == ["rd61=0", "rd62=0", "rd66=0", "rd67=1"], faketouch_seq(log)


def pin_ops(seq):
    """The stand-in's output changes per pin, in order: (pin, level)."""
    out = []
    for op in seq:
        m = re.fullmatch(r"set(\d+)=(\d)", op)
        if m and (not out or out[-1] != (int(m[1]), int(m[2]))):
            out.append((int(m[1]), int(m[2])))
    return out


def touch_run(name, mode, record=None, want="ok", budget=10200):
    d = hb_disk(name, record or hb_record(touch="probe"), cfg=f"parent.faketouch={mode}")
    log, boot, floor, _ = parent_run(name, d)
    assert (boot, floor) == ("baseline", None), (name, boot, floor)
    keys = dict(d.probe() or [])
    r = d.record()[1]
    assert r.get("touch", "") == "" and r["unmarked"] == "1", (name, r)
    assert log.index("WRITE slot=") < log.index("TOUCH_GPIO"), f"{name}: drove a pin before the clearing write"
    assert keys.get("touch.result", "").startswith(want), (name, keys.get("touch.result"), log[-1500:])
    ms = int(re.search(r" in (\d+) ms", keys["touch.result"])[1])
    assert ms <= budget, f"{name}: the bring-up took {ms} ms"
    c = faketouch_calls(log)
    assert c["open_now"] == "0", f"{name}: the bus left open: {c}"
    return d, log, keys, c, faketouch_seq(log)


@case("x86_64", "aarch64")
def touch_probe_brings_up_reports_and_powers_down():
    """005 Scenario 2, FR-004 to FR-010: touch=probe without probe=1: the key cleared in
    its own write first; the pins read, the power-up order, reset, the bus opened, the
    descriptor and system information in the report, the power-down; counted and
    answered as without the key."""
    d, log, keys, c, seq = touch_run("touch-probe", "1")
    assert seq[:4] == ["rd61=0", "rd62=0", "rd66=0", "rd67=1"], seq
    assert pin_ops(seq) == [(61, 1), (62, 1), (66, 0), (66, 1), (66, 0), (62, 0), (61, 0)], pin_ops(seq)
    assert "cfg67i" in seq and seq.index("cfg67i") < seq.index("open3") < seq.index("close"), seq
    assert c["powered"] == "0" and c["group"] == "0" and c["other"] == "0", c
    assert "wr0001:" in seq and "wr0004:05002f0002" in seq, seq
    assert keys["touch.found"] == "61=0 62=0 66=0 67=1", keys
    assert keys["touch.power"] == "on: 61, 62" and keys["touch.down"] == "off: 66, 62, 61", keys
    assert keys["touch.open"] == "instance=3 address=0x24 khz=100", keys
    assert keys["touch.hid"].startswith("vendor=0x04b4 product=0xc101"), keys
    assert keys["touch.res"] == "1872x1404 max_tch=10 ask=1", keys
    assert keys["gpio.check"] == "ok" and "gops" in keys, "a full probe report"
    # The next start: touch empty, nothing touched.
    log, boot, floor, _ = parent_run("touch-probe-2", d)
    c = faketouch_calls(log)
    assert c["set"] == c["configure"] == c["open"] == c["pin_read"] == "0", c
    assert d.record()[1]["unmarked"] == "2"


@case("x86_64", "aarch64")
def touch_probe_found_powered_is_left_powered():
    """005 FR-005, FR-008: 61 and 62 read high: the power pins left alone, reset high."""
    d, log, keys, c, seq = touch_run("touch-powered", "powered")
    assert pin_ops(seq) == [(66, 0), (66, 1)], pin_ops(seq)
    assert c["powered"] == "1" and keys["touch.power"] == "found on: left as found", (c, keys)
    assert keys["touch.down"] == "left powered", keys


@case("x86_64", "aarch64")
def touch_probe_fails_at_each_step_and_powers_down():
    """005 Scenario 3, FR-007, FR-008: a failure at each step ends the bring-up there,
    logged, the controller powered down, inside the deadline; the start goes on."""
    for mode, why in (("fail-open", "failed open"), ("fail-write", "failed hid: write"),
                      ("nodesc", "failed hid: no answer"), ("baddesc", "failed hid: not the descriptor"),
                      ("nosysinfo", "failed sysinfo"), ("fail-gpio", "failed power")):
        d, log, keys, c, seq = touch_run(f"touch-{mode}", mode, want=why)
        assert c["powered"] == "0", (mode, c)
        if mode != "fail-gpio":
            assert pin_ops(seq)[-3:] == [(66, 0), (62, 0), (61, 0)], (mode, pin_ops(seq))
    d, log, keys, c, seq = touch_run("touch-sysinfo-retry", "sysinfo-retry")
    assert keys["touch.res"].endswith("ask=3"), keys


@case("x86_64", "aarch64")
def touch_probe_waits_for_a_slow_controller():
    """005 FR-005, FR-007: the reset sentinel, the bootloader's answer to launch-app and
    the application's sentinel are waited for (logged TOUCH_WAIT with their duration);
    writes the controller doesn't take are sent again; all inside the deadline."""
    for mode in ("slow-boot", "nack-first", "late-app"):
        d, log, keys, c, seq = touch_run(f"touch-{mode}", mode)
        ms = lambda k: int(keys[k].split()[0])
        assert keys["touch.launch"].startswith("answer 0b00"), (mode, keys)
        assert keys["touch.wait.reset"].endswith("ok") and " ok" in keys["touch.wait.app"], (mode, keys)
        assert re.search(r"TOUCH_WAIT reset \d+ ms ok", log) and re.search(r"TOUCH_WAIT launch \d+ ms ok", log), mode
        if mode == "slow-boot":
            assert ms("touch.wait.launch") >= 100, keys
        if mode == "late-app":
            assert ms("touch.wait.app") >= 400 and "read_errors=" in keys["touch.wait.app"], keys
            assert seq.index("wr0001:") > seq.index(next(o for o in seq if o.startswith("wr0004:0b00"))), seq
        if mode == "nack-first":
            assert re.search(r"TOUCH_WAIT write 0x0004 \d+ ms ok tries=4", log), log[-2000:]
            assert sum(o.startswith("wr0004:0b00") for o in seq) == 4, seq


@case("x86_64", "aarch64")
def touch_probe_waits_for_sysinfo_like_linux():
    """005 FR-005, FR-007: GET_SYSINFO is sent once and waited for up to 3 s (an answer
    2.5 s late is taken); with no answer it is sent again after 100 ms (an answer to the
    second send is taken); the mode from the descriptor's packet id; inside the 10 s
    deadline."""
    for mode, asks in (("sysinfo-late", 1), ("sysinfo-second", 2)):
        d, log, keys, c, seq = touch_run(f"touch-{mode}", mode, budget=10200)
        assert keys["touch.mode"] == "app", (mode, keys)
        assert keys["touch.res"] == f"1872x1404 max_tch=10 ask={asks}", (mode, keys)
        assert sum(o == "wr0004:05002f0002" for o in seq) == asks, (mode, seq)
        ms = int(keys["touch.wait.sysinfo"].split()[0])
        assert ms >= (2000 if mode == "sysinfo-late" else 300), (mode, keys)
        if mode == "sysinfo-second":
            assert re.search(r"TOUCH_WAIT sysinfo (2[89]\d\d|3\d\d\d) ms timeout", log), log[-3000:]


@case("x86_64", "aarch64")
def touch_probe_a_hang_is_not_repeated():
    """005 Scenario 3, FR-004: an I2C read that never returns: the machine is reset; the
    next start finds touch cleared, makes no GPIO or I2C call and answers. A record
    write that fails: the controller isn't touched."""
    d = hb_disk("touch-hang", hb_record(touch="probe"), cfg="parent.faketouch=hang-read")
    vm = VM("touch-hang", d)
    vm.finish(timeout=150)
    log = vm.log()
    assert "PARENT_FAKETOUCH_HANG" in log and "PARENT_POWEROFF" not in log, log[-800:]
    assert d.record()[1].get("touch", "") == "", d.record()
    log, boot, floor, _ = parent_run("touch-hang-2", d)
    assert boot == "baseline", boot
    c = faketouch_calls(log)
    assert c["set"] == c["configure"] == c["open"] == c["read"] == "0", c
    assert "TOUCH_GPIO" not in log and "TOUCH_I2C" not in log
    d = hb_disk("touch-ro", hb_record(touch="probe"), cfg="parent.faketouch=1")
    vm = VM("touch-ro", d, readonly=True)
    vm.wait("PARENT_POWEROFF")
    vm.finish()
    log = vm.log()
    assert "TOUCH_SKIPPED not-written" in log and "TOUCH_GPIO set" not in log and "TOUCH_GPIO configure" not in log, log[-800:]
    c = faketouch_calls(log)
    assert c["set"] == c["configure"] == c["open"] == "0", c


GOP_GUID = "9042a9de-23dc-4a38-96fb-7aded080516a"
POINTER_PATH = "5c8a3f1e-7b2d-4e9a-8c6f-1d3e5a7b9c0f"  # tests/parent/src/fakefw.rs


def fakefw_calls(log):
    m = re.findall(r"PARENT_FAKEFW calls ([^\r\n]*)", log)
    assert m, "the stand-in firmware reported no calls"
    return dict(kv.split("=") for kv in m[-1].split())


def probe_gops(p):
    """The probe's GOP lines, per GOP: {"gop": ..., "layout": ..., ...}."""
    keys = dict(p)
    out = []
    for n in range(1, int(keys["gops"]) + 1):
        g = {k[len(f"gop{n}."):]: v for k, v in p if k.startswith(f"gop{n}.")}
        g["gop"] = keys[f"gop{n}"]
        out.append(g)
    return out


@case("x86_64", "aarch64")
def handback_probe_reads_an_extended_gop_raw():
    """The probe on firmware with an extended GOP (Mode at +0x28, a refresh at +0x18,
    GetFrameBuffer at +0x20), its console splitter's (NULL at +0x18 and +0x20) and three
    GOPs whose Mode a standard reader must not trust: each is read raw, the layout named,
    GetFrameBuffer called once on the extended one; the refresh, SetMode and Blt never,
    nor any QueryMode of a GOP whose Mode failed its checks. The input devices are listed
    with their identities, the absolute pointer's Mode read from memory, and no Reset or
    GetState of the pointer is called."""
    d = hb_disk("hb-probe-ext", hb_record(probe="1"), cfg="parent.fakefw=1")
    log, boot, floor, _ = parent_run("hb-probe-ext", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    assert "parent.fakefw" not in re.search(r'PARENT_START options="([^"]*)"', log).group(1), "a parent word was passed"
    p = d.probe()
    assert p, "no valid probe report"
    with open(os.path.join(OUT, "hb-probe-ext", "probe.txt"), "w") as f:
        f.write("".join(f"{k}={v}\n" for k, v in p))
    gops = probe_gops(p)
    by = {}
    for g in gops:
        by.setdefault(g["layout"], []).append(g)
    ext = by.get("extended", [])
    assert len(ext) == 1, [g["layout"] for g in gops]
    e = ext[0]
    assert e["path"] == f"VenHw({GOP_GUID})", e["path"]
    assert e["mode_at_0x28"] == "ok 1872x1404 bgr stride=1872", e["mode_at_0x28"]
    assert not e["mode_at_0x18"].startswith("ok"), e["mode_at_0x18"]
    fb = re.search(r"PARENT_FAKEFW installed ext=0x([0-9a-f]+) fb=0x([0-9a-f]+)", log)
    assert e["iface"] == "0x" + fb.group(1), (e["iface"], fb.group(1))
    assert e["framebuffer"] == f"status=SUCCESS fb=0x{fb.group(2)} w=1872 h=1404", e["framebuffer"]
    assert e["mode0"] == "1872x1404 bgr stride=1872", e.get("mode0")
    assert e["in"].split()[3] == e["in"].split()[0] and e["in"].startswith("image"), e["in"]
    assert e["gop"].startswith("handle=") and "modes=1 current=0" in e["gop"], e["gop"]
    assert GOP_GUID in e["guids"] and "09576e91-6d3f-11d2-8e39-00a0c969723b" in e["guids"], e["guids"]
    split = by.get("extended-no-slots", [])
    assert len(split) == 1 and split[0]["path"] == "none" and split[0]["mode_at_0x18"] == "null", split
    assert "framebuffer" not in split[0], "GetFrameBuffer called on a GOP with no slots"
    unk = sorted(g["mode_at_0x18"] for g in by.get("unknown", []))
    assert unk == sorted(["null", "size_of_info=0x9999", "not-ram"]), unk
    assert all("mode0" not in g for g in by["unknown"]), "QueryMode on a GOP with no valid Mode"
    assert by.get("standard"), "the firmware's own GOP isn't standard"
    keys = dict(p)
    assert keys["var.DisplaySplashBufferInfo"] == "absent" and keys["var.DISABLEDISPLAY"] == "absent", keys
    assert any(k.startswith("image") and "base=0x" in v for k, v in p), "no image lines"
    c = fakefw_calls(log)
    assert c == {"query": "2", "set_mode": "0", "blt": "0", "get_fb": "1", "garbage_query": "0", "refresh": "0",
                 "ptr_reset": "0", "ptr_state": "0"}, c
    # The input devices: the stand-in's absolute pointer with its Mode, read from memory
    # (no Reset, no GetState: the counts above), and the text inputs, one of them conin.
    n = int(keys["apointers"])
    ours = [i for i in range(1, n + 1) if keys.get(f"apointer{i}.path") == f"VenHw({POINTER_PATH})"]
    assert len(ours) == 1, [(k, v) for k, v in p if k.startswith("apointer")]
    a = f"apointer{ours[0]}"
    ptr = re.search(r"PARENT_FAKEFW installed [^\r\n]* pointer=0x([0-9a-f]+)", log).group(1)
    assert re.fullmatch(rf"handle=\d+ conin=0 iface=0x{ptr}", keys[a]), keys[a]
    assert keys[f"{a}.mode"] == "min_x=0 max_x=4095 min_y=0 max_y=3071 min_z=0 max_z=0 attr=0x0", keys[f"{a}.mode"]
    ins = keys[f"{a}.in"].split()
    assert len(ins) == 2 and ins[0] == ins[1] and ins[0].startswith("image") and ins[0] in keys, keys[f"{a}.in"]
    assert "8d59d32b-c655-4ae9-9b15-f25904992a43" in keys[f"{a}.guids"], keys[f"{a}.guids"]
    assert int(keys["spointers"]) >= 0 and int(keys["textins"]) >= 1, keys
    t = [keys[f"textin{i}"] for i in range(1, int(keys["textins"]) + 1)]
    assert sum(" conin=1 " in x + " " for x in t) == 1, t
    assert all(f"textin{i}.path" in keys for i in range(1, len(t) + 1)), keys
    assert "PARENT_FAKEFW_REFRESH" not in log, "the probe refreshed the panel"
    assert "PROBE_WRITTEN ok=1" in log


PANEL = (1872, 1404)  # the stand-in's panel, native landscape
# The panel's CRC-32 after the two pictures below are drawn: the goldens both the loader
# and epd_golden must give.
EPD_GOLDEN_SPLASH, EPD_GOLDEN_OVERRIDE = "39c73611", "33306d02"
PORTRAIT = (PANEL[1], PANEL[0])


def portrait_px(w, h, ink, paper):
    """An asymmetric picture: a block at the top left and a bar along the bottom, so a
    wrong rotation or mirror changes the framebuffer."""
    px = []
    for y in range(h):
        for x in range(w):
            if (x < w // 3 and y < h // 4) or y >= h - h // 10:
                px.append(ink)
            else:
                px.append(paper)
    return px


def epd_golden(px, w, h, bg):
    """The CRC-32 of the panel's packed RGB565 after the loader draws `px` (made for the
    portrait mode): composed in portrait, then native (x, y) = (1871 - v, u)."""
    import array
    lw, lh = PORTRAIT
    screen = compose(px, w, h, PORTRAIT, bg, lw, lh)
    nw, nh = PANEL
    out = array.array("H", bytes(nw * nh * 2))
    for y in range(nh):
        base = y * nw
        for x in range(nw):
            p = screen[(nw - 1 - x) * lw + y]
            out[base + x] = (((p >> 19) & 0x1F) << 11) | (((p >> 10) & 0x3F) << 5) | ((p >> 3) & 0x1F)
    return f"{crc32(out.tobytes()):08x}"


def rewrite(d, **changes):
    """The running system's record write: the record in use with `changes`, seq one
    higher, every other key kept (both slots, as glassine-loader-mark's library would
    after two writes)."""
    seq, r, _ = d.record()
    r = dict(r, **changes, seq=str(seq + 1))
    lines = [("format", "1")] + [(k, v) for k, v in r.items() if k != "format"]
    for i in range(2):
        d.put(REGIONS["record"][0] + i * 16384, slot_bytes(lines))


def mark_healthy(d):
    """glassine-loader-mark healthy: unmarked=0, nothing else."""
    rewrite(d, unmarked="0")


def display_retry(d):
    """glassine-loader-mark display-retry: the sticky display_failed cleared."""
    rewrite(d, display_failed="", drew="")


def refreshes(log):
    return re.findall(r"PARENT_FAKEFW_REFRESH ([^\r\n]*)", log)


@case("x86_64", "aarch64")
def handback_epd_fw_draws_the_splash_once():
    """display=epd-fw on firmware with an extended GOP: the splash is composed in
    portrait, written into the framebuffer as packed RGB565 in the panel's landscape and
    refreshed once for the whole panel; no SetMode, no Blt; the answer is unchanged.
    Without the key, or without an extended GOP, the loader stays headless."""
    sw, sh = 600, 800
    spx = portrait_px(sw, sh, 0x000000, 0xC0C0C0)
    splash = mksplash.encode(spx, sw, sh, PORTRAIT, 0xFFFFFF)
    d = hb_disk("hb-epd", hb_record(display="epd-fw"), cfg="parent.fakefw=1", images={"splash": splash})
    log, boot, floor, _ = parent_run("hb-epd", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    c = fakefw_calls(log)
    assert (c["refresh"], c["set_mode"], c["blt"], c["get_fb"]) == ("1", "0", "0", "1"), c
    want = epd_golden(spx, sw, sh, 0xFFFFFF)
    assert want == EPD_GOLDEN_SPLASH, f"the Python twin changed: {want}"
    assert refreshes(log) == [f"this=1 fb=1 rect=0,0,1872,1404 stride=1872 mode=0 crc={want}"], (refreshes(log), want)
    assert "SPLASH_DRAWN splash" in log and "EPD_FW " in log, "no draw logged"
    assert re.search(r"EPD_FW epd-fw [^\r\n]* bound=mode", log), "the Mode's bound not logged"
    assert "HINT_SHOWN" not in log, "a key line on a backend with no key"
    seq, r, _ = d.record()
    assert (seq, r["unmarked"], r["drew"]) == (3, "1", ""), (seq, r)
    w, start = log.index("WRITE slot="), log.index("PARENT_FAKEFW_REFRESH_START")
    assert w < start, "the refresh ran before the count was written"

    # The key without the firmware: headless.
    d = hb_disk("hb-epd-nofw", hb_record(display="epd-fw"), images={"splash": splash})
    log, boot, floor, _ = parent_run("hb-epd-nofw", d)
    assert (boot, floor) == ("baseline", None) and "EPD_FW_NONE" in log and "SPLASH_DRAWN" not in log, log[-400:]
    # The firmware without the key: headless, the store untouched.
    d = hb_disk("hb-epd-nokey", hb_record(), cfg="parent.fakefw=1", images={"splash": splash})
    log, boot, floor, _ = parent_run("hb-epd-nokey", d)
    assert fakefw_calls(log)["refresh"] == "0" and fakefw_calls(log)["get_fb"] == "0", fakefw_calls(log)


@case("x86_64", "aarch64")
def handback_epd_fw_bound_is_the_splash_buffer_info():
    """The Note Air's shape (parent.fakefw=device): GetFrameBuffer gives the 1872x1404
    panel, the Mode a 325x1423 framebuffer (stride 325, 1,849,900 bytes) too small for
    its picture, and DisplaySplashBufferInfo (version 1, the base, 0x02300000 bytes)
    names a region that holds it: the loader draws, with the CRC of the panel's shape,
    and logs that bound. The variable missing, of version 2, 11 bytes long, or naming a
    region one byte too small: headless, the override left in place."""
    sw, sh = 600, 800
    spx = portrait_px(sw, sh, 0x000000, 0xC0C0C0)
    images = {"splash": mksplash.encode(spx, sw, sh, PORTRAIT, 0xFFFFFF),
              "override": mksplash.encode(portrait_px(400, 300, 0x404040, 0xFFFFFF), 400, 300, PORTRAIT, 0x808080)}
    d = hb_disk("hb-epd-dev", hb_record(display="epd-fw"), cfg="parent.fakefw=device", images={"splash": images["splash"]})
    log, boot, floor, _ = parent_run("hb-epd-dev", d)
    assert (boot, floor) == ("baseline", None), (boot, floor)
    fb = re.search(r"PARENT_FAKEFW installed ext=0x[0-9a-f]+ fb=0x([0-9a-f]+) var=([0-9a-f]+)", log)
    assert fb, "no device-shape stand-in"
    base = int(fb.group(1), 16)
    assert fb.group(2) == struct.pack("<III", 1, base, 0x02300000).hex(), fb.group(2)
    c = fakefw_calls(log)
    assert (c["refresh"], c["set_mode"], c["blt"], c["get_fb"]) == ("1", "0", "0", "1"), c
    assert refreshes(log) == [f"this=1 fb=1 rect=0,0,1872,1404 stride=1872 mode=0 crc={EPD_GOLDEN_SPLASH}"], refreshes(log)
    assert re.search(r"EPD_FW epd-fw 1404x1872 [^\r\n]* bound=DisplaySplashBufferInfo", log), "the variable's bound not logged"
    assert "SPLASH_DRAWN splash" in log, log[-600:]

    whys = {"novar": "no DisplaySplashBufferInfo",
            "v2": "a DisplaySplashBufferInfo of 12 bytes that names no region",
            "short": "a DisplaySplashBufferInfo of 11 bytes that names no region",
            "small": "DisplaySplashBufferInfo's 0x{base:x}+5256575"}
    for var, why in whys.items():
        name = f"hb-epd-dev-{var}"
        d = hb_disk(name, hb_record(display="epd-fw"), cfg=f"parent.fakefw=device-{var}", images=images)
        before = d.region("override")
        log, boot, floor, _ = parent_run(name, d)
        assert (boot, floor) == ("baseline", None), (name, boot, floor)
        base = int(re.search(r"PARENT_FAKEFW installed ext=0x[0-9a-f]+ fb=0x([0-9a-f]+)", log).group(1), 16)
        why = why.format(base=base)
        want = f"EPD_FW_NONE framebuffer 0x{base:x}+5256576 outside the Mode's 0x{base:x}+1849900 and {why}: headless"
        assert want in log, (name, want, log[-800:])
        c = fakefw_calls(log)
        assert (c["refresh"], c["get_fb"], c["set_mode"], c["blt"]) == ("0", "1", "0", "0"), (name, c)
        assert "SPLASH_DRAWN" not in log and "OVERRIDE_REMOVED" not in log, name
        assert d.region("override") == before, f"{name}: the override was touched"

    # The probe reads the stand-in's variable as it reads the device's, and draws nothing.
    d = hb_disk("hb-epd-dev-probe", hb_record(probe="1"), cfg="parent.fakefw=device")
    log, boot, floor, _ = parent_run("hb-epd-dev-probe", d)
    base = int(re.search(r"PARENT_FAKEFW installed ext=0x[0-9a-f]+ fb=0x([0-9a-f]+)", log).group(1), 16)
    keys = dict(d.probe())
    want = "present size=12 attr=0x6 hex=" + struct.pack("<III", 1, base, 0x02300000).hex()
    assert keys["var.DisplaySplashBufferInfo"] == want, keys["var.DisplaySplashBufferInfo"]
    ext = [g for g in probe_gops(d.probe()) if g["layout"] == "extended"]
    assert len(ext) == 1 and ext[0]["mode_at_0x28"] == "ok 325x1423 bgr stride=325", ext
    assert ext[0]["framebuffer"] == f"status=SUCCESS fb=0x{base:x} w=1872 h=1404", ext[0]["framebuffer"]
    assert fakefw_calls(log)["refresh"] == "0" and "PARENT_FAKEFW_REFRESH" not in log, "the probe refreshed the panel"


@case("x86_64", "aarch64")
def handback_epd_fw_hang_is_counted_then_skipped():
    """A refresh that never returns (the stand-in hangs; the parent's watchdog resets):
    the start was counted before the draw (drew=1). The next start doesn't draw and sets
    the sticky display_failed; the count reaches the limit; a healthy mark doesn't bring
    the draw back; display-retry plus a healthy mark does (and it hangs again, counted
    first)."""
    splash = mksplash.encode(portrait_px(600, 800, 0, 0xC0C0C0), 600, 800, PORTRAIT, 0xFFFFFF)
    d = hb_disk("hb-epd-hang", hb_record(display="epd-fw"), cfg="parent.fakefw=hang", images={"splash": splash})

    def hung(name):
        vm = VM(name, d)
        vm.finish(timeout=150)
        log = vm.log()
        assert "PARENT_FAKEFW_HANG" in log, f"{name}: the stand-in didn't hang"
        assert "PARENT_RETURNED" not in log and "PARENT_POWEROFF" not in log, f"{name}: returned from a hung refresh"
        assert log.index("WRITE slot=") < log.index("PARENT_FAKEFW_REFRESH_START"), f"{name}: drawn before counted"
        r = d.record()[1]
        assert (r["unmarked"], r["drew"], r.get("display_failed", "")) == ("1", "1", ""), (name, r)

    def skipped(name, want_boot, want_unmarked, why):
        log, boot, floor, _ = parent_run(name, d)
        assert (boot, floor) == (want_boot, None), (name, boot, floor)
        assert f"EPD_FW_SKIPPED {why}" in log, (name, log[-800:])
        assert fakefw_calls(log)["refresh"] == "0" and "PARENT_FAKEFW_REFRESH_START" not in log, name
        r = d.record()[1]
        assert r["unmarked"] == want_unmarked and r["display_failed"] == "1" and r.get("drew", "") == "", (name, r)

    hung("hb-epd-hang")
    skipped("hb-epd-hang-2", "baseline", "2", "the previous start drew and wasn't marked healthy: display_failed=1")
    skipped("hb-epd-hang-3", "baseline", "3", "display_failed=1")
    skipped("hb-epd-hang-4", "recovery", "3", "display_failed=1")
    mark_healthy(d)
    skipped("hb-epd-hang-healthy", "baseline", "1", "display_failed=1")
    display_retry(d)
    mark_healthy(d)
    hung("hb-epd-hang-retry")


@case("x86_64", "aarch64")
def handback_epd_fw_override_removed_before_drawn():
    """The override (made for the portrait mode) is removed durably, then drawn with the
    one refresh; the next start draws the splash."""
    sw, sh = 600, 800
    spx = portrait_px(sw, sh, 0x000000, 0xC0C0C0)
    opx = portrait_px(400, 300, 0x404040, 0xFFFFFF)
    images = {"splash": mksplash.encode(spx, sw, sh, PORTRAIT, 0xFFFFFF),
              "override": mksplash.encode(opx, 400, 300, PORTRAIT, 0x808080)}
    d = hb_disk("hb-epd-ovr", hb_record(display="epd-fw"), cfg="parent.fakefw=1", images=images)
    log, _, floor, _ = parent_run("hb-epd-ovr", d)
    assert floor is None and "OVERRIDE_REMOVED" in log, log[-600:]
    assert log.index("OVERRIDE_REMOVED") < log.index("SPLASH_DRAWN override"), "drawn before removed"
    want = epd_golden(opx, 400, 300, 0x808080)
    assert want == EPD_GOLDEN_OVERRIDE, f"the Python twin changed: {want}"
    assert [r.split("crc=")[1] for r in refreshes(log)] == [want], (refreshes(log), want)
    assert d.region("override")[:64] == b"\0" * 64, "the override is still there"
    mark_healthy(d)
    log, _, _, _ = parent_run("hb-epd-ovr-2", d)
    assert [r.split("crc=")[1] for r in refreshes(log)] == [epd_golden(spx, sw, sh, 0xFFFFFF)], refreshes(log)


# ---- the start menu (spec 004) -------------------------------------------------------

POINTER_RANGE = (4095, 3071)  # the stand-in's max_x, max_y (min 0)
ROW_H = 144  # loader/src/epdmenu.rs


def tap_xy(u, v):
    """The stand-in pointer's position for portrait (u, v): the inverse of the loader's
    mapping with the identity orientation (core::menu::from_portrait)."""
    nw, nh = PANEL
    nx, ny = nw - 1 - v, u
    mx, my = POINTER_RANGE
    up = lambda n, span, out: min((n * span + out - 2) // (out - 1), span)
    return up(nx, mx, nw), up(ny, my, nh)


def row_tap(i, after):
    """A tap in the middle of menu row i (0 the first entry), once the loader has made
    `after` refreshes."""
    x, y = tap_xy(PORTRAIT[0] // 2, ROW_H * (i + 2) + ROW_H // 2)
    return f"{after}:{x}:{y}"


def frames(d, name):
    """The stand-in's frame dumps (parent.fbdump=1), each saved as a portrait PNG."""
    out, k = [], 1
    while True:
        r = subprocess.run(["mtype", "-i", f"{d.path}@@{ESP_OFF}", f"::/fb{k}.raw"], capture_output=True)
        if r.returncode != 0 or not r.stdout:
            break
        nw, nh = PANEL
        im = Image.frombytes("RGB", (nw, nh), r.stdout, "raw", "BGR;16")
        im.transpose(Image.ROTATE_90).save(os.path.join(OUT, name, f"frame{k}.png"))
        out.append(r.stdout)
        k += 1
    return out


def menu_run(name, d, timeout=400):
    vm = VM(name, d)
    vm.wait("PARENT_POWEROFF", timeout=timeout)
    vm.finish()
    log = vm.log()
    boot = re.findall(r"PARENT_BOOT ([^\r\n]*)", log)[-1]
    ans = re.findall(r"HANDBACK_ANSWER ([^\r\n]*)", log)
    return log, boot, (ans[-1] if ans else None)


GOLDEN_MENU = os.path.join(ROOT, "tests", "golden", "epd-menu.txt")


def golden(name, crcs):
    """The framebuffer CRCs after each refresh of a case, against tests/golden/epd-menu.txt
    (QTEST_BLESS=1 writes them). The frames are also kept as PNGs in out/test/<case>/."""
    lines = {}
    if os.path.exists(GOLDEN_MENU):
        for l in open(GOLDEN_MENU):
            if l.strip() and not l.startswith("#"):
                k, v = l.split(None, 1)
                lines[k] = v.split()
    if os.environ.get("QTEST_BLESS") == "1":
        lines[name] = crcs
        with open(GOLDEN_MENU, "w") as f:
            f.write("# The stand-in's CRC-32 of the whole framebuffer after each refresh of a start-menu\n"
                    "# case (tests/qemu/run.py, golden()); the same on every arch and firmware.\n")
            for k in sorted(lines):
                f.write(f"{k} {' '.join(lines[k])}\n")
        return
    assert lines.get(name) == crcs, f"{name}: frames {crcs}, golden {lines.get(name)}"


def menu_rects(log):
    """Each refresh's (x, y, w, h), checked aligned and inside the panel."""
    out = []
    for r in refreshes(log):
        x, y, w, h = map(int, re.search(r"rect=(\d+),(\d+),(\d+),(\d+)", r).groups())
        assert x % 8 == 0 and w % 8 == 0 and w > 0 and h > 0, r
        assert x + w <= PANEL[0] and y + h <= PANEL[1], r
        assert "stride=1872 mode=0" in r and "this=1 fb=1" in r, r
        out.append((x, y, w, h))
    return out


def crcs(log):
    return [r.split("crc=")[1] for r in refreshes(log)]


FULL = (0, 0, PANEL[0], PANEL[1])
D1, D2 = "d1", "d2"
MENU_REC = dict(display="epd-fw", request="menu", default=SET_A, default_rec="aa" * 32, previous=SET_B,
                previous_rec="bb" * 32, deploys="2", deploy1=f"{D1} previous 3.24.1 2026-11-01",
                deploy2=f"{D2} candidate 3.24.2 2026-11-10")
MENU_SPLASH = None


def menu_disk(name, taps=(), extra="", **keys):
    global MENU_SPLASH
    if MENU_SPLASH is None:
        MENU_SPLASH = mksplash.encode(portrait_px(600, 800, 0, 0xC0C0C0), 600, 800, PORTRAIT, 0xFFFFFF)
    cfg = f"parent.fakefw=1 parent.fbdump=1 parent.taps={','.join(taps)} {extra}".strip()
    return hb_disk(name, hb_record(**keys), cfg=cfg, images={"splash": MENU_SPLASH})


def menu_case(name, taps=(), extra="", **keys):
    d = menu_disk(name, taps, extra, **keys)
    log, boot, ans = menu_run(name, d)
    frames(d, name)
    return d, log, boot, ans


@case("x86_64", "aarch64")
def menu_on_request_each_entry_by_tap():
    """004 Scenarios 1, 3 and 6: request=menu opens the menu (the menu write before the
    first refresh, the whole panel first); a tap on each entry gives its answer and its
    write, one partial refresh for the tap, then the splash; the frames match the goldens."""
    want = {
        # name: (taps, answer, cmdline_add, unmarked, started, started_why)
        "glassineos": ([row_tap(0, 1)], f"boot=set:{SET_A}", None, "1", SET_A, "default"),
        "prevdeploy": ([row_tap(1, 1)], f"boot=set:{SET_A}", f"glassine.deploy.once={D1}", "1", SET_A, "default"),
        "more": ([row_tap(2, 1), row_tap(0, 3)], f"boot=set:{SET_A}", f"glassine.deploy.once={D2}", "1", SET_A, "default"),
        "moreback": ([row_tap(2, 1), row_tap(1, 3), row_tap(4, 4)], "boot=recovery", None, "0", "", ""),
        "prevkernel": ([row_tap(3, 1)], f"boot=set:{SET_B}", None, "1", SET_B, "choice"),
        "recovery": ([row_tap(4, 1)], "boot=recovery", None, "0", "", ""),
    }
    for n, (taps, ans, add, unmarked, started, why) in want.items():
        name = f"menu-{n}"
        d, log, boot, got = menu_case(name, taps, **MENU_REC, probe="1" if n == "glassineos" else "")
        assert got and got.startswith(f"format=1 {ans}"), (name, got)
        assert ("PARENT_CMDLINE_ADD " + add in log) if add else "PARENT_CMDLINE_ADD" not in log, (name, add)
        assert "MENU_OPEN why=request partial=1" in log, name
        w1, first = log.index("WRITE slot="), log.index("PARENT_FAKEFW_REFRESH_START")
        assert w1 < first, f"{name}: refreshed before the menu write"
        assert len(re.findall(r"PARENT_FAKEFW_TAP", log)) == len(taps), (name, "not every tap was read")
        assert "MENU_INPUT firmware-pointer" in log and "TOUCH_SKIPPED the console splitter's pointer" in log, name
        rects = menu_rects(log)
        assert rects[0] == FULL and rects[-1] == FULL, (name, rects)
        # A tap that moves the focus refreshes only the rows it changed (none for the
        # entry already focused); a new list refreshes the whole panel.
        partial = [r for r in rects[1:-1] if r != FULL]
        assert all(r[2] < PANEL[0] for r in partial) and (bool(partial) == (n != "glassineos")), (name, rects)
        seq, r, _ = d.record()
        assert seq == 3, (name, seq, "the menu write and the pick's write")
        assert (r["request"], r["unmarked"], r["drew"], r["started"], r["started_why"]) == ("", unmarked, "", started, why), (name, r)
        assert crcs(log)[-1] == EPD_GOLDEN_SPLASH, (name, "the splash after the pick")
        golden(name, crcs(log))
        if n == "glassineos":
            # 004 FR-014: the probe written again after the menu, its log with the pick.
            p = d.probe()
            assert p and any(k.startswith("log") and "MENU_PICK 0 GlassineOS" in v for k, v in p), "no menu lines in the probe"
            assert log.count("PROBE_WRITTEN ok=1") == 2, "the probe wasn't written again"
            assert fakefw_calls(log)["ptr_reset"] == "0", "Reset called on the pointer"


TOUCH_RES = (1872, 1404)  # the touch stand-in's system information


def touch_tap(i, after):
    """A tap of the touch stand-in in the middle of menu row i: the inverse of the loader's
    mapping (src/touch.rs ORIENT: native y flipped) at the controller's resolution."""
    u, v = PORTRAIT[0] // 2, ROW_H * (i + 2) + ROW_H // 2
    nw, nh = PANEL
    nx, ny = nw - 1 - v, nh - 1 - u
    up = lambda n, span, out: min((n * span + out - 2) // (out - 1), span)
    return f"{after}:{up(nx, TOUCH_RES[0] - 1, nw)}:{up(ny, TOUCH_RES[1] - 1, nh)}"


def touch_menu(name, taps, mode="1", **keys):
    extra = f"parent.nopointer=1 parent.faketouch={mode} parent.touchtaps={','.join(taps)}"
    d = menu_disk(name, extra=extra, touch="on", **MENU_REC, **keys)
    log, boot, ans = menu_run(name, d)
    return d, log, boot, ans


@case("x86_64", "aarch64")
def touch_on_each_entry_by_tap():
    """005 Scenario 4, FR-011 to FR-015: touch=on and no firmware pointer: the loader's
    driver is the menu's tap source, brought up after the menu write; a tap on each entry
    gives its answer and write; More and Back work; the controller is powered down before
    the answer."""
    want = {
        "glassineos": ([touch_tap(0, 1)], f"boot=set:{SET_A}", None),
        "prevdeploy": ([touch_tap(1, 1)], f"boot=set:{SET_A}", f"glassine.deploy.once={D1}"),
        "more": ([touch_tap(2, 1), touch_tap(0, 3)], f"boot=set:{SET_A}", f"glassine.deploy.once={D2}"),
        "moreback": ([touch_tap(2, 1), touch_tap(1, 3), touch_tap(4, 4)], "boot=recovery", None),
        "prevkernel": ([touch_tap(3, 1)], f"boot=set:{SET_B}", None),
        "recovery": ([touch_tap(4, 1)], "boot=recovery", None),
    }
    for n, (taps, ans, add) in want.items():
        name = f"touch-menu-{n}"
        d, log, boot, got = touch_menu(name, taps)
        assert got and got.startswith(f"format=1 {ans}"), (name, got, log[-1500:])
        assert ("PARENT_CMDLINE_ADD " + add in log) if add else "PARENT_CMDLINE_ADD" not in log, (name, add)
        assert "MENU_INPUT loader-cyttsp5" in log, name
        assert len(re.findall(r"PARENT_FAKETOUCH_TAP", log)) == len(taps), (name, "not every tap was read")
        assert len(re.findall(r"TOUCH press raw=", log)) == len(taps), name
        w1 = log.index("WRITE slot=")
        assert w1 < log.index("TOUCH_GPIO set"), f"{name}: the controller touched before the menu write"
        assert log.index("TOUCH_DOWN off: 66, 62, 61") < log.index("HANDBACK_ANSWER"), f"{name}: not powered down first"
        c = faketouch_calls(log)
        assert c["powered"] == "0" and c["open_now"] == "0", (name, c)
        assert d.record()[1].get("touch") == "on", "touch=on stays"


@case("x86_64", "aarch64")
def touch_on_without_the_protocols_or_with_a_failed_bring_up():
    """005 FR-011, FR-014: touch=on without the firmware's protocols: no tap source, no
    menu (MENU_NO_INPUT). With a bring-up that fails: the menu without taps, the
    controller powered down, the default after the timeout."""
    d = menu_disk("touch-menu-none", extra="parent.nopointer=1", touch="on", **MENU_REC)
    log, boot, ans = menu_run("touch-menu-none", d)
    assert "TOUCH_SKIPPED gpio none" in log and "MENU_NO_INPUT why=request" in log, log[-800:]
    d, log, boot, ans = touch_menu("touch-menu-fail", [touch_tap(4, 1)], mode="nodesc")
    assert "TOUCH bring-up failed: no taps" in log and "MENU_TIMEOUT" in log, log[-1500:]
    assert ans.startswith(f"format=1 boot=set:{SET_A} "), ans
    c = faketouch_calls(log)
    assert c["powered"] == "0" and c["open_now"] == "0", c


@case("x86_64", "aarch64")
def menu_full_refreshes_with_display_partial_0():
    """004 Scenario 6: display_partial=0: every refresh is the whole panel, and the frames
    are the same as with partial refreshes."""
    d, log, boot, ans = menu_case("menu-full", [row_tap(4, 1)], **MENU_REC, display_partial="0")
    assert ans.startswith("format=1 boot=recovery"), ans
    assert "MENU_OPEN why=request partial=0" in log
    assert all(r == FULL for r in menu_rects(log)) and len(menu_rects(log)) == 3, menu_rects(log)
    golden("menu-full", crcs(log))
    golden("menu-recovery", crcs(log))


@case("x86_64", "aarch64")
def menu_after_an_unmarked_start_only():
    """004 Scenario 2, FR-001, FR-002, FR-007: a start that drew the splash clears drew
    after the refresh; the next start (not marked healthy) opens the menu by itself; a
    start with unmarked 0 and no request opens none; request=menu without the display is
    cleared."""
    d = menu_disk("menu-unmarked", [row_tap(0, 1)], display="epd-fw")
    log, boot, ans = menu_run("menu-unmarked", d)
    assert "MENU_OPEN" not in log and menu_rects(log) == [FULL], "a menu without a request"
    seq, r, _ = d.record()
    assert (seq, r["unmarked"], r["drew"]) == (3, "1", ""), (seq, r)
    assert log.index("SPLASH_DRAWN") < log.rindex("WRITE slot="), "drew cleared before the draw"
    log, boot, ans = menu_run("menu-unmarked-2", d)
    assert "MENU_OPEN why=unmarked" in log, log[-600:]
    assert ans.startswith("format=1 boot=baseline why=default"), ans
    seq, r, _ = d.record()
    assert (seq, r["unmarked"], r["drew"]) == (5, "2", ""), (seq, r)
    # request=menu without display=epd-fw: no menu, cleared.
    d = hb_disk("menu-nodisplay", hb_record(request="menu"), cfg="parent.fakefw=1")
    log, boot, _, _ = parent_run("menu-nodisplay", d)
    assert boot == "baseline" and "MENU_OPEN" not in log and fakefw_calls(log)["refresh"] == "0", log[-400:]
    assert d.record()[1]["request"] == "", "request=menu left"


@case("x86_64", "aarch64")
def menu_timeout_takes_the_default_under_the_watchdog():
    """004 Scenario 4, FR-012, FR-013: no input for 60 s: the GlassineOS pick; the
    parent's 30 s watchdog never fires (the loader re-arms it), and it is 30 s when the
    loader answers."""
    d = menu_disk("menu-timeout", **MENU_REC)
    vm = VM("menu-timeout", d)
    vm.wait("EPD_REFRESH 1 ", timeout=300)
    # FR-010: keys are logged and do nothing: no move, no pick, no new timeout.
    for k in ("down", "down", "ret"):
        vm.key(k)
    vm.wait("PARENT_POWEROFF", timeout=400)
    vm.finish()
    log = vm.log()
    frames(d, "menu-timeout")
    boot = re.findall(r"PARENT_BOOT ([^\r\n]*)", log)[-1]
    ans = re.findall(r"HANDBACK_ANSWER ([^\r\n]*)", log)[-1]
    keys = re.findall(r"KEY scan=0x([0-9a-f]{4}) char=0x([0-9a-f]{4})", log)
    assert keys == [("0002", "0000"), ("0002", "0000"), ("0000", "000d")], keys
    assert "MENU_PICK" not in log and len(menu_rects(log)) == 2, menu_rects(log)
    assert "MENU_TIMEOUT 60 s: GlassineOS" in log, log[-600:]
    assert ans.startswith(f"format=1 boot=set:{SET_A}") and boot == "baseline", ans
    t0 = int(re.search(r"\[loader +(\d+)\] MENU_OPEN", log).group(1))
    t1 = int(re.search(r"\[loader +(\d+)\] MENU_TIMEOUT", log).group(1))
    assert t1 - t0 >= 60000, (t0, t1)
    menu = log[log.index("MENU_SHOWN"):log.index("HANDBACK_ANSWER")]
    arms = re.findall(r"PARENT_WATCHDOG (\d+)", menu)
    assert len(arms) >= 6 and set(arms) == {"30"}, arms
    assert "PARENT_WATCHDOG 0" not in log[:log.index("HANDBACK_ANSWER")], "the loader disarmed the watchdog"
    last = re.findall(r"PARENT_WATCHDOG (\d+)", log[:log.index("HANDBACK_ANSWER")])[-1]
    assert last == "30", last
    seq, r, _ = d.record()
    assert (r["unmarked"], r["started_why"], r["drew"]) == ("1", "default", ""), r
    golden("menu-timeout", crcs(log))


@case("x86_64", "aarch64")
def menu_without_a_tap_source():
    """004 Scenario 5, FR-001: firmware with no tap source (no absolute pointer of a
    device's own; the console splitter's doesn't count): no menu. On request: logged
    MENU_NO_INPUT, the request cleared, the default at once, counted, the splash of 003.
    After an unmarked start: the same without a draw (003 draws only at unmarked 0)."""
    d = menu_disk("menu-noinput", extra="parent.nopointer=1", **MENU_REC, probe="1")
    log, boot, ans = menu_run("menu-noinput", d)
    assert "MENU_NO_INPUT why=request" in log and "MENU_OPEN" not in log, log[-800:]
    # 004 FR-014: the probe written again, its log with MENU_NO_INPUT.
    assert log.count("PROBE_WRITTEN ok=1") == 2, "the probe wasn't written again"
    assert any(k.startswith("log") and "MENU_NO_INPUT" in v for k, v in d.probe()), "no MENU_NO_INPUT in the probe"
    assert ans.startswith(f"format=1 boot=set:{SET_A} ") and " why=default " in ans, ans
    assert menu_rects(log) == [FULL] and crcs(log) == [EPD_GOLDEN_SPLASH], refreshes(log)
    seq, r, _ = d.record()
    assert (seq, r["request"], r["unmarked"], r["drew"], r["started_why"]) == (3, "", "1", "", "default"), (seq, r)
    log, boot, ans = menu_run("menu-noinput-2", d)
    assert "MENU_NO_INPUT why=unmarked" in log and "MENU_OPEN" not in log, log[-800:]
    assert "EPD_FW_SKIPPED" in log and menu_rects(log) == [], refreshes(log)
    seq, r, _ = d.record()
    assert (r["unmarked"], r["display_failed"] if "display_failed" in r else "") == ("2", ""), r


@case("x86_64", "aarch64")
def menu_hang_is_counted_then_skipped():
    """004 Scenario 7: a refresh in the menu that never returns: the menu write (the
    request cleared, counted, drew=1) came first, the parent's watchdog resets; the next
    start draws nothing and sets display_failed."""
    d = hb_disk("menu-hang", hb_record(display="epd-fw", request="menu"), cfg="parent.fakefw=hang",
                images={"splash": mksplash.encode(portrait_px(600, 800, 0, 0xC0C0C0), 600, 800, PORTRAIT, 0xFFFFFF)})
    vm = VM("menu-hang", d)
    vm.finish(timeout=200)
    log = vm.log()
    assert "PARENT_FAKEFW_HANG" in log and "MENU_OPEN" in log and "PARENT_RETURNED" not in log, log[-600:]
    assert log.index("WRITE slot=") < log.index("PARENT_FAKEFW_REFRESH_START")
    r = d.record()[1]
    assert (r["request"], r["unmarked"], r["drew"]) == ("", "1", "1"), r
    log, boot, _, _ = parent_run("menu-hang-2", d)
    assert boot == "baseline" and "MENU_OPEN" not in log, log[-600:]
    assert "EPD_FW_SKIPPED the previous start drew and wasn't marked healthy: display_failed=1" in log, log[-600:]
    assert d.record()[1]["display_failed"] == "1"


@case("x86_64", "aarch64")
def handback_fat_form():
    """012 FR-008, boot-partition.md "Two forms": on a FAT32 boot partition the firmware
    offers a file system; the parent loads loader.efi, the loader keeps its record in the
    slot files and writes the probe as \\glassine\\probe.txt, saying boot_fs=1."""
    d = hb_disk("hb-fat", hb_record(request="recovery", probe="1"), fat=True)
    log, boot, floor, _ = parent_run("hb-fat", d)
    assert (boot, floor) == ("recovery", None), (boot, floor)
    assert "PARENT_FORM fat" in log and "HANDBACK form=fat" in log, "not the FAT form"
    seq, r, _ = d.record()
    assert (seq, r["request"]) == (2, ""), (seq, r)
    p = d.probe()
    assert p and dict(p)["boot_fs"] == "1", p and dict(p).get("boot_fs")
    assert "PROBE_WRITTEN ok=1" in log and "form=fat" in log



def main():
    global A
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="x86_64", choices=sorted(ARCHES))
    ap.add_argument("--skip", action="append", default=[], metavar="PREFIX",
                    help="leave out the cases whose names start with PREFIX")
    ap.add_argument("cases", nargs="*")
    a = ap.parse_args()
    A = ARCHES[a.arch]
    # another build of the firmware (QTEST_FW_CODE, QTEST_FW_VARS: paths in the container)
    A["code"] = os.environ.get("QTEST_FW_CODE") or A["code"]
    A["vars"] = os.environ.get("QTEST_FW_VARS") or A["vars"]
    pictures()
    os.makedirs(OUT, exist_ok=True)
    for c in CASES:
        if a.arch in c.arches and (not a.cases or c.__name__ in a.cases) and not any(c.__name__.startswith(x) for x in a.skip):
            c()
    fails = [r for r in RESULTS if r[1] != "PASS"]
    print(f"{len(RESULTS) - len(fails)}/{len(RESULTS)} passed ({a.arch})")
    with open(os.path.join(OUT, f"results-{a.arch}.json"), "w") as f:
        json.dump(RESULTS, f, indent=1)
    sys.exit(1 if fails or not RESULTS else 0)


if __name__ == "__main__":
    main()
