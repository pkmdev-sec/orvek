# /// script
# requires-python = ">=3.11"
# dependencies = ["Pillow==11.3.0"]
# ///
"""Generate Orvek's README artwork: uv run scripts/generate-readme-logo.py."""

import argparse
import hashlib
import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
SIZE = (480, 180)
SCALE = 2
FRAMES = 120
DURATION = 40
BACKGROUND = (18, 16, 24)
PALETTE = [(182, 161, 242), (137, 201, 232), (214, 177, 171), (242, 231, 213)]
# Seven-row README adaptation of the terminal's block-letter wordmark.
GLYPHS = [
    [14, 17, 17, 17, 17, 17, 14],
    [30, 17, 17, 30, 20, 18, 17],
    [17, 17, 17, 17, 17, 10, 4],
    [31, 16, 16, 30, 16, 16, 31],
    [17, 18, 20, 24, 20, 18, 17],
]
CAPTION = {
    "A": [14, 17, 17, 31, 17, 17, 17],
    "C": [14, 17, 16, 16, 16, 17, 14],
    "D": [30, 17, 17, 17, 17, 17, 30],
    "E": GLYPHS[3],
    "G": [14, 17, 16, 23, 17, 17, 14],
    "I": [14, 4, 4, 4, 4, 4, 14],
    "N": [17, 25, 25, 21, 19, 19, 17],
    "O": GLYPHS[0],
    "T": [31, 4, 4, 4, 4, 4, 4],
    "V": GLYPHS[2],
}
FACES = [
    ((0, 0, 1), [(-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1)]),
    ((0, 0, -1), [(1, -1, -1), (-1, -1, -1), (-1, 1, -1), (1, 1, -1)]),
    ((1, 0, 0), [(1, -1, 1), (1, -1, -1), (1, 1, -1), (1, 1, 1)]),
    ((-1, 0, 0), [(-1, -1, -1), (-1, -1, 1), (-1, 1, 1), (-1, 1, -1)]),
    ((0, 1, 0), [(-1, 1, 1), (1, 1, 1), (1, 1, -1), (-1, 1, -1)]),
    ((0, -1, 0), [(-1, -1, -1), (1, -1, -1), (1, -1, 1), (-1, -1, 1)]),
]


def blend(a, b, amount):
    return tuple(round(x + (y - x) * amount) for x, y in zip(a, b))


def view(point, angle):
    x, y, z = point
    x, z = (
        x * math.cos(angle) + z * math.sin(angle),
        -x * math.sin(angle) + z * math.cos(angle),
    )
    pitch = 0.28
    return (
        x,
        y * math.cos(pitch) - z * math.sin(pitch),
        y * math.sin(pitch) + z * math.cos(pitch),
    )


def wordmark(draw, phase):
    for letter, glyph in enumerate(GLYPHS):
        for row, bits in enumerate(glyph):
            for column in range(5):
                if not bits & (1 << (4 - column)):
                    continue
                x, y = 210 + (letter * 6 + column) * 8, 59 + row * 8
                draw.rectangle((x + 4, y + 4, x + 10, y + 10), fill=(48, 37, 68))
    for letter, glyph in enumerate(GLYPHS):
        for row, bits in enumerate(glyph):
            for column in range(5):
                if not bits & (1 << (4 - column)):
                    continue
                x, y = 210 + (letter * 6 + column) * 8, 59 + row * 8
                for sy in range(0, 7, 2):
                    for sx in range(0, 7, 2):
                        base = PALETTE[0 if letter < 3 else 1]
                        grain = (
                            (letter * 23 + row * 13 + column * 7 + sx * 3 + sy * 5) % 11
                        ) / 10
                        sweep = (1 + math.sin(phase - (x + sx) / 42 + row / 3)) / 2
                        color = blend(
                            base,
                            PALETTE[2 if grain > 0.7 else 3],
                            0.13 + 0.27 * grain * sweep,
                        )
                        draw.rectangle(
                            (x + sx, y + sy, x + sx + 1, y + sy + 1), fill=color
                        )
    x = 211
    for letter in "NATIVE CODING AGENT":
        for row, bits in enumerate(CAPTION.get(letter, [0] * 7)):
            for column in range(5):
                if bits & (1 << (4 - column)):
                    draw.rectangle(
                        (
                            x + column * 2,
                            126 + row * 2,
                            x + column * 2 + 1,
                            127 + row * 2,
                        ),
                        fill=(145, 139, 163),
                    )
        x += 12


def render(index, voxels):
    phase = math.tau * index / FRAMES
    angle = phase + 0.42
    image = Image.new("RGB", SIZE, BACKGROUND)
    draw = ImageDraw.Draw(image)
    # Pixel-stepped ambient light and a grounded, unanimated shadow.
    for radius in range(72, 0, -4):
        strength = (1 - radius / 80) * 0.19
        draw.ellipse(
            (98 - radius, 81 - radius, 98 + radius, 81 + radius),
            fill=blend(BACKGROUND, (72, 47, 103), strength),
        )
    for offset in range(-3, 4):
        color = (32, 28, 44)
        draw.line((38 + offset * 10, 139, 98 + offset * 10, 157), fill=color)
        draw.line((38 + offset * 10, 157, 98 + offset * 10, 139), fill=color)
    draw.ellipse((57, 144, 138, 156), fill=(12, 11, 18))
    faces = []
    for x, y, z in voxels:
        grain = ((x * 41 + y * 67 + z * 31) % 101) / 100
        base = PALETTE[0 if x + z < 0 else 1]
        base = blend(base, PALETTE[2], 0.55 if grain > 0.84 else 0.0)
        for normal, corners in FACES:
            nx, ny, nz = view(normal, angle)
            if nz <= 0:
                continue
            vertices = [
                view((x + a * 0.46, y + b * 0.46, z + c * 0.46), angle)
                for a, b, c in corners
            ]
            points = [
                (round(98 + vx * 6.3), round(80 - vy * 6.3 + math.sin(phase) * 2))
                for vx, vy, _ in vertices
            ]
            light = max(0.0, min(1.0, (-nx * 0.35 + ny * 0.65 + nz * 0.7) * 0.75))
            color = blend((35, 28, 58), base, 0.45 + light * 0.5)
            color = blend(color, PALETTE[3], 0.12 * grain * light)
            faces.append((sum(v[2] for v in vertices) / 4, points, color))
    for _, points, color in sorted(faces, key=lambda face: face[0]):
        draw.polygon(points, fill=color)
    wordmark(draw, phase)
    return image.resize((SIZE[0] * SCALE, SIZE[1] * SCALE), Image.Resampling.NEAREST)


def main():
    voxels = [
        (x, y, z)
        for x in range(-8, 9)
        for y in range(-8, 9)
        for z in range(-8, 9)
        if 7 <= abs(x) + abs(y) + abs(z) <= 8
    ]
    frames = [render(index, voxels) for index in range(FRAMES)]
    # A fixed palette prevents color flicker; no dithering blurs the pixel edges.
    samples = Image.new("RGB", (frames[0].width * 4, frames[0].height * 4))
    for index in range(16):
        samples.paste(
            frames[index * FRAMES // 16],
            ((index % 4) * frames[0].width, (index // 4) * frames[0].height),
        )
    palette = samples.quantize(colors=192, dither=Image.Dither.NONE)
    indexed = [
        frame.quantize(palette=palette, dither=Image.Dither.NONE) for frame in frames
    ]
    assets = ROOT / "assets"
    indexed[0].save(
        assets / "orvek-logo.gif",
        save_all=True,
        append_images=indexed[1:],
        duration=DURATION,
        loop=0,
        optimize=False,
        disposal=1,
    )
    indexed[0].convert("RGB").save(assets / "orvek-logo.png", optimize=True)
    print(
        f"Saved {FRAMES} frames at {1000 // DURATION} fps; {FRAMES * DURATION / 1000:g}s seamless loop"
    )


def verify():
    assets = ROOT / "assets"
    with Image.open(assets / "orvek-logo.gif") as animation:
        assert animation.size == (SIZE[0] * SCALE, SIZE[1] * SCALE)
        assert animation.n_frames == FRAMES and animation.info["loop"] == 0
        first = animation.convert("RGB")
        fingerprints = set()
        for index in range(animation.n_frames):
            animation.seek(index)
            assert animation.info["duration"] == DURATION
            fingerprints.add(
                hashlib.sha256(animation.convert("RGB").tobytes()).digest()
            )
        assert len(fingerprints) == FRAMES
    with Image.open(assets / "orvek-logo.png") as still:
        assert still.convert("RGB").tobytes() == first.tobytes()
    assert (assets / "orvek-logo.gif").stat().st_size < 3_000_000
    print(
        "Verified dimensions, frame timing, infinite loop, distinct frames, and static fallback"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify existing assets without rewriting them",
    )
    if not parser.parse_args().check:
        main()
    verify()
