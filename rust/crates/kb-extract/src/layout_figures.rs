//! Figures cut from rendered PDF pages by the layout detector (`ING-15`).
//!
//! A PDF's figures reach the visual pipeline in two ways that both miss a lot
//! of a textbook:
//!
//! - an OCR or layout provider that returns crops (Mistral, MinerU) only runs
//!   on the pages it is given, and the local Xiaomi-OCR recipe returns none;
//! - the PDF's own image objects (`visuals::pdf`) are raw bitmaps: a vector
//!   diagram (a function graph, a number line, an arrow diagram) has no image
//!   object at all, and a photo's bitmap is taken before the labels and arrows
//!   drawn over it, and before its clipping.
//!
//! This pass does what MinerU and Mistral OCR do: render the page, ask the
//! layout detector where its pictures and charts are, and crop them from the
//! **rendered** page, so the crop shows what a reader sees, vectors and labels
//! included. A `figure_title` region next to a figure gives its caption, read
//! from the page's text layer ("Gambar 1.6 Sidang BPUPK").
//!
//! It runs on every page, whether or not the page went to OCR, and it is off
//! unless `KB_LAYOUT_FIGURES=on` (it renders every page and waits on the
//! detector). A page the detector fails on is listed in the report, never
//! treated as a page without figures.

use std::collections::HashSet;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use kb_ports::{ExtractedFigure, LayoutBox, LayoutDetector, LayoutFigureReport, SeatRelease};
use pdfium_render::prelude::*;

use crate::ExtractError;
use crate::page_render::{PageImageFormat, PageRenderSpec, render_pdf_page_indices_spec};
use crate::pdfium::get_pdfium;

/// The most pages of one PDF the pass looks at, by default.
pub const DEFAULT_MAX_PAGES: u32 = 1000;

/// How the pages are rendered: the resolution and size the detector and the
/// Xiaomi recipe use, as a PNG so a diagram's thin lines survive.
pub const RENDER_SPEC: PageRenderSpec = PageRenderSpec {
    dpi: 150,
    format: PageImageFormat::Png,
    max_edge: 2048,
};

/// Pages rendered and held at a time: a 600-page book at 150 dpi is too much
/// PNG to hold at once.
const WINDOW: usize = 8;

/// Pages in flight at the detector. The sidecar serves one page at a time
/// under a lock; two keeps it busy while the next page is uploaded.
const DETECT_CONCURRENCY: usize = 2;

/// Region labels that are figures.
const FIGURE_LABELS: &[&str] = &["image", "chart"];

/// Region labels that are figure captions.
const CAPTION_LABELS: &[&str] = &["figure_title"];

/// Margin added around a figure, in thousandths of the page: labels written
/// just outside a picture's box still belong to it.
const PAD: f64 = 12.0;

/// The smallest figure side, in thousandths of the page. Smaller regions are
/// bullets, icons and logos.
const MIN_SIDE: f64 = 40.0;

/// The smallest crop side in pixels.
const MIN_PIXELS: u32 = 48;

/// How far from a figure a caption may sit, in thousandths of the page.
const CAPTION_GAP: f64 = 60.0;

/// The longest caption kept.
const MAX_CAPTION_CHARS: usize = 300;

/// The layout figure pass: a detector and a page cap.
#[derive(Clone)]
pub struct LayoutFigurePass {
    detector: Arc<dyn LayoutDetector>,
    max_pages: u32,
}

impl std::fmt::Debug for LayoutFigurePass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutFigurePass")
            .field("max_pages", &self.max_pages)
            .finish_non_exhaustive()
    }
}

/// A figure region and the caption region next to it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct FigureRegion {
    /// `image` or `chart`.
    pub label: String,
    /// The padded box, 0–1000.
    pub bbox: [f64; 4],
    /// The caption's box, 0–1000.
    pub caption_box: Option<[f64; 4]>,
}

impl LayoutFigurePass {
    /// A pass over at most `max_pages` pages of each PDF.
    #[must_use]
    pub fn new(detector: Arc<dyn LayoutDetector>, max_pages: u32) -> Self {
        Self {
            detector,
            max_pages: max_pages.max(1),
        }
    }

    /// Cut the figures out of a PDF.
    ///
    /// `skip_pages` are pages another extractor already returned figures for:
    /// they are not cropped twice.
    ///
    /// # Errors
    ///
    /// When the PDF cannot be opened. A page that cannot be rendered or that
    /// the detector fails on is listed in the report instead.
    pub async fn run(
        &self,
        pdf_bytes: &[u8],
        skip_pages: &HashSet<u32>,
        seat: &SeatRelease,
    ) -> Result<(Vec<ExtractedFigure>, LayoutFigureReport), ExtractError> {
        let bytes: Arc<Vec<u8>> = Arc::new(pdf_bytes.to_vec());
        let counted = Arc::clone(&bytes);
        let page_count = tokio::task::spawn_blocking(move || {
            let pdfium = get_pdfium()?;
            let document = pdfium
                .load_pdf_from_byte_slice(&counted, None)
                .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;
            Ok::<u32, ExtractError>(u32::try_from(document.pages().len()).unwrap_or(0))
        })
        .await
        .map_err(|e| ExtractError::Container(format!("layout figure task: {e}")))??;

        let mut report = LayoutFigureReport {
            skipped_pages: page_count.saturating_sub(self.max_pages),
            ..LayoutFigureReport::default()
        };
        let pages: Vec<u32> = (0..page_count.min(self.max_pages))
            .filter(|page| !skip_pages.contains(page))
            .collect();
        if pages.is_empty() {
            return Ok((Vec::new(), report));
        }

        // What is left is mostly waiting on the detector: give the worker's
        // extract seat back, as the OCR router does (ING-6).
        seat.release();

        let mut figures = Vec::new();
        for window in pages.chunks(WINDOW) {
            let rendered_from = Arc::clone(&bytes);
            let indices = window.to_vec();
            let images = tokio::task::spawn_blocking(move || {
                render_pdf_page_indices_spec(&rendered_from, &indices, RENDER_SPEC)
            })
            .await
            .map_err(|e| ExtractError::Container(format!("layout figure render task: {e}")))??;

            let detector = Arc::clone(&self.detector);
            let answers: Vec<(u32, Vec<u8>, Option<Vec<FigureRegion>>)> =
                stream::iter(images.into_iter().map(|image| {
                    let detector = Arc::clone(&detector);
                    async move {
                        if image.bytes.is_empty() {
                            return (image.page, image.bytes, None);
                        }
                        match detector.detect(&image.bytes, &image.mime).await {
                            Ok(layout) => {
                                let regions = figure_regions(&layout.regions);
                                (image.page, image.bytes, Some(regions))
                            }
                            Err(err) => {
                                tracing::warn!(
                                    page = image.page + 1,
                                    error = %err,
                                    "layout figure detection failed"
                                );
                                (image.page, image.bytes, None)
                            }
                        }
                    }
                }))
                .buffered(DETECT_CONCURRENCY)
                .collect()
                .await;

            let mut found = Vec::new();
            for (page, png, regions) in answers {
                match regions {
                    None => report.failed_pages.push(page + 1),
                    Some(regions) => {
                        report.pages += 1;
                        if !regions.is_empty() {
                            found.push((page, png, regions));
                        }
                    }
                }
            }
            if found.is_empty() {
                continue;
            }

            let source = Arc::clone(&bytes);
            let cut = tokio::task::spawn_blocking(move || cut_figures(&source, found))
                .await
                .map_err(|e| ExtractError::Container(format!("layout figure crop task: {e}")))?;
            figures.extend(cut);
        }

        let before = figures.len();
        let figures = tokio::task::spawn_blocking(move || drop_repeated(figures))
            .await
            .map_err(|e| ExtractError::Container(format!("layout figure dedupe task: {e}")))?;
        report.repeated = u32::try_from(before - figures.len()).unwrap_or(u32::MAX);
        report.figures = u32::try_from(figures.len()).unwrap_or(u32::MAX);
        Ok((figures, report))
    }
}

/// How many pages the same picture may appear on before it is page furniture.
const FURNITURE_PAGES: usize = 3;

/// How many of the 64 hash bits two crops may differ in and still be the same
/// picture.
const SAME_PICTURE_BITS: u32 = 6;

/// A 64-bit difference hash of a picture: its 9 × 8 grey thumbnail, one bit
/// per horizontal neighbour pair. Rendering the same icon at another place of
/// another page changes a few pixels, not the hash.
#[must_use]
pub fn picture_hash(png: &[u8]) -> Option<u64> {
    let grey = image::load_from_memory(png)
        .ok()?
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if grey.get_pixel(x, y)[0] < grey.get_pixel(x + 1, y)[0] {
                hash |= 1;
            }
        }
    }
    Some(hash)
}

/// Drop the pictures a book repeats on many pages: section icons, chapter
/// banners, logos. A picture whose look appears on [`FURNITURE_PAGES`] or more
/// pages of one document is the page's furniture, not its content, and
/// describing it once per page would cost a vision call each time.
#[must_use]
pub fn drop_repeated(figures: Vec<ExtractedFigure>) -> Vec<ExtractedFigure> {
    let hashes: Vec<Option<u64>> = figures
        .iter()
        .map(|f| picture_hash(&f.image_bytes))
        .collect();
    let furniture: Vec<bool> = hashes
        .iter()
        .map(|hash| {
            let Some(hash) = hash else { return false };
            let pages: HashSet<u32> = hashes
                .iter()
                .zip(&figures)
                .filter(|(other, _)| {
                    other.is_some_and(|other| (other ^ hash).count_ones() <= SAME_PICTURE_BITS)
                })
                .map(|(_, figure)| figure.page)
                .collect();
            pages.len() >= FURNITURE_PAGES
        })
        .collect();
    figures
        .into_iter()
        .zip(furniture)
        .filter_map(|(figure, furniture)| (!furniture).then_some(figure))
        .collect()
}

/// The figures of one page's layout, each with its caption box.
///
/// A region is a figure when its label is `image` or `chart` and it is not a
/// sliver. Its box is padded by [`PAD`] so labels written just outside the
/// picture are kept. A `figure_title` region that overlaps the figure
/// horizontally and sits within [`CAPTION_GAP`] above or below it is its
/// caption; the nearest one wins.
#[must_use]
pub fn figure_regions(regions: &[LayoutBox]) -> Vec<FigureRegion> {
    let captions: Vec<[f64; 4]> = regions
        .iter()
        .filter(|r| CAPTION_LABELS.contains(&r.label.as_str()))
        .map(|r| r.bbox_2d)
        .collect();
    regions
        .iter()
        .filter(|r| FIGURE_LABELS.contains(&r.label.as_str()))
        .filter(|r| {
            let [x1, y1, x2, y2] = r.bbox_2d;
            x2 - x1 >= MIN_SIDE && y2 - y1 >= MIN_SIDE
        })
        .map(|r| FigureRegion {
            label: r.label.clone(),
            bbox: pad(r.bbox_2d),
            caption_box: nearest_caption(r.bbox_2d, &captions),
        })
        .collect()
}

fn pad(b: [f64; 4]) -> [f64; 4] {
    [
        (b[0] - PAD).max(0.0),
        (b[1] - PAD).max(0.0),
        (b[2] + PAD).min(1000.0),
        (b[3] + PAD).min(1000.0),
    ]
}

fn nearest_caption(figure: [f64; 4], captions: &[[f64; 4]]) -> Option<[f64; 4]> {
    let [fx1, fy1, fx2, fy2] = figure;
    captions
        .iter()
        .filter_map(|c| {
            let [cx1, cy1, cx2, cy2] = *c;
            let overlap = fx2.min(cx2) - fx1.max(cx1);
            let narrower = (fx2 - fx1).min(cx2 - cx1);
            if narrower <= 0.0 || overlap < 0.3 * narrower {
                return None;
            }
            let below = cy1 - fy2;
            let above = fy1 - cy2;
            let gap = if (-10.0..=CAPTION_GAP).contains(&below) {
                below.max(0.0)
            } else if (-10.0..=CAPTION_GAP).contains(&above) {
                above.max(0.0)
            } else {
                return None;
            };
            Some((gap, *c))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c)
}

/// Pixel rectangle `(x, y, width, height)` of a 0–1000 box in a
/// `width × height` image, or `None` when it is too small to be a figure.
#[must_use]
pub fn pixel_rect(bbox: [f64; 4], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let to_px =
        |v: f64, size: u32| -> u32 { ((v / 1000.0) * f64::from(size)).round().max(0.0) as u32 };
    let x1 = to_px(bbox[0], width).min(width);
    let y1 = to_px(bbox[1], height).min(height);
    let x2 = to_px(bbox[2], width).min(width);
    let y2 = to_px(bbox[3], height).min(height);
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    let (w, h) = (x2 - x1, y2 - y1);
    (w >= MIN_PIXELS && h >= MIN_PIXELS).then_some((x1, y1, w, h))
}

/// A caption as stored: one line, trimmed, bounded.
#[must_use]
pub fn clean_caption(raw: &str) -> Option<String> {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    Some(joined.chars().take(MAX_CAPTION_CHARS).collect())
}

/// The figure kind the visual pipeline understands.
fn kind_of(label: &str) -> &'static str {
    if label == "chart" { "chart" } else { "image" }
}

/// Crop the figures of the found pages and read their captions.
///
/// Runs on a blocking thread: it decodes PNGs and reads the PDF's text layer.
fn cut_figures(
    pdf_bytes: &[u8],
    found: Vec<(u32, Vec<u8>, Vec<FigureRegion>)>,
) -> Vec<ExtractedFigure> {
    let document = get_pdfium().ok().and_then(|pdfium| {
        pdfium
            .load_pdf_from_byte_slice(pdf_bytes, None)
            .map_err(|e| tracing::warn!(error = %e, "captions: PDF not reopened"))
            .ok()
    });
    let mut out = Vec::new();
    for (page, png, regions) in found {
        let image = match image::load_from_memory(&png) {
            Ok(image) => image,
            Err(e) => {
                tracing::warn!(page = page + 1, error = %e, "layout figure: page image not decoded");
                continue;
            }
        };
        let (width, height) = (image.width(), image.height());
        let text_page = document.as_ref().and_then(|doc| {
            #[allow(clippy::cast_possible_truncation)]
            doc.pages().get(page as _).ok()
        });
        for (index, region) in regions.iter().enumerate() {
            let Some((x, y, w, h)) = pixel_rect(region.bbox, width, height) else {
                continue;
            };
            let crop = image.crop_imm(x, y, w, h);
            let mut bytes = Vec::new();
            if crop
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .is_err()
            {
                continue;
            }
            let caption = match (&text_page, region.caption_box) {
                (Some(page), Some(caption_box)) => caption_text(page, caption_box),
                _ => None,
            };
            out.push(ExtractedFigure {
                kind: kind_of(&region.label).to_string(),
                page,
                #[allow(clippy::cast_possible_truncation)]
                bbox: region.bbox.map(|v| (v / 1000.0) as f32),
                caption,
                image_bytes: bytes,
                id: Some(format!("p{page}-layout{index}")),
            });
        }
    }
    out
}

/// The text layer inside a 0–1000 box of a page, as a caption.
fn caption_text(page: &PdfPage<'_>, caption_box: [f64; 4]) -> Option<String> {
    let text = page.text().ok()?;
    let (width, height) = (page.width().value, page.height().value);
    #[allow(clippy::cast_possible_truncation)]
    let at = |v: f64, size: f32| (v / 1000.0) as f32 * size;
    // The detector's box has its origin at the top left; a PDF's at the
    // bottom left.
    let rect = PdfRect::new_from_values(
        height - at(caption_box[3], height),
        at(caption_box[0], width),
        height - at(caption_box[1], height),
        at(caption_box[2], width),
    );
    clean_caption(&text.inside_rect(rect))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn region(label: &str, bbox: [f64; 4]) -> LayoutBox {
        LayoutBox {
            label: label.to_string(),
            score: 0.9,
            bbox_2d: bbox,
            task_type: "skip".to_string(),
        }
    }

    #[test]
    fn images_and_charts_are_figures_and_text_is_not() {
        let regions = figure_regions(&[
            region("text", [100.0, 100.0, 900.0, 200.0]),
            region("image", [100.0, 250.0, 600.0, 600.0]),
            region("chart", [100.0, 650.0, 900.0, 900.0]),
            region("table", [100.0, 50.0, 900.0, 90.0]),
        ]);
        let labels: Vec<&str> = regions.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["image", "chart"]);
    }

    #[test]
    fn a_sliver_or_an_icon_is_not_a_figure() {
        assert!(figure_regions(&[region("image", [100.0, 100.0, 120.0, 400.0])]).is_empty());
        assert!(figure_regions(&[region("image", [10.0, 10.0, 30.0, 30.0])]).is_empty());
    }

    #[test]
    fn the_box_is_padded_and_stays_on_the_page() {
        let regions = figure_regions(&[region("image", [5.0, 100.0, 995.0, 500.0])]);
        assert_eq!(regions[0].bbox, [0.0, 88.0, 1000.0, 512.0]);
    }

    #[test]
    fn the_nearest_caption_below_or_above_is_paired() {
        let regions = figure_regions(&[
            region("image", [100.0, 200.0, 700.0, 500.0]),
            region("figure_title", [120.0, 510.0, 680.0, 530.0]),
            region("figure_title", [120.0, 560.0, 680.0, 580.0]),
            region("figure_title", [750.0, 510.0, 950.0, 530.0]),
        ]);
        assert_eq!(regions[0].caption_box, Some([120.0, 510.0, 680.0, 530.0]));

        let above = figure_regions(&[
            region("figure_title", [120.0, 160.0, 680.0, 185.0]),
            region("image", [100.0, 200.0, 700.0, 500.0]),
        ]);
        assert_eq!(above[0].caption_box, Some([120.0, 160.0, 680.0, 185.0]));

        let far = figure_regions(&[
            region("image", [100.0, 200.0, 700.0, 500.0]),
            region("figure_title", [120.0, 700.0, 680.0, 720.0]),
        ]);
        assert_eq!(
            far[0].caption_box, None,
            "a caption far below is another figure's"
        );
    }

    #[test]
    fn pixel_rect_scales_and_drops_tiny_crops() {
        assert_eq!(
            pixel_rect([100.0, 200.0, 600.0, 700.0], 1000, 2000),
            Some((100, 400, 500, 1000))
        );
        assert_eq!(pixel_rect([0.0, 0.0, 10.0, 10.0], 1000, 1000), None);
        assert_eq!(pixel_rect([500.0, 500.0, 400.0, 900.0], 1000, 1000), None);
    }

    fn png(draw: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        let image = image::GrayImage::from_fn(900, 800, |x, y| image::Luma([draw(x, y)]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn figure(page: u32, bytes: Vec<u8>) -> ExtractedFigure {
        ExtractedFigure {
            kind: "image".to_string(),
            page,
            bbox: [0.0, 0.0, 1.0, 1.0],
            caption: None,
            image_bytes: bytes,
            id: None,
        }
    }

    #[test]
    fn a_picture_on_three_pages_is_furniture_and_dropped() {
        let icon = || png(|x, _| if x < 450 { 20 } else { 230 });
        // The same icon, a pixel off, as a render at another place gives.
        let shifted = png(|x, _| if x < 452 { 20 } else { 230 });
        let diagram = png(|x, y| u8::try_from((x / 4 + y / 3) % 255).unwrap());
        let kept = drop_repeated(vec![
            figure(1, icon()),
            figure(4, diagram.clone()),
            figure(7, icon()),
            figure(9, shifted),
        ]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].page, 4);

        let twice = drop_repeated(vec![
            figure(1, icon()),
            figure(2, icon()),
            figure(3, diagram),
        ]);
        assert_eq!(twice.len(), 3, "a picture on two pages is still content");
    }

    #[test]
    fn captions_are_one_bounded_line() {
        assert_eq!(
            clean_caption("  Gambar 1.6\n Sidang   BPUPK "),
            Some("Gambar 1.6 Sidang BPUPK".to_string())
        );
        assert_eq!(clean_caption(" \n "), None);
        assert_eq!(
            clean_caption(&"x".repeat(1000)).unwrap().chars().count(),
            300
        );
    }
}
