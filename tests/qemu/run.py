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


def compose(px, w, h, mode, bg):
    """The Python twin of gspl::compose."""
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
