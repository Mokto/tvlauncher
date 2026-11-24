#!/usr/bin/env python3
"""Writes made-up Continue watching and YouTube rows with generated artwork into a simulator root, so a screenshot shows
no one's posters or thumbnails:

    cp -R fixtures /tmp/demo && python3 tools/demo-fixtures.py /tmp/demo
    LAUNCHER_ROOT=/tmp/demo target/release/tvlauncher --shot build/demo.bmp "D"
"""
import json, os, random, sys
from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = sys.argv[1]
DIR = "/media/developer/tvlauncher"
OUT = ROOT + DIR + "/cache"
os.makedirs(OUT, exist_ok=True)
for f in os.listdir(OUT):
    os.remove(f"{OUT}/{f}")

PALETTES = [
    ((14, 22, 48), (214, 110, 84), (255, 196, 120)),
    ((10, 30, 40), (46, 140, 150), (190, 235, 210)),
    ((34, 14, 44), (172, 70, 120), (255, 170, 150)),
    ((12, 18, 34), (70, 90, 190), (160, 200, 255)),
    ((30, 22, 12), (190, 120, 40), (255, 226, 150)),
    ((8, 24, 24), (30, 120, 90), (170, 240, 150)),
]


def art(seed, w, h):
    rnd = random.Random(seed)
    top, mid, sun = PALETTES[seed % len(PALETTES)]
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        t = y / h
        c = [int(top[i] * (1 - t) + mid[i] * t) for i in range(3)]
        for x in range(w):
            px[x, y] = tuple(c)
    glow = Image.new("RGB", (w, h), (0, 0, 0))
    g = ImageDraw.Draw(glow)
    cx, cy, r = int(w * rnd.uniform(0.62, 0.86)), int(h * rnd.uniform(0.34, 0.5)), int(h * 0.16)
    g.ellipse([cx - r, cy - r, cx + r, cy + r], fill=sun)
    glow = glow.filter(ImageFilter.GaussianBlur(h * 0.05))
    im = ImageChops.add(im, glow)
    d = ImageDraw.Draw(im)
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=sun)
    for layer in range(3):
        base = h * (0.58 + 0.12 * layer)
        pts, x = [], 0
        while x <= w + 40:
            pts.append((x, base - rnd.uniform(0, h * (0.2 - 0.04 * layer))))
            x += w / rnd.randint(6, 12)
        pts += [(w, h), (0, h)]
        shade = [int(mid[i] * (0.62 - 0.2 * layer) + top[i] * 0.2) for i in range(3)]
        d.polygon(pts, fill=tuple(shade))
    return im


def save(im, name):
    im.convert("RGB").save(f"{OUT}/{name}", format="JPEG", quality=88)
    return f"{DIR}/cache/{name}"


watching = [
    ("Northern Passage", "S1 · E4 — The Long Night", "A cargo crew is trapped by pack ice and has to decide what to leave behind.", 0.62),
    ("Harbour Lights", "2023 · 1 h 52 min", "Two strangers share one night in a harbour town before the last ferry leaves.", 0.18),
    ("Glass Mountain", "S2 · E1 — Ascent", "The expedition reaches the base camp and the weather turns.", 0.85),
    ("The Quiet Orbit", "2021 · 2 h 06 min", "A repair mission on a silent station goes wrong, and the only help is a voice on the radio.", 0.4),
]
items = []
for i, (title, sub, text, prog) in enumerate(watching):
    items.append(
        {
            "id": f"demo{i}",
            "title": title,
            "sub": f"{sub}\n{text}",
            "progress": prog,
            "card": save(art(10 + i, 384, 216), f"card-demo{i}.img"),
            "bg": save(art(10 + i, 1920, 1080), f"bg-demo{i}.img"),
        }
    )
json.dump({"items": items}, open(f"{OUT}/watching.json", "w"))

plex = [
    ("Salt Road", "S3 · E2 — Low Tide", "Two rival smugglers are forced to share a boat when the coast guard closes the harbour.", 0.33),
    ("Paper Lanterns", "2022", "A retired lantern maker is asked to light one last festival.", 0.7),
    ("Deep Field", "S1 · E6 — Signal", "The observatory picks up something that repeats every eleven minutes.", 0.1),
]
pitems = []
for i, (title, sub, text, prog) in enumerate(plex):
    pitems.append(
        {
            "server": "demoserver",
            "ratingKey": str(300 + i),
            "title": title,
            "sub": f"{sub}\n{text}",
            "progress": prog,
            "card": save(art(20 + i, 640, 360), f"plex-c-demo-{300 + i}-1.img"),
            "bg": save(art(20 + i, 1920, 1080), f"plex-b-demo-{300 + i}-1.img"),
        }
    )
json.dump({"items": pitems}, open(f"{OUT}/plex.json", "w"))

tube = [
    ("Slow Morning Coffee, Rain on the Window", "Quiet Hours", "1.2M views · 3 weeks ago"),
    ("Why Old Bridges Still Stand", "Structure Lab", "480K views · 5 days ago"),
    ("Cooking With One Pan: Five Dinners", "Weeknight Kitchen", "2.1M views · 1 year ago"),
    ("A Week on a Sailboat, Start to Finish", "Blue Margin", "310K views · 2 months ago"),
    ("The Sound of an Empty Concert Hall", "Room Tone", "95K views · 8 days ago"),
    ("Building a Cabin From Fallen Trees", "North Woods", "3.4M views · 6 months ago"),
]
yt = []
for i, (title, chan, views) in enumerate(tube):
    yt.append(
        {
            "id": f"demoyt{i}",
            "title": title,
            "sub": f"{chan}\n{views}",
            "progress": 0.0,
            "card": save(art(40 + i, 640, 360).resize((640, 360)), f"yt-demoyt{i}.img"),
        }
    )
json.dump({"items": yt}, open(f"{OUT}/youtube.json", "w"))

json.dump(
    [{"path": it["bg"], "title": it["title"], "sub": it["sub"]} for it in items],
    open(f"{OUT}/backdrops.json", "w"),
)
for name, body in {
    "jellyfin.json": {"server": "http://127.0.0.1:9", "serverId": "demo", "userId": "demo", "user": "demo", "token": "demo"},
    "plex.json": {"token": "demo", "clientId": "demo", "user": "demo"},
    "youtube.json": {"clientId": "demo", "clientSecret": "demo", "refreshToken": "demo", "accessToken": "demo", "expiresAt": 9999999999},
}.items():
    fd = os.open(f"{ROOT}{DIR}/{name}", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(body, f)
