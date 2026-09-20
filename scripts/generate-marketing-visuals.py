# /// script
# requires-python = ">=3.11"
# dependencies = ["Pillow==11.3.0"]
# ///
"""Build Orvex marketing artwork: uv run scripts/generate-marketing-visuals.py."""

import argparse
import hashlib
import json
import math
from functools import cache
from itertools import pairwise
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets/marketing"
FRAMES_DIR = ROOT / ".agent-map/orvex-marketing/frames"
SIZE = (1280, 800)
FRAMES = 200
DURATION = 40
BG = (18, 16, 24)
INK = (242, 231, 213)
MUTED = (174, 168, 189)
VIOLET = (182, 161, 242)
CYAN = (137, 201, 232)
PINK = (214, 177, 171)
ACCENTS = (VIOLET, CYAN, PINK, INK)
ASSETS = ("orvex-differentiators", "orvex-comparison")
CONTENT = json.loads((OUT / "content.json").read_text())


def blend(a, b, amount):
    return tuple(round(x + (y - x) * amount) for x, y in zip(a, b))


@cache
def font(size):
    return ImageFont.load_default(size=size)


def label(draw, xy, value, size=22, color=INK, width=None):
    x, _ = xy
    bounds = draw.textbbox(xy, value, font=font(size), anchor="lt")
    assert bounds[2] <= (x + width if width else SIZE[0] - 40), (value, bounds)
    assert 0 <= bounds[0] and 0 <= bounds[1] and bounds[3] < SIZE[1], value
    draw.text(xy, value, font=font(size), fill=color, anchor="lt")


def panel(draw, bounds, fill, outline, step=10):
    x, y, right, bottom = bounds
    points = [
        (x + step, y),
        (right - step, y),
        (right - step, y + step),
        (right, y + step),
        (right, bottom - step),
        (right - step, bottom - step),
        (right - step, bottom),
        (x + step, bottom),
        (x + step, bottom - step),
        (x, bottom - step),
        (x, y + step),
        (x + step, y + step),
    ]
    draw.polygon(points, fill=fill)
    draw.line(points + [points[0]], fill=outline, width=2)


def base():
    image = Image.new("RGB", SIZE, BG)
    draw = ImageDraw.Draw(image)
    for x in range(32, 1280, 32):
        for y in range(32, 800, 32):
            draw.point((x, y), fill=(36, 31, 46))
    draw.line((64, 242, 1216, 242), fill=(55, 47, 70), width=1)
    return image, draw


@cache
def diamond_frames():
    frames = []
    with Image.open(ROOT / "assets/orvek-logo.gif") as source:
        for index in range(source.n_frames):
            source.seek(index)
            crop = source.convert("RGB").crop((24, 24, 368, 320))
            mask = ImageChops.difference(crop, Image.new("RGB", crop.size, BG))
            mask = mask.convert("L").point(lambda value: 255 if value else 0)
            frames.append((crop, mask))
    return frames


def path_particle(draw, points, progress, color):
    lengths = [math.dist(a, b) for a, b in pairwise(points)]
    distance = (progress % 1) * sum(lengths)
    for start, end, length in zip(points, points[1:], lengths):
        if distance <= length:
            fraction = distance / length
            x, y = (round(a + (b - a) * fraction) for a, b in zip(start, end))
            draw.rectangle((x - 5, y - 5, x + 5, y + 5), fill=blend(BG, color, 0.2))
            draw.rectangle((x - 2, y - 2, x + 2, y + 2), fill=color)
            break
        distance -= length


def overview(index):
    image, draw = base()
    phase = index / FRAMES
    data = CONTENT["overview"]
    label(draw, (64, 38), data["eyebrow"], 16, VIOLET)
    label(draw, (62, 73), CONTENT["brand"], 64)
    label(draw, (64, 154), data["title"], 36)
    label(draw, (64, 206), data["subtitle"], 21, MUTED)
    paths = [
        [(410, 352), (480, 352), (480, 428), (564, 428)],
        [(410, 570), (480, 570), (480, 452), (564, 452)],
        [(716, 428), (800, 428), (800, 352), (870, 352)],
        [(716, 452), (800, 452), (800, 570), (870, 570)],
    ]
    positions = [(64, 280), (64, 498), (870, 280), (870, 498)]
    for number, (card, (x, y), points, accent) in enumerate(
        zip(data["cards"], positions, paths, ACCENTS)
    ):
        energy = max(0, math.cos(math.tau * (phase - number / 4))) ** 8
        draw.line(
            points, fill=blend((52, 44, 68), accent, 0.15 + energy * 0.35), width=2
        )
        for offset in (0, 0.5):
            path_particle(draw, points, phase * 2 + offset, accent)
        panel(draw, (x + 5, y + 8, x + 351, y + 152), (11, 10, 16), (11, 10, 16))
        panel(
            draw,
            (x, y, x + 346, y + 144),
            (27, 23, 36),
            blend((68, 56, 88), accent, energy * 0.7),
        )
        draw.rectangle(
            (x + 22, y + 24, x + 49, y + 51), fill=blend((27, 23, 36), accent, 0.16)
        )
        label(draw, (x + 28, y + 29), str(number + 1), 18, accent)
        label(draw, (x + 64, y + 29), card["title"], 23, INK, 264)
        for line, value in enumerate(card["lines"]):
            label(draw, (x + 24, y + 76 + line * 28), value, 19, MUTED, 298)
    frames = diamond_frames()
    crop, mask = frames[int((phase % 1) * len(frames))]
    image.paste(crop, (468, 304), mask)
    draw = ImageDraw.Draw(image)
    label(draw, (578, 613), "ONE LOCAL HOST", 16, VIOLET)
    label(draw, (488, 643), "Your work stays connected.", 22, INK)
    draw.line((64, 703, 1216, 703), fill=(55, 47, 70), width=1)
    label(draw, (64, 727), data["footnote"], 16, MUTED)
    label(draw, (64, 763), "Orvex / terminal-native, workflow-aware", 14, VIOLET)
    return image


def comparison(index):
    image, draw = base()
    phase = index / FRAMES
    data = CONTENT["comparison"]
    label(draw, (64, 38), "Orvex / WORKFLOW COMPARISON", 17, VIOLET)
    label(draw, (62, 91), data["title"], 46)
    label(draw, (64, 160), data["subtitle"], 21, MUTED)
    draw.rectangle((64, 227, 1216, 246), fill=BG)
    columns = [(262, 590), (590, 903), (903, 1216)]
    panel(draw, (64, 236, 1216, 679), (24, 21, 31), (61, 52, 77))
    draw.rectangle((262, 238, 589, 677), fill=(37, 29, 52))
    draw.rectangle((262, 238, 589, 243), fill=VIOLET)
    label(draw, (86, 276), "WORKFLOW", 15, MUTED)
    for product, (left, right) in zip(data["columns"], columns):
        label(
            draw,
            (left + 24, 269),
            product,
            30,
            VIOLET if product == "Orvex" else INK,
            right - left - 48,
        )
    for row_index, row in enumerate(data["rows"]):
        y = 324 + row_index * 87
        energy = max(0, math.cos(math.tau * (phase - row_index / 4))) ** 8
        draw.line((64, y, 1216, y), fill=(56, 48, 70), width=1)
        draw.rectangle(
            (263, y + 1, 588, y + 86),
            fill=blend((37, 29, 52), (59, 44, 79), energy * 0.65),
        )
        draw.rectangle(
            (264, y + 13, 267, y + 73), fill=blend((62, 48, 80), VIOLET, energy)
        )
        for line, value in enumerate(row["label"]):
            label(draw, (86, y + 23 + line * 26), value, 18, MUTED, 158)
        for cell, (left, right) in zip(row["cells"], columns):
            for line, value in enumerate(cell):
                label(
                    draw,
                    (left + 24, y + 22 + line * 28),
                    value,
                    20,
                    INK,
                    right - left - 48,
                )
    for x in (262, 590, 903):
        draw.line((x, 245, x, 677), fill=(60, 49, 78), width=1)
    path_particle(
        draw,
        [(264, 240), (588, 240), (588, 675), (264, 675), (264, 240)],
        phase,
        VIOLET,
    )
    label(
        draw,
        (64, 704),
        "Orvex: opt-in memory; Docker for children; web assets for browser review.",
        16,
        VIOLET,
    )
    label(draw, (64, 738), data["footnote"], 15, MUTED)
    label(
        draw,
        (64, 768),
        "Documentation checked "
        + CONTENT["checked_on"]
        + " / Details and caveats: SOURCES.md",
        13,
        MUTED,
    )
    return image


def generate(name, renderer, keep_frames):
    frames = [renderer(index) for index in range(FRAMES)]
    samples = Image.new("RGB", (SIZE[0] * 4, SIZE[1] * 4))
    for index in range(16):
        samples.paste(
            frames[index * FRAMES // 16],
            ((index % 4) * SIZE[0], (index // 4) * SIZE[1]),
        )
    palette = samples.quantize(colors=224, dither=Image.Dither.NONE)
    indexed = [
        frame.quantize(palette=palette, dither=Image.Dither.NONE) for frame in frames
    ]
    indexed[0].save(
        OUT / f"{name}.gif",
        save_all=True,
        append_images=indexed[1:],
        duration=DURATION,
        loop=0,
        optimize=False,
        disposal=1,
    )
    indexed[0].convert("RGB").save(OUT / f"{name}.png", optimize=True)
    frames[0].save(
        OUT / f"{name}.webp",
        save_all=True,
        append_images=frames[1:],
        duration=DURATION,
        loop=0,
        lossless=True,
        method=4,
    )
    if keep_frames:
        target = FRAMES_DIR / name
        target.mkdir(parents=True, exist_ok=True)
        for index, frame in enumerate(frames):
            frame.save(target / f"{index:03}.png", compress_level=1)
    contact = Image.new("RGB", (SIZE[0], SIZE[1]))
    for index in range(4):
        thumb = frames[index * FRAMES // 4].resize((640, 400))
        contact.paste(thumb, ((index % 2) * 640, (index // 2) * 400))
    contact.save(ROOT / f".agent-map/orvex-marketing/{name}-contact-sheet.png")
    print(f"Generated {name}: {FRAMES} frames, 25 fps, 8-second loop", flush=True)


def verify():
    assert CONTENT["brand"] == "Orvex"
    assert CONTENT["comparison"]["columns"] == ["Orvex", "Codex", "Claude Code"]
    assert len(CONTENT["comparison"]["rows"]) == 4
    assert "orvek" not in json.dumps(CONTENT).lower()
    results = {}
    for name, renderer in zip(ASSETS, (overview, comparison)):
        assert renderer(0).tobytes() == renderer(FRAMES).tobytes(), name
        with Image.open(OUT / f"{name}.gif") as animation:
            assert animation.size == SIZE and animation.n_frames == FRAMES
            assert animation.info["loop"] == 0
            first = animation.convert("RGB")
            hashes = set()
            for index in range(FRAMES):
                animation.seek(index)
                assert animation.info["duration"] == DURATION
                hashes.add(hashlib.sha256(animation.convert("RGB").tobytes()).digest())
            assert len(hashes) == FRAMES, (name, len(hashes))
        with Image.open(OUT / f"{name}.png") as still:
            assert still.convert("RGB").tobytes() == first.tobytes()
        with Image.open(OUT / f"{name}.webp") as animation:
            assert animation.n_frames == FRAMES and animation.size == SIZE
            assert animation.info["loop"] == 0
        assert (OUT / f"{name}.gif").stat().st_size < 8_000_000
        results[name] = {
            "size": SIZE,
            "frames": FRAMES,
            "duration_ms": FRAMES * DURATION,
            "files": {
                suffix: {
                    "bytes": (OUT / f"{name}.{suffix}").stat().st_size,
                    "sha256": hashlib.sha256(
                        (OUT / f"{name}.{suffix}").read_bytes()
                    ).hexdigest(),
                }
                for suffix in ("gif", "png", "webp")
            },
        }
    (ROOT / ".agent-map/orvex-marketing/verification.json").write_text(
        json.dumps(results, indent=2) + "\n"
    )
    print(
        "Verified branding, bounds, timing, unique frames, seamless renderer loops, and still fallbacks."
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify existing assets")
    parser.add_argument(
        "--frames", action="store_true", help="keep PNG frames for video encoding"
    )
    args = parser.parse_args()
    (ROOT / ".agent-map/orvex-marketing").mkdir(parents=True, exist_ok=True)
    if not args.check:
        for name, renderer in zip(ASSETS, (overview, comparison)):
            generate(name, renderer, args.frames)
    verify()
