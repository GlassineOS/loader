#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""The start menu's icons and the GlassineOS mark as LVGL A4 masks (loader spec 004).

    tools/menu-art.py loader/c/menu_art.c

The shapes are the boards' (GlassineOS/ui designs/startmenu: data/menu.js for the icons,
the mark as Menu.dc.html and the splash draw it). Icons: strokes on a 24-unit grid,
1.5 units wide, round ends and joins. The mark: a sheet's outline (2.5 wide), a second
sheet turned 6 degrees over it, a pen stroke on that sheet, on a 48-unit grid, as three
masks (sheet, outline, pen) so the menu can colour each. Coverage is counted on a 4 x 4
grid per pixel with +, -, *, / and sqrt only, so the bytes are the same on every machine.
"""
import math
import sys

ICONS = {
    "previous": "M5.2 9.2A7.5 7.5 0 1 1 4.5 12M4.6 4.8v4.6h4.6M12 8v4l2.8 1.8",
    "more": "M8 4.5h9.5a2 2 0 0 1 2 2V17M6.5 7.5h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-8a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2z",
    "kernel": "M8.5 7h7a1.5 1.5 0 0 1 1.5 1.5v7a1.5 1.5 0 0 1-1.5 1.5h-7A1.5 1.5 0 0 1 7 15.5v-7A1.5 1.5 0 0 1 8.5 7zM10 4v3M14 4v3M10 17v3M14 17v3M4 10h3M4 14h3M17 10h3M17 14h3",
    "other": "M8.5 3h7a2.5 2.5 0 0 1 2.5 2.5v13a2.5 2.5 0 0 1-2.5 2.5h-7A2.5 2.5 0 0 1 6 18.5v-13A2.5 2.5 0 0 1 8.5 3zM11 17.5h2",
    "recovery": "M12 3.5a8.5 8.5 0 1 1 0 17a8.5 8.5 0 1 1 0-17zM12 8.5a3.5 3.5 0 1 1 0 7a3.5 3.5 0 1 1 0-7zM6 6l3.5 3.5M18 6l-3.5 3.5M6 18l3.5-3.5M18 18l-3.5-3.5",
    "deployment": "M7 3h10a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2zM8.5 8h7M8.5 12h7M8.5 16h4",
    "back": "M19 12H5M11 6l-6 6l6 6",
    "clock": "M12 3.5a8.5 8.5 0 1 1 0 17a8.5 8.5 0 1 1 0-17zM12 7.5V12l3 2",
}
# (name, icon, pixels)
MASKS = [(n, n, 96) for n in ICONS if n != "clock"] + [("clock", "clock", 56)]
MARKS = [("mark_card", 96), ("mark_head", 112)]
SS = 4  # samples per pixel per axis
STROKE = 1.5
C6, S6 = 0.9945218953682733, 0.10452846326765347  # cos, sin of 6 degrees


def tokens(d):
    out, num = [], ""
    for ch in d:
        if ch.isalpha():
            if num:
                out.append(float(num)); num = ""
            out.append(ch)
        elif ch in " ,":
            if num:
                out.append(float(num)); num = ""
        elif ch == "-" and num and num[-1] != "e":
            out.append(float(num)); num = "-"
        elif ch == "." and "." in num:
            out.append(float(num)); num = "."
        else:
            num += ch
    if num:
        out.append(float(num))
    return out


def arc(p0, rx, ry, phi, large, sweep, p1, n=48):
    """SVG endpoint arc to points (phi in degrees; the icons use 0)."""
    (x1, y1), (x2, y2) = p0, p1
    cp, sp = math.cos(math.radians(phi)), math.sin(math.radians(phi))
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    lam = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry)
    if lam > 1:
        s = math.sqrt(lam); rx *= s; ry *= s
    num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    co = math.sqrt(max(num / den, 0.0)) * (-1 if large == sweep else 1)
    cxp, cyp = co * rx * y1p / ry, -co * ry * x1p / rx
    cx, cy = cp * cxp - sp * cyp + (x1 + x2) / 2, sp * cxp + cp * cyp + (y1 + y2) / 2

    def ang(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a
    t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not sweep and dt > 0:
        dt -= 2 * math.pi
    elif sweep and dt < 0:
        dt += 2 * math.pi
    pts = []
    for i in range(1, n + 1):
        t = t1 + dt * i / n
        x, y = rx * math.cos(t), ry * math.sin(t)
        pts.append((cp * x - sp * y + cx, sp * x + cp * y + cy))
    return pts


def polylines(d):
    t, i, cur, start, lines, cmd = tokens(d), 0, (0.0, 0.0), (0.0, 0.0), [], None
    line = []

    def take(k):
        nonlocal i
        v = t[i:i + k]; i += k
        return v
    while i < len(t):
        if isinstance(t[i], str):
            cmd = t[i]; i += 1
        rel = cmd.islower()
        c = cmd.upper()
        ox, oy = cur if rel else (0.0, 0.0)
        if c == "M":
            x, y = take(2); cur = (ox + x, oy + y); start = cur
            if len(line) > 1:
                lines.append(line)
            line = [cur]
            cmd = "l" if rel else "L"
        elif c == "L":
            x, y = take(2); cur = (ox + x, oy + y); line.append(cur)
        elif c == "H":
            (x,) = take(1); cur = ((ox if rel else 0.0) + x, cur[1]); line.append(cur)
        elif c == "V":
            (y,) = take(1); cur = (cur[0], (oy if rel else 0.0) + y); line.append(cur)
        elif c == "A":
            rx, ry, phi, la, sw, x, y = take(7)
            end = (ox + x, oy + y)
            line += arc(cur, rx, ry, phi, int(la), int(sw), end); cur = end
        elif c == "C":
            x1, y1, x2, y2, x, y = take(6)
            p0, p1, p2, p3 = cur, (ox + x1, oy + y1), (ox + x2, oy + y2), (ox + x, oy + y)
            for k in range(1, 33):
                s = k / 32; u = 1 - s
                a, b, cc, dd = u * u * u, 3 * u * u * s, 3 * u * s * s, s * s * s
                line.append((a * p0[0] + b * p1[0] + cc * p2[0] + dd * p3[0], a * p0[1] + b * p1[1] + cc * p2[1] + dd * p3[1]))
            cur = p3
        elif c == "Z":
            line.append(start); cur = start
        else:
            raise SystemExit(f"path command {cmd}")
    if len(line) > 1:
        lines.append(line)
    return lines


def stroke_cov(lines, size, units, width):
    """Coverage 0..SS*SS per pixel of strokes `width` units wide on a `units` grid."""
    n = size * SS
    sc = n / units
    hit = bytearray(n * n)
    r = width / 2 * sc
    r2 = r * r
    for line in lines:
        for (ax, ay), (bx, by) in zip(line, line[1:]):
            ax, ay, bx, by = ax * sc, ay * sc, bx * sc, by * sc
            dx, dy = bx - ax, by - ay
            ll = dx * dx + dy * dy
            x0, x1 = max(int(min(ax, bx) - r) - 1, 0), min(int(max(ax, bx) + r) + 2, n)
            y0, y1 = max(int(min(ay, by) - r) - 1, 0), min(int(max(ay, by) + r) + 2, n)
            for y in range(y0, y1):
                py = y + 0.5
                row = y * n
                for x in range(x0, x1):
                    if hit[row + x]:
                        continue
                    px = x + 0.5
                    t = ((px - ax) * dx + (py - ay) * dy) / ll if ll else 0.0
                    t = 0.0 if t < 0 else 1.0 if t > 1 else t
                    ex, ey = px - ax - t * dx, py - ay - t * dy
                    if ex * ex + ey * ey <= r2:
                        hit[row + x] = 1
    return hit


def box(px, py, cx, cy, hw, hh, r):
    qx, qy = abs(px - cx) - hw + r, abs(py - cy) - hh + r
    ox, oy = max(qx, 0.0), max(qy, 0.0)
    return math.sqrt(ox * ox + oy * oy) + min(max(qx, qy), 0.0) - r


def mark_cov(size):
    n = size * SS
    sc = n / 48.0
    leaf, outline = bytearray(n * n), bytearray(n * n)
    for y in range(n):
        vy = (y + 0.5) / sc
        for x in range(n):
            vx = (x + 0.5) / sc
            rx, ry = vx - 29.0, vy - 27.0
            lx, ly = C6 * rx + S6 * ry + 29.0, -S6 * rx + C6 * ry + 27.0
            if box(lx, ly, 29.0, 27.0, 15.0, 17.0, 5.0) <= 0:
                leaf[y * n + x] = 1
            elif abs(box(vx, vy, 17.0, 21.0, 15.0, 17.0, 5.0)) <= 1.25:
                outline[y * n + x] = 1
    # the pen stroke, drawn in the leaf's frame then turned with it
    pen_local = polylines("M20 30C26 24 30 34 38 26")
    turned = [[(C6 * (x - 29) - S6 * (y - 27) + 29, S6 * (x - 29) + C6 * (y - 27) + 27) for x, y in l] for l in pen_local]
    pen = stroke_cov(turned, size, 48, 3.0)
    return leaf, outline, pen


def a4(hit, size):
    n = size * SS
    out = bytearray()
    full = SS * SS
    for y in range(size):
        vals = []
        for x in range(size):
            c = 0
            for sy in range(SS):
                row = (y * SS + sy) * n + x * SS
                c += sum(hit[row:row + SS])
            vals.append((c * 15 + full // 2) // full)
        for k in range(0, size, 2):
            out.append((vals[k] << 4) | (vals[k + 1] if k + 1 < size else 0))
    return out


def emit(name, data, size):
    lines = [f"static const uint8_t {name}_px[{len(data)}] = {{"]
    for k in range(0, len(data), 24):
        lines.append("    " + ", ".join(f"0x{b:02x}" for b in data[k:k + 24]) + ",")
    lines.append("};")
    lines.append(f"const lv_image_dsc_t menu_art_{name} = {{")
    lines.append(f"    .header = {{ .magic = LV_IMAGE_HEADER_MAGIC, .cf = LV_COLOR_FORMAT_A4, .w = {size}, .h = {size}, .stride = {(size + 1) // 2} }},")
    lines.append(f"    .data_size = {len(data)},")
    lines.append(f"    .data = {name}_px,")
    lines.append("};")
    return "\n".join(lines)


def main():
    out = ["/* SPDX-License-Identifier: GPL-2.0-or-later */",
           "/* Generated by tools/menu-art.py: the start menu's icons and the GlassineOS mark",
           " * as A4 masks (loader spec 004). Do not edit; run the tool. */",
           '#include "menu_art.h"', ""]
    for name, icon, size in MASKS:
        out.append(emit(name, a4(stroke_cov(polylines(ICONS[icon]), size, 24, STROKE), size), size))
    for name, size in MARKS:
        leaf, outline, pen = mark_cov(size)
        out.append(emit(name + "_leaf", a4(leaf, size), size))
        out.append(emit(name + "_outline", a4(outline, size), size))
        out.append(emit(name + "_pen", a4(pen, size), size))
    open(sys.argv[1], "w").write("\n".join(out) + "\n")
    names = [m[0] for m in MASKS] + [f"{m}_{p}" for m, _ in MARKS for p in ("leaf", "outline", "pen")]
    hdr = ["/* SPDX-License-Identifier: GPL-2.0-or-later */", "/* Generated by tools/menu-art.py. */",
           "#ifndef GLASSINE_MENU_ART_H", "#define GLASSINE_MENU_ART_H", '#include "lvgl.h"']
    hdr += [f"extern const lv_image_dsc_t menu_art_{n};" for n in names]
    hdr.append("#endif")
    open(sys.argv[1][:-2] + ".h", "w").write("\n".join(hdr) + "\n")


if __name__ == "__main__":
    main()
