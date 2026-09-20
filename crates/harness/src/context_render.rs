//! Deterministic bitmap carriers for derived context views.
//!
//! This module only converts exact source text into immutable PNG artifacts.
//! Selection policy, request budgets, and source-history ownership remain in
//! the context controller.

use crate::{
    Digest,
    artifacts::{ArtifactError, ArtifactStore, PublicArtifactRef},
    context::HistoryRange,
};
use png::{BitDepth, ColorType, Compression, Encoder, EncodingError, Filter};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use thiserror::Error;
use zeroize::Zeroizing;

const RENDERER_VERSION: u32 = 1;
const WIDTH: usize = 1568;
const MAX_HEIGHT: usize = 1568;
const CELL_WIDTH: usize = 8;
const CELL_HEIGHT: usize = 16;
const FONT_HEIGHT: usize = 13;
const MARGIN: usize = 16;
const HEADER_ROWS: usize = 3;
const FOOTER_ROWS: usize = 1;
const COLUMNS: usize = (WIDTH - 2 * MARGIN) / CELL_WIDTH;
const TAB_COLUMNS: usize = 8;
const ROW_BYTES: usize = WIDTH / 8;
const PATCH_PIXELS: usize = 32;
const FONT: &[u8; 95 * FONT_HEIGHT] = include_bytes!("context_render/fonts/8x13-ascii.bin");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderProfile {
    PatchAligned8On16,
    PatchAligned8On16Repeated,
}

impl RenderProfile {
    pub const fn id(self) -> &'static str {
        match self {
            Self::PatchAligned8On16 => "ascii-8on16-patch-v1",
            Self::PatchAligned8On16Repeated => "ascii-8on16-patch-repeat2-v1",
        }
    }

    const fn line_repeat(self) -> usize {
        match self {
            Self::PatchAligned8On16 => 1,
            Self::PatchAligned8On16Repeated => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderLimits {
    pub max_pages: usize,
    pub max_png_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BitmapPage {
    pub source: HistoryRange,
    pub width: u32,
    pub height: u32,
    pub digest: Digest,
    pub artifact: PublicArtifactRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BitmapManifest {
    pub version: u32,
    pub renderer: Digest,
    pub profile: RenderProfile,
    pub source: Digest,
    pub source_artifact: PublicArtifactRef,
    pub pages: Vec<BitmapPage>,
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("bitmap rendering was cancelled")]
    Cancelled,
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
}

enum Ineligible {
    Unsupported,
    Limit,
    Cancelled,
}

struct RenderedPage {
    start: usize,
    end: usize,
    width: u32,
    height: u32,
    png: Zeroizing<Vec<u8>>,
}

/// Render eligible ASCII text and store each page as an immutable artifact.
/// Unsupported text and configured rendering limits select native text by
/// returning `Ok(None)`; they do not fail accepted work.
pub fn render_to_artifacts(
    store: &ArtifactStore,
    text: &str,
    profile: RenderProfile,
    limits: RenderLimits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<BitmapManifest>, RenderError> {
    let source = Digest::of(text.as_bytes());
    let renderer = renderer_digest(profile);
    let pages = match render(text, source, profile, limits, &cancelled) {
        Ok(pages) => pages,
        Err(Ineligible::Unsupported | Ineligible::Limit) => return Ok(None),
        Err(Ineligible::Cancelled) => return Err(RenderError::Cancelled),
    };
    if cancelled() {
        return Err(RenderError::Cancelled);
    }
    let source_artifact = store.write(text.as_bytes())?;
    let mut stored = Vec::with_capacity(pages.len());
    for page in pages {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        let artifact = store.write(&page.png)?;
        stored.push(BitmapPage {
            source: HistoryRange {
                start: u64::try_from(page.start).unwrap_or(u64::MAX),
                end: u64::try_from(page.end).unwrap_or(u64::MAX),
            },
            width: page.width,
            height: page.height,
            digest: artifact.digest(),
            artifact,
        });
    }
    Ok(Some(BitmapManifest {
        version: RENDERER_VERSION,
        renderer,
        profile,
        source,
        source_artifact,
        pages: stored,
    }))
}

fn renderer_digest(profile: RenderProfile) -> Digest {
    let mut identity = Vec::with_capacity(FONT.len() + 128);
    identity.extend_from_slice(b"orvek-context-bitmap-renderer-v1\0");
    identity.extend_from_slice(profile.id().as_bytes());
    identity.extend_from_slice(&WIDTH.to_le_bytes());
    identity.extend_from_slice(&MAX_HEIGHT.to_le_bytes());
    identity.extend_from_slice(&CELL_WIDTH.to_le_bytes());
    identity.extend_from_slice(&CELL_HEIGHT.to_le_bytes());
    identity.extend_from_slice(&profile.line_repeat().to_le_bytes());
    identity.extend_from_slice(FONT);
    Digest::of(&identity)
}

fn render(
    text: &str,
    source: Digest,
    profile: RenderProfile,
    limits: RenderLimits,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<RenderedPage>, Ineligible> {
    if text.is_empty() || limits.max_pages == 0 || limits.max_png_bytes == 0 {
        return Err(Ineligible::Limit);
    }
    for (offset, byte) in text.bytes().enumerate() {
        if offset % 1024 == 0 && cancelled() {
            return Err(Ineligible::Cancelled);
        }
        match byte {
            b' '..=b'~' | b'\n' | b'\t' => {}
            b'\r' if text.as_bytes().get(offset + 1) == Some(&b'\n') => {}
            _ => return Err(Ineligible::Unsupported),
        }
    }

    let repeat = profile.line_repeat();
    let body_rows = (MAX_HEIGHT / CELL_HEIGHT - HEADER_ROWS - FOOTER_ROWS) / repeat;
    let mut pages = Vec::new();
    let mut start = 0;
    let mut png_bytes = 0;
    while start < text.len() {
        if cancelled() {
            return Err(Ineligible::Cancelled);
        }
        if pages.len() == limits.max_pages {
            return Err(Ineligible::Limit);
        }
        let mut end = start;
        let mut rows = 0;
        while end < text.len() && rows < body_rows {
            end = row_end(text.as_bytes(), end);
            rows += 1;
        }
        let unaligned_height = (HEADER_ROWS + rows * repeat + FOOTER_ROWS) * CELL_HEIGHT;
        let height = unaligned_height
            .next_multiple_of(PATCH_PIXELS)
            .min(MAX_HEIGHT);
        let mut pixels = Zeroizing::new(vec![0xff; ROW_BYTES * height]);
        let heading = format!("SOURCE {source}");
        let locator = format!(
            "PROFILE {} | PAGE {} | BYTES {start}..{end}",
            profile.id(),
            pages.len() + 1
        );
        paint_row(&mut pixels, heading.as_bytes(), 0);
        paint_row(&mut pixels, locator.as_bytes(), 1);
        let separator = 2 * CELL_HEIGHT + CELL_HEIGHT / 2;
        pixels[separator * ROW_BYTES + MARGIN / 8..separator * ROW_BYTES + (WIDTH - MARGIN) / 8]
            .fill(0);

        let mut offset = start;
        for row in 0..rows {
            let next = row_end(text.as_bytes(), offset);
            for copy in 0..repeat {
                paint_row(
                    &mut pixels,
                    &text.as_bytes()[offset..next],
                    HEADER_ROWS + row * repeat + copy,
                );
            }
            offset = next;
        }
        let remaining = limits.max_png_bytes.saturating_sub(png_bytes);
        let png = encode_png(&pixels, height as u32, remaining).map_err(|_| Ineligible::Limit)?;
        png_bytes = png_bytes.checked_add(png.len()).ok_or(Ineligible::Limit)?;
        pages.push(RenderedPage {
            start,
            end,
            width: WIDTH as u32,
            height: height as u32,
            png,
        });
        start = end;
    }
    Ok(pages)
}

fn row_end(text: &[u8], start: usize) -> usize {
    let mut end = start;
    let mut column = 0;
    while let Some(&byte) = text.get(end) {
        match byte {
            b'\n' => return end + 1,
            b'\r' => return end + 2,
            _ if column == COLUMNS => break,
            b'\t' => column += TAB_COLUMNS - column % TAB_COLUMNS,
            _ => column += 1,
        }
        end += 1;
    }
    end
}

fn paint_row(pixels: &mut [u8], text: &[u8], row: usize) {
    let mut column = 0;
    for &byte in text {
        match byte {
            b'\r' | b'\n' => break,
            b'\t' => column += TAB_COLUMNS - column % TAB_COLUMNS,
            _ => {
                let glyph = (byte - b' ') as usize * FONT_HEIGHT;
                let x_byte = MARGIN / 8 + column;
                let y = row * CELL_HEIGHT + 1;
                for (glyph_row, &ink) in FONT[glyph..glyph + FONT_HEIGHT].iter().enumerate() {
                    pixels[(y + glyph_row) * ROW_BYTES + x_byte] &= !ink;
                }
                column += 1;
            }
        }
    }
}

fn encode_png(
    pixels: &[u8],
    height: u32,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>, EncodingError> {
    let mut bytes = Zeroizing::new(Vec::new());
    let sink = BoundedPng {
        bytes: &mut bytes,
        max_bytes,
    };
    let mut encoder = Encoder::new(sink, WIDTH as u32, height);
    encoder.set_color(ColorType::Grayscale);
    encoder.set_depth(BitDepth::One);
    encoder.set_filter(Filter::NoFilter);
    encoder.set_compression(Compression::Balanced);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(bytes)
}

struct BoundedPng<'a> {
    bytes: &'a mut Zeroizing<Vec<u8>>,
    max_bytes: usize,
}

impl Write for BoundedPng<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("bitmap PNG byte limit exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn limits() -> RenderLimits {
        RenderLimits {
            max_pages: 16,
            max_png_bytes: 1_000_000,
        }
    }

    #[test]
    fn golden_pages_are_deterministic_patch_aligned_and_durable() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let store = Store::open(&state).unwrap();
        let source = "fn main() {\n\tprintln!(\"hello\");\n}\n";
        let first = render_to_artifacts(
            store.artifacts(),
            source,
            RenderProfile::PatchAligned8On16,
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        let repeated = render_to_artifacts(
            store.artifacts(),
            source,
            RenderProfile::PatchAligned8On16,
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(first, repeated);
        assert_eq!(first.pages.len(), 1);
        assert_eq!(
            (first.pages[0].source.start, first.pages[0].source.end),
            (0, 34)
        );
        assert_eq!(first.pages[0].width, 1568);
        assert_eq!(first.pages[0].height % PATCH_PIXELS as u32, 0);
        assert_eq!(
            store
                .artifacts()
                .resolve(first.pages[0].artifact)
                .unwrap()
                .len(),
            1306
        );
        assert_eq!(
            store.artifacts().resolve(first.source_artifact).unwrap(),
            source.as_bytes()
        );

        drop(store);
        let reopened = Store::open(&state).unwrap();
        let bytes = reopened
            .artifacts()
            .resolve(first.pages[0].artifact)
            .unwrap();
        assert_eq!(Digest::of(&bytes), first.pages[0].digest);
        assert_eq!(
            first.pages[0].digest.to_string(),
            "9134073297a38f99e11b85a394e9e194ea1913907fe03d90d11ccb1044ab0e9f"
        );
        assert_eq!(
            first.renderer.to_string(),
            "e91befc27f09a0d452e7b1921de264c91818eff621d6c53856c6e64f7ebe809e"
        );
    }

    #[test]
    fn corrupted_source_archives_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state")).unwrap();
        let manifest = render_to_artifacts(
            store.artifacts(),
            "archived exact source",
            RenderProfile::PatchAligned8On16,
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        std::fs::write(
            store.artifacts().path(manifest.source_artifact.digest()),
            b"corrupted source",
        )
        .unwrap();
        assert!(matches!(
            store.artifacts().resolve(manifest.source_artifact),
            Err(ArtifactError::Integrity(_))
        ));
    }

    #[test]
    fn repeated_lines_are_a_profile_choice_and_unsupported_text_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state")).unwrap();
        let plain = render_to_artifacts(
            store.artifacts(),
            "alpha\nbeta\n",
            RenderProfile::PatchAligned8On16,
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        let repeated = render_to_artifacts(
            store.artifacts(),
            "alpha\nbeta\n",
            RenderProfile::PatchAligned8On16Repeated,
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(plain.pages[0].digest, repeated.pages[0].digest);
        assert!(repeated.pages[0].height > plain.pages[0].height);
        assert!(
            render_to_artifacts(
                store.artifacts(),
                "unsupported snowman: ☃",
                RenderProfile::PatchAligned8On16,
                limits(),
                || false,
            )
            .unwrap()
            .is_none()
        );
    }
}
