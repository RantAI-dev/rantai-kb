//! [`VisualCollector`] over local decoders: every picture a reader of a file
//! can see, normalised for a vision model (`ING-5`).
//!
//! | Format | Where the pictures are | Page | Alt text |
//! |---|---|---|---|
//! | `.png .jpg .jpeg .gif .webp .bmp` | the file itself | 0 | — |
//! | `.tif .tiff` | every page of the file | its page | — |
//! | `.docx` | `a:blip` / `v:imagedata` in `word/document.xml` | 0 | `wp:docPr` `descr`, else `title` |
//! | `.pptx` | `a:blip` on each `ppt/slides/slideN.xml` | slide − 1 | `p:cNvPr` `descr`, else `title` |
//! | `.xlsx` | `a:blip` in the drawing each sheet references | sheet index | `xdr:cNvPr` `descr`, else `title` |
//! | `.odt .ods` | `draw:image` in `content.xml` | 0 / table index | `svg:title`, `svg:desc` |
//! | `.epub` | `<img>` in the spine documents | 0 | `alt` |
//! | `.pdf` | image objects, only with [`VisualOptions::pdf_images`] | its page | — |
//!
//! Headers and footers of Office files are not read: a picture there is a
//! letterhead logo on every page. Pictures in a format with no decoder here
//! (EMF, WMF, SVG, HEIC) are counted as unsupported, and bytes that do not
//! decode as undecodable; neither stops the rest of the file.
//!
//! Normalising decodes the picture (the first frame of a GIF), flattens any
//! transparency onto white, shrinks it so its longest side is at most
//! [`MAX_SIDE`] pixels (never enlarging it) and encodes it as JPEG. The
//! reported width and height are the original's, so a filter downstream sees
//! the icon the author placed, not the bytes sent to a model.

mod office;
mod pdf;

use std::io::Cursor;

use async_trait::async_trait;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageReader, Limits, RgbImage};
use kb_ports::{
    NormalizedImage, PortError, Visual, VisualCollector, VisualOptions, VisualOrigin, VisualScan,
    VisualScanSkips,
};

use crate::detect::extension_of;
use crate::tiff_pages;

/// Longest side of a normalised picture, in pixels. The size the hosted vision
/// models resample to anyway; more pixels only cost upload time.
pub const MAX_SIDE: u32 = 1568;

/// JPEG quality of a normalised picture.
pub const JPEG_QUALITY: u8 = 85;

/// Largest picture decoded, per side. A hostile file declaring a 100k × 100k
/// image is refused before any allocation.
const MAX_DECODE_SIDE: u32 = 20_000;

/// Picture formats no decoder here reads, by media file extension.
const UNSUPPORTED_MEDIA: &[&str] = &[
    ".emf", ".wmf", ".svg", ".svgz", ".heic", ".heif", ".wdp", ".jxr", ".eps", ".pict", ".emz",
    ".wmz",
];

/// Collects and normalises pictures with pure-Rust decoders (and pdfium for
/// PDF image objects).
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalVisualCollector;

/// A picture as found, before normalisation.
pub(crate) struct RawVisual {
    pub page: u32,
    pub bytes: Vec<u8>,
    /// Already decoded pixels (PDF image objects), used instead of `bytes`.
    pub decoded: Option<DynamicImage>,
    /// Name of the media part (`word/media/image1.emf`), to recognise a format
    /// with no decoder without trying to decode it.
    pub media_name: Option<String>,
    pub alt_text: Option<String>,
    pub bbox: Option<[f32; 4]>,
}

/// Decode, flatten, shrink and re-encode one picture.
///
/// `Ok(None)` when the bytes are not a picture this decodes.
#[must_use]
pub fn normalize_image(bytes: &[u8]) -> Option<NormalizedImage> {
    let image = decode(bytes)?;
    Some(normalize_decoded(&image))
}

fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    reader.limits(limits);
    reader.decode().ok()
}

/// Normalise an already decoded picture.
pub(crate) fn normalize_decoded(image: &DynamicImage) -> NormalizedImage {
    let (width, height) = (image.width(), image.height());
    let rgb = flatten_on_white(image);
    let rgb = shrink(rgb);
    let mut bytes = Vec::new();
    // Encoding into a Vec cannot fail for an RGB8 buffer of a valid size.
    let _ = JpegEncoder::new_with_quality(&mut Cursor::new(&mut bytes), JPEG_QUALITY)
        .encode_image(&rgb);
    NormalizedImage {
        mime: "image/jpeg".to_string(),
        bytes,
        width,
        height,
    }
}

fn flatten_on_white(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u16::from(p[3]);
        #[allow(clippy::cast_possible_truncation)]
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])]));
    }
    out
}

fn shrink(rgb: RgbImage) -> RgbImage {
    let (w, h) = rgb.dimensions();
    let longest = w.max(h);
    if longest <= MAX_SIDE {
        return rgb;
    }
    let scale = f64::from(MAX_SIDE) / f64::from(longest);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (nw, nh) = (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    );
    image::imageops::resize(&rgb, nw, nh, image::imageops::FilterType::Triangle)
}

/// Standalone image extensions this collector reads as the file itself.
const IMAGE_FILES: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"];

/// The synchronous body of [`LocalVisualCollector::collect`].
pub fn collect_visuals(filename: &str, bytes: &[u8], options: VisualOptions) -> VisualScan {
    let ext = extension_of(filename);
    let mut scan = VisualScan::default();
    let limit = if options.max_visuals == 0 {
        usize::MAX
    } else {
        options.max_visuals as usize
    };
    match ext.as_str() {
        e if IMAGE_FILES.contains(&e) => match normalize_image(bytes) {
            Some(n) => scan.visuals.push(upload_visual(0, n)),
            None => scan.skipped.undecodable += 1,
        },
        e if tiff_pages::is_tiff_extension(e) => match tiff_pages::pages_as_png(bytes) {
            Ok(pages) => {
                for (index, png) in pages.iter().enumerate().take(limit) {
                    match normalize_image(png) {
                        #[allow(clippy::cast_possible_truncation)]
                        Some(n) => scan.visuals.push(upload_visual(index as u32, n)),
                        None => scan.skipped.undecodable += 1,
                    }
                }
            }
            Err(_) => scan.skipped.undecodable += 1,
        },
        ".heic" | ".heif" => scan.skipped.unsupported_format += 1,
        ".pdf" if options.pdf_images => {
            let raw = pdf::image_objects(bytes, limit, &mut scan.skipped);
            push_embedded(&mut scan, raw, limit);
        }
        ".docx" | ".pptx" | ".xlsx" | ".odt" | ".ods" | ".epub" => {
            let raw = office::pictures(&ext, bytes);
            push_embedded(&mut scan, raw, limit);
        }
        _ => {}
    }
    scan
}

fn upload_visual(page: u32, n: NormalizedImage) -> Visual {
    Visual {
        origin: VisualOrigin::Upload,
        page,
        mime: n.mime,
        bytes: n.bytes,
        width: n.width,
        height: n.height,
        alt_text: None,
        caption: None,
        bbox: None,
        figure_id: None,
        kind_hint: None,
    }
}

/// Alt text worth keeping: not empty and not just a picture's file name, which
/// is what python-pptx and several exporters write when the author gave none.
fn meaningful_alt(alt: &str) -> Option<String> {
    let alt = alt.trim();
    if alt.is_empty() {
        return None;
    }
    let is_file_name = !alt.contains(' ')
        && matches!(
            extension_of(alt).as_str(),
            ".png"
                | ".jpg"
                | ".jpeg"
                | ".gif"
                | ".bmp"
                | ".tif"
                | ".tiff"
                | ".emf"
                | ".wmf"
                | ".svg"
                | ".webp"
        );
    (!is_file_name).then(|| alt.to_string())
}

fn push_embedded(scan: &mut VisualScan, raw: Vec<RawVisual>, limit: usize) {
    for r in raw {
        if scan.visuals.len() >= limit {
            break;
        }
        if r.media_name
            .as_deref()
            .is_some_and(|name| UNSUPPORTED_MEDIA.contains(&extension_of(name).as_str()))
        {
            scan.skipped.unsupported_format += 1;
            continue;
        }
        let normalized = match &r.decoded {
            Some(image) => Some(normalize_decoded(image)),
            None => normalize_image(&r.bytes),
        };
        let Some(n) = normalized else {
            scan.skipped.undecodable += 1;
            continue;
        };
        scan.visuals.push(Visual {
            origin: VisualOrigin::Embedded,
            page: r.page,
            mime: n.mime,
            bytes: n.bytes,
            width: n.width,
            height: n.height,
            alt_text: r.alt_text.and_then(|a| meaningful_alt(&a)),
            caption: None,
            bbox: r.bbox,
            figure_id: None,
            kind_hint: None,
        });
    }
}

#[async_trait]
impl VisualCollector for LocalVisualCollector {
    async fn collect(
        &self,
        filename: &str,
        _mime_type: &str,
        bytes: &[u8],
        options: VisualOptions,
    ) -> Result<VisualScan, PortError> {
        let name = filename.to_string();
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || collect_visuals(&name, &data, options))
            .await
            .map_err(|e| PortError::Unavailable(format!("visual collect task: {e}")))
    }

    async fn normalize(&self, bytes: &[u8]) -> Result<Option<NormalizedImage>, PortError> {
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || normalize_image(&data))
            .await
            .map_err(|e| PortError::Unavailable(format!("normalize task: {e}")))
    }
}

/// Re-exported for the scan accounting of the format walkers.
pub(crate) type Skips = VisualScanSkips;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    pub(crate) fn png(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, Rgba(color));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn a_large_picture_shrinks_to_the_longest_side_and_keeps_its_original_size() {
        let n = normalize_image(&png(3136, 1000, [10, 20, 30, 255])).unwrap();
        assert_eq!((n.width, n.height), (3136, 1000));
        assert_eq!(n.mime, "image/jpeg");
        let out = image::load_from_memory(&n.bytes).unwrap();
        assert_eq!(out.width(), MAX_SIDE);
        assert_eq!(out.height(), 500);
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        let n = normalize_image(&png(40, 30, [0, 0, 0, 255])).unwrap();
        let out = image::load_from_memory(&n.bytes).unwrap();
        assert_eq!((out.width(), out.height()), (40, 30));
    }

    #[test]
    fn transparency_is_flattened_onto_white() {
        let n = normalize_image(&png(8, 8, [0, 0, 0, 0])).unwrap();
        let out = image::load_from_memory(&n.bytes).unwrap().to_rgb8();
        assert!(out.pixels().all(|p| p[0] > 245 && p[1] > 245 && p[2] > 245));
    }

    #[test]
    fn gif_webp_and_bmp_decode() {
        let src = DynamicImage::ImageRgba8(RgbaImage::from_pixel(5, 4, Rgba([200, 0, 0, 255])));
        for format in [ImageFormat::Gif, ImageFormat::WebP, ImageFormat::Bmp] {
            let mut bytes = Vec::new();
            src.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
            let n = normalize_image(&bytes).unwrap_or_else(|| panic!("{format:?}"));
            assert_eq!((n.width, n.height), (5, 4), "{format:?}");
        }
    }

    #[test]
    fn a_file_name_is_not_alt_text() {
        assert_eq!(meaningful_alt(" image.png "), None);
        assert_eq!(meaningful_alt("Picture1.JPG"), None);
        assert_eq!(meaningful_alt(""), None);
        assert_eq!(
            meaningful_alt("Floor plan.png of level 2").as_deref(),
            Some("Floor plan.png of level 2")
        );
        assert_eq!(
            meaningful_alt("A red bicycle").as_deref(),
            Some("A red bicycle")
        );
    }

    #[test]
    fn garbage_is_not_a_picture() {
        assert!(normalize_image(b"not an image").is_none());
    }

    #[test]
    fn a_standalone_png_is_one_upload_visual_and_heic_is_counted() {
        let scan = collect_visuals(
            "photo.PNG",
            &png(10, 10, [1, 2, 3, 255]),
            VisualOptions::default(),
        );
        assert_eq!(scan.visuals.len(), 1);
        assert_eq!(scan.visuals[0].origin, VisualOrigin::Upload);
        let heic = collect_visuals("phone.heic", b"....ftypheic", VisualOptions::default());
        assert!(heic.visuals.is_empty());
        assert_eq!(heic.skipped.unsupported_format, 1);
        let text = collect_visuals("notes.md", b"# hi", VisualOptions::default());
        assert_eq!(text, VisualScan::default());
    }

    #[test]
    fn a_pdf_is_only_scanned_when_asked() {
        let scan = collect_visuals(
            "a.pdf",
            crate::pdfium::MINIMAL_PDF_HELLO_WORLD,
            VisualOptions::default(),
        );
        assert_eq!(scan, VisualScan::default());
    }
}
