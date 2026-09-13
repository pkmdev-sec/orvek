//! Deterministic, local bitmap pages for eligible source text.
//!
//! Source text is borrowed. Owned pixels, headers, and PNG output are zeroized;
//! the `png` encoder and its compression dependencies retain internal copies
//! outside this crate's zeroization guarantee.

use png::{BitDepth, ColorType, Compression, Encoder, EncodingError, Filter};
use std::{
    fmt,
    io::{self, Write},
    ops::Range,
};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const WIDTH: usize = 1568;
const MAX_HEIGHT: usize = 1568;
const CELL_WIDTH: usize = 8;
const CELL_HEIGHT: usize = 16;
const FONT_HEIGHT: usize = 13;
const MARGIN: usize = 16;
const HEADER_ROWS: usize = 3;
const COLUMNS: usize = (WIDTH - 2 * MARGIN) / CELL_WIDTH;
const BODY_ROWS: usize = MAX_HEIGHT / CELL_HEIGHT - HEADER_ROWS - 1;
const TAB_COLUMNS: usize = 8;
const ROW_BYTES: usize = WIDTH / 8;
const MAX_LABEL_BYTES: usize = COLUMNS - "SOURCE ".len();
const FONT: &[u8; 95 * FONT_HEIGHT] = include_bytes!("fonts/8x13-ascii.bin");

#[derive(Debug, Clone, Copy)]
pub(crate) struct RenderLimits {
    pub(crate) max_pages: usize,
    /// Maximum combined PNG bytes across all pages.
    pub(crate) max_png_bytes: usize,
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct RenderedPage {
    #[zeroize(skip)]
    pub(crate) source: Range<usize>,
    #[zeroize(skip)]
    pub(crate) width: u32,
    #[zeroize(skip)]
    pub(crate) height: u32,
    pub(crate) png: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for RenderedPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedPage")
            .field("source", &self.source)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("png", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error)]
pub(crate) enum RenderError {
    #[error("unsupported bitmap text in {location} at byte {byte_offset}")]
    UnsupportedText {
        location: &'static str,
        byte_offset: usize,
    },
    #[error("bitmap rendering was cancelled")]
    Cancelled,
    #[error("bitmap rendering exceeded the {limit} limit")]
    LimitExceeded { limit: &'static str },
    #[error("could not encode bitmap page")]
    Encode(#[source] EncodingError),
}

/// Preserve printable ASCII, spaces, tabs, LF, and CRLF without changing source
/// bytes. Each page covers a contiguous byte range, including its line endings.
/// Unsupported characters, cancellation, or limits discard the entire result.
pub(crate) fn render(
    text: &str,
    label: &str,
    limits: RenderLimits,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<RenderedPage>, RenderError> {
    validate(text, label, &cancelled)?;
    let mut pages = Vec::new();
    let mut start = 0;
    let mut png_bytes = 0;

    while start < text.len() {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        if pages.len() == limits.max_pages {
            return Err(RenderError::LimitExceeded {
                limit: "page count",
            });
        }

        let mut end = start;
        let mut rows = 0;
        while end < text.len() && rows < BODY_ROWS {
            if cancelled() {
                return Err(RenderError::Cancelled);
            }
            end = row_end(text.as_bytes(), end);
            rows += 1;
        }

        let height = (HEADER_ROWS + rows + 1) * CELL_HEIGHT;
        let mut pixels = Zeroizing::new(vec![0xff; ROW_BYTES * height]);
        let heading = Zeroizing::new(format!("SOURCE {label}"));
        let locator = Zeroizing::new(format!(
            "PAGE {} | BYTES {start}..{end} | TABS {TAB_COLUMNS}",
            pages.len() + 1
        ));
        paint_row(&mut pixels, heading.as_bytes(), 0);
        paint_row(&mut pixels, locator.as_bytes(), 1);
        let separator = 2 * CELL_HEIGHT + CELL_HEIGHT / 2;
        pixels[separator * ROW_BYTES + MARGIN / 8..separator * ROW_BYTES + (WIDTH - MARGIN) / 8]
            .fill(0);

        let mut offset = start;
        for row in 0..rows {
            if cancelled() {
                return Err(RenderError::Cancelled);
            }
            let next = row_end(text.as_bytes(), offset);
            paint_row(
                &mut pixels,
                &text.as_bytes()[offset..next],
                HEADER_ROWS + row,
            );
            offset = next;
        }
        let png = encode_png(
            &pixels,
            height as u32,
            limits.max_png_bytes - png_bytes,
            &cancelled,
        )?;
        png_bytes += png.len();
        pages.push(RenderedPage {
            source: start..end,
            width: WIDTH as u32,
            height: height as u32,
            png,
        });
        start = end;
    }

    if cancelled() {
        return Err(RenderError::Cancelled);
    }
    Ok(pages)
}

fn validate(text: &str, label: &str, cancelled: &impl Fn() -> bool) -> Result<(), RenderError> {
    if cancelled() {
        return Err(RenderError::Cancelled);
    }
    if label.len() > MAX_LABEL_BYTES {
        return Err(RenderError::LimitExceeded {
            limit: "label width",
        });
    }
    for (byte_offset, byte) in label.bytes().enumerate() {
        if !(b' '..=b'~').contains(&byte) {
            return Err(RenderError::UnsupportedText {
                location: "label",
                byte_offset,
            });
        }
    }
    for (byte_offset, byte) in text.bytes().enumerate() {
        if byte_offset % 1024 == 0 && cancelled() {
            return Err(RenderError::Cancelled);
        }
        match byte {
            b' '..=b'~' | b'\n' | b'\t' => {}
            b'\r' if text.as_bytes().get(byte_offset + 1) == Some(&b'\n') => {}
            _ => {
                return Err(RenderError::UnsupportedText {
                    location: "source",
                    byte_offset,
                });
            }
        }
    }
    Ok(())
}

/// A newline after an exactly full row belongs to that row. Tabs cannot straddle
/// rows because the column count is a multiple of the fixed tab width.
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
    cancelled: &impl Fn() -> bool,
) -> Result<Zeroizing<Vec<u8>>, RenderError> {
    let mut bytes = Zeroizing::new(Vec::new());
    let mut aborted = None;
    let result = (|| {
        let sink = BoundedPng {
            bytes: &mut bytes,
            max_bytes,
            cancelled,
            aborted: &mut aborted,
        };
        let mut encoder = Encoder::new(sink, WIDTH as u32, height);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(BitDepth::One);
        encoder.set_filter(Filter::NoFilter);
        encoder.set_compression(Compression::Balanced);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(pixels)?;
        writer.finish()
    })();
    if let Some(error) = aborted {
        return Err(error);
    }
    result.map_err(RenderError::Encode)?;
    Ok(bytes)
}

struct BoundedPng<'a, F> {
    bytes: &'a mut Zeroizing<Vec<u8>>,
    max_bytes: usize,
    cancelled: &'a F,
    aborted: &'a mut Option<RenderError>,
}

impl<F: Fn() -> bool> Write for BoundedPng<'_, F> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            *self.aborted = Some(RenderError::Cancelled);
            return Err(io::Error::other("bitmap rendering cancelled"));
        }
        if buffer.len() > self.max_bytes - self.bytes.len() {
            *self.aborted = Some(RenderError::LimitExceeded { limit: "PNG bytes" });
            return Err(io::Error::other("bitmap PNG byte limit exceeded"));
        }
        let needed = self.bytes.len() + buffer.len();
        if needed > self.bytes.capacity() {
            let capacity = needed.max(self.bytes.capacity().saturating_mul(2));
            let mut grown = Zeroizing::new(Vec::with_capacity(capacity.min(self.max_bytes)));
            grown.extend_from_slice(self.bytes);
            // Replacing zeroizes the old allocation; Vec's ordinary growth would
            // release its previous allocation without clearing it.
            *self.bytes = grown;
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
    use super::{
        BODY_ROWS, CELL_HEIGHT, COLUMNS, HEADER_ROWS, MARGIN, MAX_HEIGHT, RenderError,
        RenderLimits, RenderedPage, WIDTH, render,
    };
    use png::{BitDepth, ColorType, Decoder, Transformations};
    use std::{cell::Cell, io::Cursor};
    use zeroize::{Zeroize, ZeroizeOnDrop};

    fn limits() -> RenderLimits {
        RenderLimits {
            max_pages: 16,
            max_png_bytes: 1_000_000,
        }
    }

    fn decode(page: &RenderedPage) -> Vec<u8> {
        let mut decoder = Decoder::new(Cursor::new(&*page.png));
        decoder.set_transformations(Transformations::EXPAND);
        let mut reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().bit_depth, BitDepth::One);
        assert_eq!(reader.info().color_type, ColorType::Grayscale);
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let output = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((output.width, output.height), (page.width, page.height));
        assert_eq!(output.bit_depth, BitDepth::Eight);
        assert!(pixels.iter().all(|&pixel| pixel == 0 || pixel == 255));
        pixels
    }

    fn cell(pixels: &[u8], column: usize, row: usize) -> Vec<u8> {
        let x = MARGIN + column * 8;
        let y = (HEADER_ROWS + row) * CELL_HEIGHT;
        (y..y + CELL_HEIGHT)
            .flat_map(|line| {
                pixels[line * WIDTH + x..line * WIDTH + x + 8]
                    .iter()
                    .copied()
            })
            .collect()
    }

    #[test]
    fn paints_the_bundled_font_as_exact_black_and_white_pixels() {
        let pages = render("A", "fixture", limits(), || false).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].source, 0..1);
        assert_eq!(pages[0].width, 1568);
        assert_eq!(pages[0].height, 80);
        let pixels = decode(&pages[0]);
        let expected_rows = [
            "00000000", "00000000", "00000000", "00011000", "00100100", "01000010", "01000010",
            "01000010", "01111110", "01000010", "01000010", "01000010", "00000000", "00000000",
            "00000000", "00000000",
        ];
        let expected: Vec<u8> = expected_rows
            .iter()
            .flat_map(|row| row.bytes().map(|bit| if bit == b'1' { 0 } else { 255 }))
            .collect();
        assert_eq!(cell(&pixels, 0, 0), expected);
        assert!(
            pixels[(HEADER_ROWS - 1) * CELL_HEIGHT * WIDTH..HEADER_ROWS * CELL_HEIGHT * WIDTH]
                .contains(&0)
        );
    }

    #[test]
    fn preserves_indentation_tabs_blank_lines_and_code_punctuation() {
        let text = "  A\t{\r\n\n\tA}\t  \n";
        let pages = render(text, "code", limits(), || false).unwrap();
        assert_eq!(pages[0].source, 0..text.len());
        assert_eq!(
            pages[0].height as usize,
            (HEADER_ROWS + 3 + 1) * CELL_HEIGHT
        );
        let pixels = decode(&pages[0]);
        let reference = render("A{}", "code", limits(), || false).unwrap();
        let reference_pixels = decode(&reference[0]);
        assert_eq!(cell(&pixels, 2, 0), cell(&reference_pixels, 0, 0));
        assert_eq!(cell(&pixels, 8, 0), cell(&reference_pixels, 1, 0));
        assert_eq!(cell(&pixels, 8, 2), cell(&reference_pixels, 0, 0));
        assert_eq!(cell(&pixels, 9, 2), cell(&reference_pixels, 2, 0));
        for column in [0, 1, 3, 4, 5, 6, 7, 9] {
            assert!(cell(&pixels, column, 0).iter().all(|&pixel| pixel == 255));
        }
        let blank_start = (HEADER_ROWS + 1) * CELL_HEIGHT * WIDTH;
        assert!(
            pixels[blank_start..blank_start + CELL_HEIGHT * WIDTH]
                .iter()
                .all(|&pixel| pixel == 255)
        );
    }

    #[test]
    fn wraps_long_lines_and_tabs_without_losing_page_source_bytes() {
        let text = format!("{}\tA\r\n\nB", "x".repeat(COLUMNS * BODY_ROWS - 1));
        let pages = render(&text, "wrapped", limits(), || false).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].height as usize, MAX_HEIGHT);
        assert_eq!(
            pages[1].height as usize,
            (HEADER_ROWS + 3 + 1) * CELL_HEIGHT
        );
        assert_eq!(pages[0].source, 0..COLUMNS * BODY_ROWS);
        assert_eq!(pages[1].source, COLUMNS * BODY_ROWS..text.len());
        let reconstructed: String = pages
            .iter()
            .map(|page| &text[page.source.clone()])
            .collect();
        assert_eq!(reconstructed, text);
        let pixels = decode(&pages[1]);
        let reference = render("A\n\nB", "wrapped", limits(), || false).unwrap();
        let reference_pixels = decode(&reference[0]);
        assert_eq!(cell(&pixels, 0, 0), cell(&reference_pixels, 0, 0));
        assert_eq!(cell(&pixels, 0, 2), cell(&reference_pixels, 0, 2));
    }

    #[test]
    fn exact_width_crlf_stays_on_the_same_row_and_page() {
        let text = format!("{}\r\nA", "x".repeat(COLUMNS * BODY_ROWS));
        let pages = render(&text, "boundary", limits(), || false).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].source, 0..COLUMNS * BODY_ROWS + 2);
        assert_eq!(pages[1].source, COLUMNS * BODY_ROWS + 2..text.len());
        assert_eq!(pages[1].height, 80);
    }

    #[test]
    fn blank_and_space_only_rows_are_not_trimmed() {
        let text = format!("{}\n", " ".repeat(COLUMNS + 1));
        let pages = render(&text, "whitespace", limits(), || false).unwrap();
        assert_eq!(pages[0].source, 0..text.len());
        assert_eq!(pages[0].height, 96);
        let pixels = decode(&pages[0]);
        assert!(
            pixels[HEADER_ROWS * CELL_HEIGHT * WIDTH..]
                .iter()
                .all(|&pixel| pixel == 255)
        );
        let blank_pages = render(&"\n".repeat(BODY_ROWS + 1), "blank", limits(), || false).unwrap();
        assert_eq!(blank_pages.len(), 2);
        assert_eq!(blank_pages[0].source, 0..BODY_ROWS);
        assert_eq!(blank_pages[1].source, BODY_ROWS..BODY_ROWS + 1);
    }

    #[test]
    fn repeats_identical_png_bytes_and_keeps_headers_separate() {
        let text = "fn main() {\n\tprintln!(\"hello\");\n}\n";
        let first = render(text, "source-a", limits(), || false).unwrap();
        let repeated = render(text, "source-a", limits(), || false).unwrap();
        let relabeled = render(text, "source-b", limits(), || false).unwrap();
        assert_eq!(*first[0].png, *repeated[0].png);
        assert_ne!(*first[0].png, *relabeled[0].png);
        let original_pixels = decode(&first[0]);
        let relabeled_pixels = decode(&relabeled[0]);
        let body = HEADER_ROWS * CELL_HEIGHT * WIDTH;
        assert_eq!(original_pixels[body..], relabeled_pixels[body..]);
    }

    #[test]
    fn rejects_unicode_and_controls_without_echoing_source_text() {
        for suffix in ["é", "😀", "\0", "\u{7f}", "\r", "\rX", "\u{1b}[31m"] {
            let text = format!("private-source{suffix}");
            let error = render(&text, "fixture", limits(), || false).unwrap_err();
            assert!(matches!(error, RenderError::UnsupportedText { .. }));
            assert!(!error.to_string().contains("private-source"));
            assert!(!format!("{error:?}").contains("private-source"));
        }
        assert!(matches!(
            render("ascii", "not\nlabel", limits(), || false),
            Err(RenderError::UnsupportedText {
                location: "label",
                ..
            })
        ));
    }

    #[test]
    fn refuses_partial_output_when_page_or_total_byte_limits_are_exceeded() {
        let text = "A\n".repeat(BODY_ROWS + 1);
        let pages = render(&text, "limit", limits(), || false).unwrap();
        let exact_bytes = pages.iter().map(|page| page.png.len()).sum();
        let exact = RenderLimits {
            max_pages: 2,
            max_png_bytes: exact_bytes,
        };
        assert_eq!(render(&text, "limit", exact, || false).unwrap().len(), 2);
        for rejected in [
            RenderLimits {
                max_pages: 0,
                ..exact
            },
            RenderLimits {
                max_pages: 1,
                ..exact
            },
            RenderLimits {
                max_png_bytes: 0,
                ..exact
            },
            RenderLimits {
                max_png_bytes: exact_bytes - 1,
                ..exact
            },
        ] {
            assert!(matches!(
                render(&text, "limit", rejected, || false),
                Err(RenderError::LimitExceeded { .. })
            ));
        }
    }

    #[test]
    fn cancellation_during_rendering_discards_the_result() {
        assert!(matches!(
            render("A", "cancel", limits(), || true),
            Err(RenderError::Cancelled)
        ));
        let polls = Cell::new(0);
        let text = "A\n".repeat(BODY_ROWS * 2);
        let result = render(&text, "cancel", limits(), || {
            polls.set(polls.get() + 1);
            polls.get() > BODY_ROWS * 3
        });
        assert!(matches!(result, Err(RenderError::Cancelled)));
    }

    #[test]
    fn empty_text_needs_no_pages_and_oversized_labels_are_ineligible() {
        assert!(render("", "empty", limits(), || false).unwrap().is_empty());
        assert!(matches!(
            render("A", &"x".repeat(COLUMNS), limits(), || false),
            Err(RenderError::LimitExceeded {
                limit: "label width"
            })
        ));
    }

    #[test]
    fn page_secrets_are_redacted_and_support_explicit_zeroization() {
        fn assert_secret_traits<T: Zeroize + ZeroizeOnDrop>() {}
        assert_secret_traits::<RenderedPage>();
        let mut pages = render("secret-sentinel", "secret-label", limits(), || false).unwrap();
        let debug = format!("{:?}", pages[0]);
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
        pages[0].zeroize();
        assert!(pages[0].png.is_empty());
    }
}
