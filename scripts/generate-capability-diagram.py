# /// script
# requires-python = ">=3.11"
# dependencies = ["Pillow==11.3.0"]
# ///
"""Build the README capability diagram: uv run scripts/generate-capability-diagram.py."""

import argparse
import json
import math
from functools import cache
from io import BytesIO
from itertools import pairwise
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets"
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
ASSET = "orvex-differentiators"
CONTENT = json.loads((OUT / "capabilities.json").read_text())


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
    image = Image.new("RGB", SIZE, BG)
    draw = ImageDraw.Draw(image)
    phase = index / FRAMES
    data = CONTENT["overview"]
    cards = [
        capability["diagram"]
        for capability in CONTENT["capabilities"]
        if capability.get("diagram") is not None
    ]
    for x in range(32, SIZE[0], 32):
        for y in range(32, SIZE[1], 32):
            draw.point((x, y), fill=(36, 31, 46))
    label(draw, (56, 34), CONTENT["brand"], 48)
    label(draw, (250, 49), data["title"], 30)
    label(draw, (56, 112), data["subtitle"], 21, MUTED)
    crop, mask = diamond_frames()[int((phase % 1) * len(diamond_frames()))]
    image.paste(crop.resize((103, 89)), (1130, 24), mask.resize((103, 89)))
    for x in (438, 834):
        draw.line((x, 160, x, 708), fill=(67, 55, 84), width=2)
        path_particle(draw, [(x, 160), (x, 708)], phase, VIOLET)
    icons = (
        (
            "0001000000",
            "1111111111",
            "0001000000",
            "0000000000",
            "0000001000",
            "1111111111",
            "0000001000",
            "0000000000",
            "0010000000",
            "1111111111",
            "0010000000",
        ),
        (
            "1111111100",
            "1000000100",
            "1011100100",
            "1000000100",
            "1011100100",
            "1000000100",
            "1111111100",
            "0000100001",
            "0000010010",
            "0000001100",
        ),
        (
            "0011111100",
            "1100000011",
            "1111111111",
            "1000000001",
            "1100000011",
            "1111111111",
            "1000000001",
            "1100000011",
            "0011111100",
        ),
        (
            "1111111111",
            "0000000000",
            "1111111111",
            "0000000000",
            "0011111100",
            "0000000000",
            "0011111100",
            "0000000000",
            "0001111000",
        ),
        (
            "0001111000",
            "0001111000",
            "0000110000",
            "0000110000",
            "0111111110",
            "0100110010",
            "0100110010",
            "1110110111",
            "1110110111",
        ),
        (
            "0111111000",
            "1100001100",
            "1001100100",
            "1011100100",
            "1011110100",
            "1000000100",
            "1100001100",
            "0111111100",
            "0000000110",
            "0000000011",
        ),
        (
            "0011111100",
            "0100000010",
            "1000000001",
            "1000001001",
            "1000010001",
            "1001100001",
            "1000000001",
            "0111111110",
        ),
        (
            "1111111111",
            "1000000001",
            "1010010001",
            "1001100001",
            "1000000001",
            "1010010001",
            "1001100001",
            "1000000001",
            "1111111111",
        ),
    )
    icons += (
        ("11111111", "10000001", "10111101", "10100101", "10111101", "10000001", "11111111"),
        ("11000011", "01100110", "00111100", "00011000", "00111100", "01100110", "11000011"),
        ("01111110", "01011010", "11111111", "10000001", "10110101", "10000001", "11111111"),
        ("00001000", "00011100", "01110110", "11000011", "00011000", "00011000", "00011000"),
        ("11000000", "11110000", "00111100", "00001111", "00000011", "00111100", "11111100"),
    )
    for number, card in enumerate(cards):
        column, row = number % 3, number // 3
        x, y = 56 + column * 396, 174 + row * 110
        accent = ACCENTS[number % len(ACCENTS)]
        energy = max(0, math.cos(math.tau * (phase - number / len(cards)))) ** 8
        panel(draw, (x + 4, y + 6, x + 374, y + 100), (11, 10, 16), (11, 10, 16))
        panel(
            draw,
            (x, y, x + 370, y + 94),
            (27, 23, 36),
            blend((68, 56, 88), accent, 0.25 + energy * 0.6),
        )
        label(draw, (x + 20, y + 13), f"{number + 1:02}", 15, accent)
        for iy, pixels in enumerate(icons[number]):
            for ix, pixel in enumerate(pixels):
                if pixel == "1":
                    left, top = x + 20 + ix * 3, y + 42 + iy * 3
                    draw.rectangle((left, top, left + 2, top + 2), fill=accent)
        top = y + (94 - len(card["lines"]) * 24) // 2
        for line, value in enumerate(card["lines"]):
            label(draw, (x + 66, top + line * 24), value, 20, INK, 284)
    draw.line((56, 738, 1224, 738), fill=(55, 47, 70), width=1)
    label(draw, (56, 758), data["footnote"], 19, MUTED)
    return image


def render_assets():
    assert CONTENT["schema_version"] == 1
    assert sum(capability.get("diagram") is not None for capability in CONTENT["capabilities"]) == 13
    assert overview(0).tobytes() == overview(FRAMES).tobytes()
    frames = [overview(index) for index in range(FRAMES)]
    samples = Image.new("RGB", (SIZE[0] * 4, SIZE[1] * 4))
    for index in range(16):
        samples.paste(frames[index * FRAMES // 16], ((index % 4) * SIZE[0], (index // 4) * SIZE[1]))
    palette = samples.quantize(colors=224, dither=Image.Dither.NONE)
    indexed = [frame.quantize(palette=palette, dither=Image.Dither.NONE) for frame in frames]
    animation, still = BytesIO(), BytesIO()
    indexed[0].save(animation, format="GIF", save_all=True, append_images=indexed[1:],
                    duration=DURATION, loop=0, optimize=False, disposal=1)
    indexed[0].convert("RGB").save(still, format="PNG", optimize=True)
    animation.seek(0)
    with Image.open(animation) as encoded:
        assert encoded.size == SIZE and encoded.n_frames == FRAMES
        assert encoded.info["loop"] == 0
        for index in range(FRAMES):
            encoded.seek(index)
            assert encoded.info["duration"] == DURATION
    return {"gif": animation.getvalue(), "png": still.getvalue()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="reject stale diagram assets")
    args = parser.parse_args()
    for suffix, data in render_assets().items():
        path = OUT / f"{ASSET}.{suffix}"
        if args.check:
            if not path.exists() or path.read_bytes() != data:
                raise SystemExit(f"{path.relative_to(ROOT)} is stale; rerun this script")
        else:
            path.write_bytes(data)
    print("Verified 13 capability areas, text bounds, seamless 200-frame loop, and reduced-motion still.")
