#!/usr/bin/env python3
"""Write an image in the splash store's format (spike draft of 011 FR-004; layout in
src/splash.rs). Library use: encode(); command line: mksplash.py IN.png OUT.gspl
--mode WxH [--bg RRGGBB] | mksplash.py --leave-screen OUT.gspl"""
import argparse
import struct
import zlib

MAGIC = b"GLSPLASH"


def rle(px):
    """px: list of 0xRRGGBB ints -> run-length payload."""
    out = bytearray()
    i, n = 0, len(px)
    lit = []

    def flush_lit():
        while lit:
            chunk = lit[:128]
            del lit[:128]
            out.append(len(chunk) - 1)
            for p in chunk:
                out += struct.pack("<I", p)

    while i < n:
        j = i
        while j < n and px[j] == px[i] and j - i < 129:
            j += 1
        if j - i >= 2:
            flush_lit()
            out.append(j - i + 126)
            out += struct.pack("<I", px[i])
            i = j
        else:
            lit.append(px[i])
            i += 1
    flush_lit()
    return bytes(out)


def header(kind, w, h, mode, bg, enc, payload):
    head = MAGIC + struct.pack("<HHIIIIIIIQI", 1, kind, w, h, mode[0], mode[1], 1, bg, enc,
                               len(payload), zlib.crc32(payload))
    head += struct.pack("<I", zlib.crc32(head)) + bytes(8)
    assert len(head) == 64
    return head


def encode(px, w, h, mode, bg):
    payload = rle(px)
    return header(0, w, h, mode, bg, 1, payload) + payload


def leave_screen():
    return header(1, 0, 0, (0, 0), 0, 0, b"")


def from_png(path):
    from PIL import Image
    im = Image.open(path).convert("RGB")
    w, h = im.size
    return [(r << 16) | (g << 8) | b for r, g, b in im.getdata()], w, h


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("inp", nargs="?")
    ap.add_argument("out")
    ap.add_argument("--mode")
    ap.add_argument("--bg", default="000000")
    ap.add_argument("--leave-screen", action="store_true")
    a = ap.parse_args()
    if a.leave_screen:
        data = leave_screen()
    else:
        px, w, h = from_png(a.inp)
        mw, mh = (int(v) for v in a.mode.split("x"))
        data = encode(px, w, h, (mw, mh), int(a.bg, 16))
    with open(a.out + ".tmp", "wb") as f:
        f.write(data)
    import os
    os.replace(a.out + ".tmp", a.out)


if __name__ == "__main__":
    main()
