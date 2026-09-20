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

## Workflow diagram

`orvex-differentiators.gif` is the 1280 × 800 workflow animation embedded before
installation instructions in the root README. It loops every eight seconds at 25 fps.
Supporting exports and the generator stay local in the ignored `assets/marketing/`
folder; they are not needed to build or run the application.
