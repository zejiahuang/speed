#!/usr/bin/env python3
"""Generate the README's theme-aware SVG diagrams.

Every figure is emitted twice — once per palette — because GitHub does not
recolour an SVG loaded through `<img>`; it only darkens the page. One palette
therefore always fails on one theme, so the README wires the pair together with
`<picture>` + `prefers-color-scheme`.

The SVGs are committed; this script only has to be re-run when a figure
changes. Two things it refuses to leave to the eye:

* **Text-anchor is emitted on every `<text>`.** SVG's default is `start`, not
  `middle`, and it is not inherited from CSS — a centred string that omits it
  silently left-aligns at `x`, which looks like nothing at all in a screenshot.
* **Text wider than its box is reported.** Nothing clips in SVG: the string
  just runs past the border, and "slightly right" and "crossed the edge" are
  the same pixels. The estimate is deliberately generous (CJK advances 1 em,
  ASCII 0.55 em); the authoritative check is still a pixel measurement of the
  rendered PNG, see `--measure`.

Usage:
    python tools/readme-diagrams.py            # write every figure
    python tools/readme-diagrams.py flow       # write just one
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

FONT = ('-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", '
        '"Hiragino Sans GB", "Microsoft YaHei", "Helvetica Neue", Arial, sans-serif')

# --- palettes ---------------------------------------------------------------
# Keyed by semantic role, never by colour name: the figure code asks for
# "green_f" and each palette decides what that is. The values are the ones the
# figures have used since the first release (GitHub's own light/dark accents).
LIGHT = {
    "blue_f": "#ddf4ff", "blue_s": "#54aeff", "blue_t": "#0a3069",
    "purple_f": "#fbefff", "purple_s": "#bf8bff", "purple_t": "#3c1e70",
    "green_f": "#dafbe1", "green_s": "#4ac26b", "green_t": "#0a3622",
    "grey_f": "#f6f8fa", "grey_s": "#d1d9e0", "grey_t": "#1f2328",
    "orange_f": "#fff1e5", "orange_s": "#f0883e", "orange_t": "#7a2e00",
    "arrow": "#8c959f", "arrow_green": "#4ac26b", "muted": "#59636e",
}
DARK = {
    "blue_f": "#0d2b52", "blue_s": "#316dca", "blue_t": "#79c0ff",
    "purple_f": "#241b3d", "purple_s": "#8957e5", "purple_t": "#d2a8ff",
    "green_f": "#0f2a1b", "green_s": "#2ea043", "green_t": "#7ee787",
    "grey_f": "#21262d", "grey_s": "#3d444d", "grey_t": "#e6edf3",
    "orange_f": "#3a1f0d", "orange_s": "#bd561d", "orange_t": "#ffa657",
    "arrow": "#8b949e", "arrow_green": "#2ea043", "muted": "#9198a1",
}

# Characters that advance a full em even though they are not CJK ideographs.
WIDE = set("→←↑↓·—…“”‘’（）《》【】、。，：；！？　")


def advance(ch: str) -> float:
    """Advance width of one character, in em.

    Deliberately generous: an overestimate produces a warning the author can
    dismiss, an underestimate produces text that quietly crosses a border.
    """
    if ord(ch) > 0x2E7F or ch in WIDE:
        return 1.0
    return 0.55


def est_width(s: str, size: float) -> float:
    return sum(advance(c) for c in s) * size


class Sheet:
    """One SVG canvas, in one palette."""

    def __init__(self, width: int, height: int, palette: dict[str, str], name: str):
        self.w = width
        self.h = height
        self.p = palette
        self.name = name
        self.parts: list[str] = []
        self.warnings: list[str] = []
        self._markers: dict[str, str] = {}

    # -- primitives ---------------------------------------------------------

    def rect(self, x: float, y: float, w: float, h: float,
             fill: str, stroke: str, rx: int = 12) -> None:
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>')

    def text(self, x: float, y: float, s: str, size: float, fill: str,
             anchor: str = "middle", weight: str | None = None,
             opacity: float | None = None) -> None:
        # text-anchor on every call — see the module docstring.
        attrs = [f'x="{x}"', f'y="{y}"', f'font-family=\'{FONT}\'',
                 f'font-size="{size}"', f'fill="{fill}"', f'text-anchor="{anchor}"']
        if weight:
            attrs.append(f'font-weight="{weight}"')
        if opacity is not None:
            attrs.append(f'opacity="{opacity}"')
        self.parts.append('<text ' + ' '.join(attrs) + f' >{s}</text>')

    def marker(self, colour: str) -> str:
        """A per-colour arrowhead. One shared marker would give a grey line a
        red tip the moment a second colour is used."""
        key = colour.lstrip('#')
        if key not in self._markers:
            mid = f"a-{key}"
            self._markers[key] = mid
            self.parts.append(
                f'<marker id="{mid}" viewBox="0 0 10 10" refX="9" refY="5" '
                f'markerWidth="6" markerHeight="6" orient="auto-start-reverse">'
                f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{colour}"/></marker>')
        return self._markers[key]

    def line(self, x1: float, y1: float, x2: float, y2: float,
             colour: str, arrow: bool = True) -> None:
        tip = f' marker-end="url(#{self.marker(colour)})"' if arrow else ''
        self.parts.append(
            f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" '
            f'stroke="{colour}" stroke-width="1.6"{tip}/>')

    # -- composed -----------------------------------------------------------

    def check(self, s: str, size: float, avail: float, where: str) -> None:
        w = est_width(s, size)
        if w > avail:
            self.warnings.append(
                f'{self.name}: "{s}" est {w:.0f}px > {avail:.0f}px ({where})')

    def stacked(self, cx: float, x: float, y: float, w: float, h: float,
                lines: list[tuple], where: str) -> None:
        """Centre a stack of text lines vertically inside a box.

        `lines` is a list of `(text, size, colour)` or `(text, size, colour,
        weight)`. Baselines are computed rather than left to
        `dominant-baseline`, which some renderers ignore.
        """
        lead = 5.0
        heights = [size * 1.34 for _, size, *_ in lines]
        total = sum(heights) + lead * (len(lines) - 1)
        cursor = y + (h - total) / 2.0
        for entry in lines:
            s, size, colour = entry[0], entry[1], entry[2]
            weight = entry[3] if len(entry) > 3 else None
            cursor += size * 0.92
            self.text(cx, cursor, s, size, colour, weight=weight)
            self.check(s, size, w - 20, where)
            cursor += size * 0.42 + lead

    def box(self, x: float, y: float, w: float, h: float, role: str,
            lines: list[tuple], where: str) -> None:
        """A rounded box in `role`'s colours with a centred text stack."""
        self.rect(x, y, w, h, self.p[role + "_f"], self.p[role + "_s"])
        self.stacked(x + w / 2.0, x, y, w, h, lines, where)

    def render(self) -> str:
        head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" '
                f'height="{self.h}" viewBox="0 0 {self.w} {self.h}" role="img">')
        defs = ('<defs>' + ''.join(
            p for p in self.parts if p.startswith('<marker')) + '</defs>')
        body = ''.join(p for p in self.parts if not p.startswith('<marker'))
        return '\n'.join([head, defs, body, '</svg>', ''])


# --- figures ----------------------------------------------------------------

def figure_flow(p: dict[str, str], dark: bool) -> Sheet:
    """工作原理: the three ways to take over, one rule table, one destination.

    The old figure drew the VPN path alone. Three modes exist now, and they
    differ exactly at the takeover point — so the figure is three lanes that
    share a head (the app) and a tail (the upstream server).
    """
    s = Sheet(900, 660, p, 'flow-dark' if dark else 'flow-light')

    lane_w, gap = 250, 30
    x0 = (900 - (lane_w * 3 + gap * 2)) / 2.0        # 45
    lanes = [x0 + i * (lane_w + gap) for i in range(3)]
    cx = [x + lane_w / 2.0 for x in lanes]           # 170 / 450 / 730

    # -- head: the app ------------------------------------------------------
    s.box(250, 20, 380, 56, 'blue', [
        ('应用（浏览器 / 游戏 / 终端）', 14.0, p['blue_t'], '600'),
        ('照常发起连接，完全不知道下面发生了什么', 11.5, p['blue_t']),
    ], 'app')

    # fan out to the three lanes
    s.line(450, 76, 450, 96, p['arrow'], arrow=False)
    s.line(cx[0], 96, cx[2], 96, p['arrow'], arrow=False)
    for c in cx:
        s.line(c, 96, c, 124, p['arrow'])

    # -- the three takeover points -----------------------------------------
    takeover = [
        ('VPN 模式', 'VpnService · tun0', '整机流量（TCP + UDP）'),
        ('代理模式', '本地 HTTP 代理', '只接你指过来的软件'),
        ('Root 模式', 'hosts + 系统 CA + iptables', '只接规则表里的域名'),
    ]
    for x, (title, sub1, sub2) in zip(lanes, takeover):
        s.box(x, 124, lane_w, 90, 'purple', [
            (title, 14.0, p['purple_t'], '600'),
            (sub1, 11.5, p['purple_t']),
            (sub2, 11.5, p['purple_t']),
        ], 'takeover')

    for c in cx:
        s.line(c, 214, c, 238, p['arrow'])

    # -- what processes the flow -------------------------------------------
    # The second lane deliberately does not claim the same dialer as the
    # first: `watt-proxy` depends on `watt-rules` (same rule table, same
    # hit/miss strategy) but not on `watt-stack`, and it carries its own
    # race constants. "Same rule table" is the true half, so that is what the
    # title says.
    processing = [
        ('内核 · 用户态协议栈', '读出目标域名 → 查规则表', 'smoltcp + 多候选拨号器'),
        ('内核 · 同一份规则表', '读出主机名 → 查规则表', 'CONNECT 隧道 + HTTP 转发'),
        ('本地反代', '以自签证书终结 TLS', '再以客户端身份连真实服务器'),
    ]
    for x, (title, sub1, sub2) in zip(lanes, processing):
        s.box(x, 238, lane_w, 96, 'purple', [
            (title, 14.0, p['purple_t'], '600'),
            (sub1, 11.5, p['purple_t']),
            (sub2, 11.5, p['purple_t']),
        ], 'processing')

    for c in cx:
        s.line(c, 334, c, 358, p['arrow'])

    # -- the outcome --------------------------------------------------------
    # One box per lane, two colours: the hit line is green, the miss line is
    # muted. The third lane has no miss branch — root mode never sees a
    # domain the rule table does not name.
    outcome = [
        ('命中 → 规则地址拨号', '未命中 → 按 DNS 直连'),
        ('命中 → 规则地址拨号', '未命中 → 直连'),
        ('只从规则地址拨号', '表外域名不经过它'),
    ]
    for x, (hit, miss) in zip(lanes, outcome):
        s.rect(x, 358, lane_w, 72, p['green_f'], p['green_s'])
        s.text(x + lane_w / 2.0, 386.0, hit, 12.5, p['green_t'], weight='600')
        s.check(hit, 12.5, lane_w - 20, 'outcome-hit')
        s.text(x + lane_w / 2.0, 410.0, miss, 11.5, p['muted'])
        s.check(miss, 11.5, lane_w - 20, 'outcome-miss')

    for c in cx:
        s.line(c, 430, c, 462, p['arrow'], arrow=False)
    s.line(cx[0], 462, cx[2], 462, p['arrow'], arrow=False)
    s.line(450, 462, 450, 496, p['arrow'])

    # -- tail: the upstream server -----------------------------------------
    s.box(250, 496, 380, 56, 'orange', [
        ('上游服务器', 14.0, p['orange_t'], '600'),
        ('三条路都到这里，区别只在于「谁在接管、连的是哪个地址」', 11.5, p['orange_t']),
    ], 'upstream')

    # -- the boundary worth repeating --------------------------------------
    s.text(450, 596, '前两种模式不解密 TLS：规则地址的证书必须覆盖域名。Root 模式是本机终结 TLS 的例外。',
           11.5, p['muted'])
    s.check('前两种模式不解密 TLS：规则地址的证书必须覆盖域名。Root 模式是本机终结 TLS 的例外。',
            11.5, 860, 'caption')
    s.text(450, 616, '三种模式共用同一套规则解析与「命中改道 / 未命中直连」策略；Root 模式固定使用它自己的那份规则文档。',
           11.5, p['muted'])
    s.check('三种模式共用同一套规则解析与「命中改道 / 未命中直连」策略；Root 模式固定使用它自己的那份规则文档。',
            11.5, 860, 'caption')

    return s


FIGURES = {
    'flow': figure_flow,
}


def main(argv: list[str]) -> int:
    want = argv[1:] or sorted(FIGURES)
    warnings: list[str] = []
    for name in want:
        if name not in FIGURES:
            print(f'unknown figure: {name}', file=sys.stderr)
            return 2
        for dark, palette in ((False, LIGHT), (True, DARK)):
            sheet = FIGURES[name](palette, dark)
            out = ASSETS / f'{name}-{"dark" if dark else "light"}.svg'
            # newline="\n": Path.write_text on Windows would turn every \n into
            # \r\n and the working tree would disagree with the index.
            out.write_text(sheet.render(), encoding='utf-8', newline='\n')
            print(f'wrote {out.relative_to(ROOT)}  ({out.stat().st_size} B)')
            warnings += sheet.warnings

    if warnings:
        print('\nWARNINGS (text wider than its box):', file=sys.stderr)
        for w in warnings:
            print('  ' + w, file=sys.stderr)
        return 1
    print('no overflow warnings')
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv))
