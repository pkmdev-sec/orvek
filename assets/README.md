# README logo

`orvek-logo.gif` is a 960 × 360 animated pixel-art logo. Its 120 frames loop every
4.8 seconds at 25 fps. `orvek-logo.png` is the matching still image for readers
who prefer reduced motion.

The voxel diamond follows `favicon.svg`. The wordmark adapts the terminal
logo in `bin/orvek/src/tui/components/brand.rs` to seven-row glyphs for readability. Colors follow Orvek's violet, cyan,
cream, and cappuccino-pink palette. No external fonts or image assets are needed.

Regenerate both files from the repository root:

```sh
uv run scripts/generate-readme-logo.py
uv run scripts/generate-readme-logo.py --check
```

The script pins Pillow and uses a fixed color palette, nearest-neighbor scaling,
and a periodic animation to keep pixel edges sharp and the loop continuous.

## Capability diagram

`orvex-differentiators.gif` is the 1280 × 800 thirteen-area capability animation embedded before
installation instructions in the root README. It loops every eight seconds at 25 fps.
`orvex-differentiators.png` is the reduced-motion fallback. Card text is in
`capabilities.json`. Regenerate and check both assets from the repository root:

```sh
uv run scripts/generate-capability-diagram.py
uv run scripts/generate-capability-diagram.py --check
```

The generator checks all text bounds and compares rendered bytes with the checked-in assets.
It is development-only and does not enter application builds.

`orvex-comparison.gif` is the matching animated comparison with Codex and Claude Code,
shown in the root README's Differentiators section. Its sources and qualifications
are listed next to the image; supporting marketing exports remain local.
