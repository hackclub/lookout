#!/usr/bin/env python3
"""Convert Lookout's single-colour SVG icons (assets/icons) to vector PDFs
that AppKit draws as template images, for the System Settings pane.

    svg2pdf.py <out-dir> <icon.svg>...

Handles what those icons use: a viewBox, root-level fill or stroke
(stroke-width, stroke-linecap/linejoin) and <path d="..."> with the full
path grammar (M L H V C S Q T A Z, absolute and relative). Arcs become
cubics. Standard library only, so it runs on any build machine.
"""

import math
import os
import re
import sys
import xml.etree.ElementTree as ET

NUM = re.compile(r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?")
TOKEN = re.compile(r"[MmLlHhVvCcSsQqTtAaZz]|[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?")


def tokens(d):
    return TOKEN.findall(d)


def arc_to_cubics(x1, y1, rx, ry, phi, large, sweep, x2, y2):
    """SVG elliptical arc (endpoint form) → list of cubic control triples."""
    if (x1, y1) == (x2, y2):
        return []
    if rx == 0 or ry == 0:
        return [((x1, y1), (x2, y2), (x2, y2))]
    rx, ry = abs(rx), abs(ry)
    cp, sp = math.cos(math.radians(phi)), math.sin(math.radians(phi))
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p = cp * dx + sp * dy
    y1p = -sp * dx + cp * dy
    lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
    if lam > 1:
        s = math.sqrt(lam)
        rx, ry = rx * s, ry * s
    num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    co = math.sqrt(max(0.0, num / den)) if den else 0.0
    if large == sweep:
        co = -co
    cxp = co * rx * y1p / ry
    cyp = -co * ry * x1p / rx
    cx = cp * cxp - sp * cyp + (x1 + x2) / 2
    cy = sp * cxp + cp * cyp + (y1 + y2) / 2

    def ang(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a

    t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not sweep and dt > 0:
        dt -= 2 * math.pi
    elif sweep and dt < 0:
        dt += 2 * math.pi
    n = max(1, math.ceil(abs(dt) / (math.pi / 2) - 1e-9))
    step = dt / n
    k = 4 / 3 * math.tan(step / 4)
    out = []

    def pt(t):
        x, y = rx * math.cos(t), ry * math.sin(t)
        return (cp * x - sp * y + cx, sp * x + cp * y + cy)

    def deriv(t):
        x, y = -rx * math.sin(t), ry * math.cos(t)
        return (cp * x - sp * y, sp * x + cp * y)

    t = t1
    for _ in range(n):
        p0, p3 = pt(t), pt(t + step)
        d0, d3 = deriv(t), deriv(t + step)
        c1 = (p0[0] + k * d0[0], p0[1] + k * d0[1])
        c2 = (p3[0] - k * d3[0], p3[1] - k * d3[1])
        out.append((c1, c2, p3))
        t += step
    out[-1] = (out[-1][0], out[-1][1], (x2, y2))
    return out


def path_ops(d):
    """Parse a path into ('m', p) / ('l', p) / ('c', c1, c2, p) / ('z',)."""
    toks = tokens(d)
    i, cmd = 0, None
    cur = start = (0.0, 0.0)
    last_c2 = last_q = None
    ops = []

    def num():
        nonlocal i
        v = float(toks[i])
        i += 1
        return v

    def flag():
        # Arc flags are one character and may run into what follows
        # ("a8 8 0 0110 10" is large=0, sweep=1, x=10).
        nonlocal i
        t = toks[i]
        if t[0] not in "01":
            raise ValueError(f"bad arc flag {t!r}")
        if len(t) > 1:
            toks[i] = t[1:]
        else:
            i += 1
        return t[0] == "1"

    while i < len(toks):
        if re.fullmatch(r"[A-Za-z]", toks[i]):
            cmd = toks[i]
            i += 1
        elif cmd is None:
            raise ValueError("path data must start with a command")
        rel = cmd.islower()
        c = cmd.upper()
        ox, oy = cur if rel else (0.0, 0.0)
        if c == "Z":
            ops.append(("z",))
            cur = start
            last_c2 = last_q = None
            continue
        if c == "M":
            cur = (ox + num(), oy + num())
            start = cur
            ops.append(("m", cur))
            cmd = "l" if rel else "L"  # further pairs are line-tos
            last_c2 = last_q = None
        elif c == "L":
            cur = (ox + num(), oy + num())
            ops.append(("l", cur))
            last_c2 = last_q = None
        elif c == "H":
            cur = ((cur[0] if rel else 0.0) + num(), cur[1])
            ops.append(("l", cur))
            last_c2 = last_q = None
        elif c == "V":
            cur = (cur[0], (cur[1] if rel else 0.0) + num())
            ops.append(("l", cur))
            last_c2 = last_q = None
        elif c == "C":
            c1 = (ox + num(), oy + num())
            c2 = (ox + num(), oy + num())
            p = (ox + num(), oy + num())
            ops.append(("c", c1, c2, p))
            cur, last_c2, last_q = p, c2, None
        elif c == "S":
            c1 = (2 * cur[0] - last_c2[0], 2 * cur[1] - last_c2[1]) if last_c2 else cur
            c2 = (ox + num(), oy + num())
            p = (ox + num(), oy + num())
            ops.append(("c", c1, c2, p))
            cur, last_c2, last_q = p, c2, None
        elif c in "QT":
            if c == "Q":
                q = (ox + num(), oy + num())
            else:
                q = (2 * cur[0] - last_q[0], 2 * cur[1] - last_q[1]) if last_q else cur
            p = (ox + num(), oy + num())
            c1 = (cur[0] + 2 / 3 * (q[0] - cur[0]), cur[1] + 2 / 3 * (q[1] - cur[1]))
            c2 = (p[0] + 2 / 3 * (q[0] - p[0]), p[1] + 2 / 3 * (q[1] - p[1]))
            ops.append(("c", c1, c2, p))
            cur, last_q, last_c2 = p, q, None
        elif c == "A":
            rx, ry, phi = num(), num(), num()
            large = flag()
            sweep = flag()
            p = (ox + num(), oy + num())
            for c1, c2, end in arc_to_cubics(cur[0], cur[1], rx, ry, phi, large, sweep, p[0], p[1]):
                ops.append(("c", c1, c2, end))
            cur, last_c2, last_q = p, None, None
        else:
            raise ValueError(f"unsupported path command {cmd}")
    return ops


def convert(svg_path):
    root = ET.parse(svg_path).getroot()
    vb = [float(v) for v in NUM.findall(root.get("viewBox", "0 0 24 24"))]
    x0, y0, w, h = vb
    stroke = root.get("stroke")
    fill = root.get("fill", "#000")
    stroked = stroke not in (None, "none")
    filled = fill != "none"
    lw = float(root.get("stroke-width", "1"))
    cap = {"butt": 0, "round": 1, "square": 2}[root.get("stroke-linecap", "butt")]
    join = {"miter": 0, "round": 1, "bevel": 2}[root.get("stroke-linejoin", "miter")]

    f = lambda v: f"{v:.4f}".rstrip("0").rstrip(".")  # noqa: E731
    # Flip to PDF's y-up space with the viewBox at the origin.
    out = [f"1 0 0 -1 {f(-x0)} {f(h + y0)} cm"]
    if stroked:
        out.append(f"{f(lw)} w {cap} J {join} j")
    out.append("0 g 0 G")
    for el in root.iter():
        tag = el.tag.rsplit("}", 1)[-1]
        if tag in ("rect", "circle", "ellipse", "line", "polyline", "polygon", "use", "text", "image"):
            raise ValueError(f"{svg_path}: <{tag}> isn't supported, only <path>")
        if tag != "path":
            continue
        for op in path_ops(el.get("d", "")):
            if op[0] == "m":
                out.append(f"{f(op[1][0])} {f(op[1][1])} m")
            elif op[0] == "l":
                out.append(f"{f(op[1][0])} {f(op[1][1])} l")
            elif op[0] == "c":
                (a, b), (c, d), (e, g) = op[1], op[2], op[3]
                out.append(f"{f(a)} {f(b)} {f(c)} {f(d)} {f(e)} {f(g)} c")
            else:
                out.append("h")
        # Phosphor's fills rely on nonzero winding, like SVG's default.
        out.append("B" if filled and stroked else ("S" if stroked else "f"))
    return w, h, "\n".join(out).encode()


def write_pdf(path, w, h, content):
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:g} {h:g}] /Contents 4 0 R /Resources << >> >>".encode(),
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream",
    ]
    data = bytearray(b"%PDF-1.4\n")
    offsets = []
    for n, body in enumerate(objs, 1):
        offsets.append(len(data))
        data += f"{n} 0 obj\n".encode() + body + b"\nendobj\n"
    xref = len(data)
    data += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        data += f"{off:010d} 00000 n \n".encode()
    data += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    with open(path, "wb") as fh:
        fh.write(data)


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for svg in sys.argv[2:]:
        name = os.path.splitext(os.path.basename(svg))[0]
        w, h, content = convert(svg)
        write_pdf(os.path.join(out, name + ".pdf"), w, h, content)


if __name__ == "__main__":
    main()
