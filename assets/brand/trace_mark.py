"""Vectorise the EA mark from the flattened XCF render into SVG paths.

The art is flat-colour with straight edges, so: classify pixels into the two
letters, walk the inside/outside boundary as unit edges, stitch them into
loops (outer loops and the A's counter alike), then Douglas-Peucker the
staircases back into the straight lines they came from.

Three outputs, chosen by the extension of the third argument: a `.txt` of `d`
attributes cropped to the ink, for the header glyph to inline; a `.svg` of the
whole mark in the master's own box, outline included -- the brand asset; and a
`.png` rasterised from that same vector, so no bitmap is a second tracing of
the master that could drift from it. What each shipped file is made with, run
from the repo root (the master is found beside this script either way):

    B=assets/brand/trace_mark.py
    python3 $B 1.6 union  /tmp/mark-paths.txt                # -> index.html
    python3 $B 1.6 layers assets/brand/embarch-mark.svg
    python3 $B 1.6 layers assets/brand/favicon-64.png    64
    python3 $B 1.6 layers vscode-extension/icon.png     128

Tracing a 256px raster costs half a pixel of edge placement and there is no
getting it back: measured across four scanlines of the A's left edge, the cut
lands -0.24, +0.06, -0.43 and -0.11 px off the master's true 50% point. The
sign varies because that internal edge is one pixel wide in the master, so
this is grid quantisation rather than a bias worth correcting.
"""
import os, zlib, struct, sys
from collections import deque

def readpng(p):
    d = open(p, 'rb').read(); i = 8; idat = b''; w = h = 0; ct = 6
    while i < len(d):
        n = struct.unpack('>I', d[i:i+4])[0]; t = d[i+4:i+8]; pay = d[i+8:i+8+n]
        if t == b'IHDR': w, h, _, ct = struct.unpack('>IIBB', pay[:10])
        if t == b'IDAT': idat += pay
        i += 12 + n
    nch = {0:1, 2:3, 3:1, 4:2, 6:4}[ct]
    raw = zlib.decompress(idat); rows = []; prev = bytearray(w*nch); pos = 0
    for y in range(h):
        f = raw[pos]; pos += 1; line = bytearray(raw[pos:pos+w*nch]); pos += w*nch
        for x in range(w*nch):
            a = line[x-nch] if x >= nch else 0; b = prev[x]; c = prev[x-nch] if x >= nch else 0
            if f == 1: line[x] = (line[x]+a) & 255
            elif f == 2: line[x] = (line[x]+b) & 255
            elif f == 3: line[x] = (line[x]+((a+b) >> 1)) & 255
            elif f == 4:
                pp = a+b-c; pa, pb, pc = abs(pp-a), abs(pp-b), abs(pp-c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x]+pr) & 255
        rows.append(bytes(line)); prev = line
    return w, h, nch, rows

# The master render that sits next to this script -- byte-identical to the
# 'from-xcf-256.png' this was first written against, renamed on the way in.
SRC = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'icon-256.png')
W, H, NCH, PX = readpng(SRC)

def px(x, y):
    i = x*NCH; r = PX[y][i]; g = PX[y][i+1]; b = PX[y][i+2]
    a = PX[y][i+3] if NCH == 4 else 255
    return r, g, b, a

# --- classify: 'r' the red A, 'g' the grey E, 'o' the dark outline ----------
cls = [[None]*W for _ in range(H)]
for y in range(H):
    for x in range(W):
        r, g, b, a = px(x, y)
        if a < 128: continue
        lum = 0.299*r + 0.587*g + 0.114*b
        if r > g + 40 and r > b + 40: cls[y][x] = 'r'
        elif lum < 80: cls[y][x] = 'o'
        else: cls[y][x] = 'g'
counts = {}
for y in range(H):
    for x in range(W):
        counts[cls[y][x]] = counts.get(cls[y][x], 0) + 1
print('classified', {k: v for k, v in counts.items()})

# --- the outline belongs to whichever letter it bounds: multi-source BFS ----
own = [[cls[y][x] if cls[y][x] in ('r', 'g') else None for x in range(W)] for y in range(H)]
q = deque((x, y) for y in range(H) for x in range(W) if own[y][x])
while q:
    x, y = q.popleft()
    for dx, dy in ((1,0), (-1,0), (0,1), (0,-1)):
        nx, ny = x+dx, y+dy
        if 0 <= nx < W and 0 <= ny < H and cls[ny][nx] == 'o' and own[ny][nx] is None:
            own[ny][nx] = own[y][x]; q.append((nx, ny))

# --- boundary as unit edges, then stitched into closed loops ---------------
def loops(mask):
    inside = lambda x, y: 0 <= x < W and 0 <= y < H and mask[y][x]
    edges = {}
    for y in range(H):
        for x in range(W):
            if not inside(x, y): continue
            if not inside(x, y-1): edges.setdefault((x, y), []).append((x+1, y))
            if not inside(x+1, y): edges.setdefault((x+1, y), []).append((x+1, y+1))
            if not inside(x, y+1): edges.setdefault((x+1, y+1), []).append((x, y+1))
            if not inside(x-1, y): edges.setdefault((x, y+1), []).append((x, y))
    out = []
    while edges:
        start = next(iter(edges)); loop = [start]; cur = start
        while True:
            nxts = edges.get(cur)
            if not nxts: break
            nxt = nxts.pop()
            if not nxts: del edges[cur]
            cur = nxt
            if cur == start: break
            loop.append(cur)
        if len(loop) > 3: out.append(loop)
    return out

def dp(pts, tol):
    """Douglas-Peucker on an open chain."""
    if len(pts) < 3: return pts
    keep = [False]*len(pts); keep[0] = keep[-1] = True
    stack = [(0, len(pts)-1)]
    while stack:
        i, j = stack.pop()
        if j <= i+1: continue
        x1, y1 = pts[i]; x2, y2 = pts[j]
        dx, dy = x2-x1, y2-y1
        n = (dx*dx+dy*dy) ** 0.5
        worst, wi = -1.0, -1
        for k in range(i+1, j):
            x0, y0 = pts[k]
            d = abs(dy*x0 - dx*y0 + x2*y1 - y2*x1)/n if n else ((x0-x1)**2+(y0-y1)**2) ** 0.5
            if d > worst: worst, wi = d, k
        if worst > tol:
            keep[wi] = True; stack.append((i, wi)); stack.append((wi, j))
    return [p for p, k in zip(pts, keep) if k]

def simplify(loop, tol):
    # start the chain at a lexicographic extreme so a real corner anchors it
    i = min(range(len(loop)), key=lambda k: loop[k])
    ch = loop[i:] + loop[:i]
    s = dp(ch + [ch[0]], tol)
    return s[:-1]

TOL = float(sys.argv[1]) if len(sys.argv) > 1 else 0.9
MODE = sys.argv[2] if len(sys.argv) > 2 else 'bfs'
OUT = sys.argv[3] if len(sys.argv) > 3 else 'mark-paths.txt'
SVG = OUT.endswith('.svg')
PNG = OUT.endswith('.png')
SIZE = int(sys.argv[4]) if len(sys.argv) > 4 else 64

def despeck(mask, floor=64):
    """Drop connected components below `floor` pixels.

    The classifier reads the bottom antialias row of the A -- one pixel deep,
    blended to (100,53,48) over the dark stroke -- as red, which the `own` BFS
    used to swallow but a per-class mask does not: 125 islands, 272 px, all of
    them a rounding artefact of the render rather than artwork. The gap either
    side of the cut is wide: the largest speck is 39 px and the smallest real
    component 5922, so any floor in between picks out the same set. Culled
    pixels are not lost, they fall through to the dark stroke underneath, which
    is roughly what that blend looked like anyway. Holes are left alone -- the
    only two are the A's counter and its ring, both real.
    """
    seen = [[False]*W for _ in range(H)]
    out = [[False]*W for _ in range(H)]
    for y in range(H):
        for x in range(W):
            if not mask[y][x] or seen[y][x]: continue
            q = deque([(x, y)]); seen[y][x] = True; cells = []
            while q:
                cx, cy = q.popleft(); cells.append((cx, cy))
                for dx, dy in ((1,0), (-1,0), (0,1), (0,-1)):
                    nx, ny = cx+dx, cy+dy
                    if 0 <= nx < W and 0 <= ny < H and mask[ny][nx] and not seen[ny][nx]:
                        seen[ny][nx] = True; q.append((nx, ny))
            if len(cells) >= floor:
                for cx, cy in cells: out[cy][cx] = True
    return out

# How the dark outline is dealt with. The first three exist because a 22px
# inline glyph cannot carry a 2px stroke; the fourth exists because a scalable
# file can, and should.
#   bfs    -- each outline pixel joins the letter it bounds (letters meet on a
#             jagged interface, invisible small but present at 128px)
#   union  -- the E is the whole silhouette and the A is painted over it, so the
#             interface is hidden under the A entirely
#   gap    -- outline dropped to transparent, leaving a hairline of background
#             between the letters, like the original's dark stroke
#   layers -- three paths: the dark stroke as the whole silhouette underneath,
#             then each letter's own pixels on top of it. That is how the master
#             is built, so it is the mode that reproduces it rather than
#             approximating it -- and the only one worth writing to an .svg.
def mask_for(letter):
    if letter == 'o':
        return [[cls[y][x] is not None for x in range(W)] for y in range(H)]
    if MODE == 'union' and letter == 'g':
        return [[cls[y][x] is not None for x in range(W)] for y in range(H)]
    if MODE in ('gap', 'layers'):
        return [[cls[y][x] == letter for x in range(W)] for y in range(H)]
    return [[own[y][x] == letter for x in range(W)] for y in range(H)]

INK = [(x, y) for y in range(H) for x in range(W) if own[y][x]]
X0 = min(x for x, y in INK); X1 = max(x for x, y in INK) + 1
Y0 = min(y for x, y in INK); Y1 = max(y for x, y in INK) + 1
print('ink bbox %d..%d x %d..%d' % (X0, X1, Y0, Y1))
if SVG or PNG:
    # The standalone mark keeps the master's box, padding and all: it is a
    # drop-in for icon-256.png, and an app icon's breathing room is part of the
    # artwork. viewBox units are the master's own pixels, so a coordinate here
    # can be read straight off the XCF.
    SCALE = 1.0; OX = OY = 0.0; ND = 1
else:
    # Crop to the ink and scale that to fill the 24 box. The logo's 25px padding
    # is right for an app icon sitting in its own container and wrong for an
    # inline glyph, which is already inside the header's own spacing -- keeping
    # it would throw away 20% of the linear size at the 22px this renders at.
    SPAN = max(X1-X0, Y1-Y0)
    SCALE = 24.0 / SPAN; ND = 2
    OX = X0 - (SPAN-(X1-X0))/2.0
    OY = Y0 - (SPAN-(Y1-Y0))/2.0
def d_attr(ls):
    parts = []
    for lp in ls:
        pts = [(round((x-OX)*SCALE, ND), round((y-OY)*SCALE, ND)) for x, y in lp]
        parts.append('M' + ' '.join('%g,%g' % p for p in pts) + 'Z')
    return ''.join(parts)

def writepng(path, w, h, px):
    raw = b''.join(b'\x00' + bytes(v for x in range(w) for v in px[y][x]) for y in range(h))
    def ch(t, dd):
        c = t+dd; return struct.pack('>I', len(dd)) + c + struct.pack('>I', zlib.crc32(c))
    open(path, 'wb').write(
        b'\x89PNG\r\n\x1a\n'
        + ch(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0))
        + ch(b'IDAT', zlib.compress(raw, 9)) + ch(b'IEND', b''))

def s2l(c): c /= 255.0; return c/12.92 if c <= 0.04045 else ((c+0.055)/1.055) ** 2.4
def l2s(c):
    c = max(0.0, min(1.0, c))
    return round((12.92*c if c <= 0.0031308 else 1.055*c ** (1/2.4)-0.055)*255)
LUT = [s2l(i) for i in range(256)]

def rasterise(layers, size, ss=16):
    """The paths, scan-converted at ss x and box-filtered down in linear light.

    A bitmap made this way is one resample, not two: the old favicon was the
    256 master box-filtered to 64, so it carried the master's own antialiasing
    into the filter and could drift from the vector the moment either changed.
    Averaging sRGB bytes directly would darken every edge -- hence the LUT --
    and colour is averaged over the covered samples alone, so a 5%-covered
    edge pixel keeps the mark's colour at 5% alpha rather than a fifth of it
    over black.
    """
    n = size*ss
    buf = bytearray(n*n*4)
    for subs, rgb in layers:
        edges = []
        for pts in subs:
            for i in range(len(pts)):
                x1, y1 = pts[i]; x2, y2 = pts[(i+1) % len(pts)]
                if y1 != y2: edges.append((x1, y1, x2, y2))
        k = n/float(W)                      # master pixels -> samples
        for sy in range(n):
            yc = (sy+0.5)/k; xs = []
            for x1, y1, x2, y2 in edges:
                if (y1 <= yc < y2) or (y2 <= yc < y1):
                    xs.append(x1 + (yc-y1)*(x2-x1)/(y2-y1))
            xs.sort(); row = sy*n*4
            for i in range(0, len(xs)-1, 2):   # even-odd: fill between pairs
                a = max(0, int(xs[i]*k + 0.5)); b = min(n, int(xs[i+1]*k + 0.5))
                for sx in range(a, b):
                    o = row + sx*4
                    buf[o] = rgb[0]; buf[o+1] = rgb[1]; buf[o+2] = rgb[2]; buf[o+3] = 255
    out = [[None]*size for _ in range(size)]
    tot = ss*ss
    for y in range(size):
        for x in range(size):
            R = G = B = 0.0; cov = 0
            for dy in range(ss):
                base = (y*ss+dy)*n*4
                for dx in range(ss):
                    o = base + (x*ss+dx)*4
                    if buf[o+3]:
                        R += LUT[buf[o]]; G += LUT[buf[o+1]]; B += LUT[buf[o+2]]; cov += 1
            out[y][x] = (0, 0, 0, 0) if not cov else (
                l2s(R/cov), l2s(G/cov), l2s(B/cov), round(cov/tot*255))
    return out

def modal_colour(letter):
    """The class's own colour, straight out of the master -- no hand-picked hex.

    The dark stroke comes in two shades a single level apart (the E's is
    #2b2b2b, the A's #2a2a2a); one path can only carry one of them, and the
    taller pile wins. A 1/255 flattening is below anything an eye or a
    downscaler will register.
    """
    tally = {}
    for y in range(H):
        for x in range(W):
            if cls[y][x] != letter: continue
            i = x*NCH
            if NCH == 4 and PX[y][i+3] != 255: continue
            tally[PX[y][i:i+3]] = tally.get(PX[y][i:i+3], 0) + 1
    best = max(tally, key=tally.get)
    return '#%02x%02x%02x' % tuple(best), tally[best], sum(tally.values())

LAYERS = (('outline', 'o'), ('E', 'g'), ('A', 'r')) if MODE == 'layers' \
    else (('E', 'g'), ('A', 'r'))
res = {}
for name, letter in LAYERS:
    mask = despeck(mask_for(letter))
    ls = [simplify(l, TOL) for l in loops(mask)]
    ls = [l for l in ls if len(l) >= 3]
    res[name] = (letter, ls)
    print(name, len(ls), 'loop(s)', [len(l) for l in ls], 'verts')

if SVG or PNG:
    paint = []
    for name, letter in LAYERS:
        fill, n, tot = modal_colour(letter)
        print('%-7s fill %s (%d/%d px)' % (name, fill, n, tot))
        paint.append((res[name][1], fill))
if PNG:
    # The favicon and anything else raster comes off the vector, not off the
    # master render, so the two cannot drift.
    layers = [(subs, tuple(int(f[i:i+2], 16) for i in (1, 3, 5))) for subs, f in paint]
    writepng(OUT, SIZE, SIZE, rasterise(layers, SIZE))
elif SVG:
    body = ['  <path d="%s" fill="%s" fill-rule="evenodd"/>' % (d_attr(ls), fill)
            for ls, fill in paint]
    open(OUT, 'w').write(
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" '
        'width="%d" height="%d" role="img" aria-label="EmbArch">\n'
        '  <title>EmbArch</title>\n%s\n</svg>\n'
        % (W, H, W, H, '\n'.join(body)))
else:
    open(OUT, 'w').write(''.join(
        '%s %s\n' % (name, d_attr(ls)) for name, (letter, ls) in res.items()))
if not PNG:
    for name, (letter, ls) in res.items():
        print(name, 'd length', len(d_attr(ls)))
print('wrote', OUT)
