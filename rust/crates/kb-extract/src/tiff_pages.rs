//! `.tif` / `.tiff`: every page, decoded and re-encoded as PNG for OCR.
//!
//! A TIFF is often a document, not a picture: scanners and fax servers write
//! one multi-page file per document, usually 1-bit CCITT Group 4. Two things
//! follow. Vision endpoints (OpenAI-compatible servers, llama.cpp) accept PNG
//! and JPEG but not TIFF, so each page is converted; and each page is OCR'd on
//! its own and keeps its page number, so a hit on page 7 of a fax cites page 7
//! as a PDF page would.

use std::io::Cursor;

use image::{DynamicImage, GrayAlphaImage, GrayImage, ImageBuffer, RgbImage, RgbaImage};
use kb_ports::ExtractionResult;
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use crate::ExtractError;
use crate::chain::ScannedPageOcr;

/// A bound on pages, so a hostile file with a long IFD chain cannot run OCR
/// for hours. Larger than any real scan batch.
pub const MAX_TIFF_PAGES: usize = 1_000;

/// Whether `ext` (lowercased, with its dot) is a TIFF extension.
#[must_use]
pub fn is_tiff_extension(ext: &str) -> bool {
    ext == ".tif" || ext == ".tiff"
}

/// Every page of a TIFF as PNG bytes, in file order.
pub fn pages_as_png(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ExtractError> {
    let fail =
        |e: &dyn std::fmt::Display| ExtractError::Container(format!("not a readable TIFF: {e}"));
    let mut decoder = Decoder::new(Cursor::new(bytes)).map_err(|e| fail(&e))?;
    let mut pages = Vec::new();
    loop {
        let page = pages.len() + 1;
        let image = decode_page(&mut decoder, bytes)
            .map_err(|e| ExtractError::Container(format!("TIFF page {page}: {e}")))?;
        let mut png = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| ExtractError::Container(format!("TIFF page {page}: PNG encode: {e}")))?;
        pages.push(png);
        if !decoder.more_images() {
            break;
        }
        if pages.len() >= MAX_TIFF_PAGES {
            return Err(ExtractError::Unsupported(format!(
                "the TIFF has more than {MAX_TIFF_PAGES} pages"
            )));
        }
        decoder.next_image().map_err(|e| fail(&e))?;
    }
    Ok(pages)
}

/// TIFF tag values this module reads by number.
const COMPRESSION_FAX3: u16 = 3;
const PHOTOMETRIC_PALETTE: u16 = 3;
const TAG_T4_OPTIONS: u16 = 292;

fn decode_page(decoder: &mut Decoder<Cursor<&[u8]>>, file: &[u8]) -> Result<DynamicImage, String> {
    let (width, height) = decoder.dimensions().map_err(|e| e.to_string())?;
    let compression = decoder
        .get_tag_unsigned::<u16>(Tag::Compression)
        .unwrap_or(1);
    if compression == COMPRESSION_FAX3 {
        return decode_g3_page(decoder, file, width, height);
    }
    if decoder
        .get_tag_unsigned::<u16>(Tag::PhotometricInterpretation)
        .ok()
        == Some(PHOTOMETRIC_PALETTE)
    {
        return Err(
            "palette (indexed-colour) TIFF pages are not supported; rescan as black-and-white, grayscale or colour"
                .to_string(),
        );
    }
    let color = decoder.colortype().map_err(|e| e.to_string())?;
    let data = decoder.read_image().map_err(|e| e.to_string())?;
    to_image(width, height, color, data)
}

/// A CCITT Group 3 (T.4) page: the compression TIFF Class F faxes use, which
/// the `tiff` crate does not decode. One-dimensional (Modified Huffman with
/// EOLs) coding is read with the `fax` crate; each strip is coded on its own.
fn decode_g3_page(
    decoder: &mut Decoder<Cursor<&[u8]>>,
    file: &[u8],
    width: u32,
    height: u32,
) -> Result<DynamicImage, String> {
    let t4_options = decoder
        .get_tag_unsigned::<u32>(Tag::Unknown(TAG_T4_OPTIONS))
        .unwrap_or(0);
    if t4_options & 1 != 0 {
        return Err("two-dimensional Group 3 fax pages are not supported".to_string());
    }
    // FillOrder 2: the bits of each byte are stored least significant first.
    let lsb_first = decoder.get_tag_unsigned::<u16>(Tag::FillOrder).unwrap_or(1) == 2;
    let offsets = decoder
        .get_tag_u64_vec(Tag::StripOffsets)
        .map_err(|e| e.to_string())?;
    let counts = decoder
        .get_tag_u64_vec(Tag::StripByteCounts)
        .map_err(|e| e.to_string())?;
    // A CCITT "white" run is a run of 0 bits; what a 0 bit looks like is the
    // page's photometric interpretation. Class F faxes are WhiteIsZero (0).
    let zero_is_white = decoder
        .get_tag_unsigned::<u16>(Tag::PhotometricInterpretation)
        .unwrap_or(0)
        == 0;
    let (white, black) = if zero_is_white { (255, 0) } else { (0, 255) };
    let line_width = u16::try_from(width).map_err(|_| "the fax page is too wide".to_string())?;
    let (w, h) = (width as usize, height as usize);

    let mut pixels: Vec<u8> = Vec::with_capacity(w * h);
    let mut rows = 0usize;
    for (offset, count) in offsets.iter().zip(&counts) {
        let start = usize::try_from(*offset).map_err(|_| bad_size())?;
        let len = usize::try_from(*count).map_err(|_| bad_size())?;
        let strip = crate::doc::slice(file, start, len)
            .ok_or_else(|| "a fax strip lies outside the file".to_string())?;
        // libtiff ends a strip after its last line, with no EOL; the `fax`
        // decoder only hands a line over once the EOL after it is read, so
        // the last line would be lost. Zero fill bits then a 1 is an EOL.
        let input = strip
            .iter()
            .map(|b| if lsb_first { b.reverse_bits() } else { *b })
            .chain([0x00, 0x01]);
        fax::decoder::decode_g3(input, |transitions| {
            if rows < h {
                pixels.extend(
                    fax::decoder::pels(transitions, line_width).map(|c| match c {
                        fax::Color::White => white,
                        fax::Color::Black => black,
                    }),
                );
                rows += 1;
            }
        });
    }
    if rows == 0 {
        return Err("the Group 3 fax data could not be decoded".to_string());
    }
    // A short last strip leaves the bottom of the page blank.
    pixels.resize(w * h, white);
    Ok(DynamicImage::ImageLuma8(
        GrayImage::from_raw(width, height, pixels).ok_or_else(bad_size)?,
    ))
}

fn bad_size() -> String {
    "pixel data does not match the page size".to_string()
}

/// Pixel data in the decoder's layout → an 8-bit image a vision model reads.
fn to_image(
    width: u32,
    height: u32,
    color: ColorType,
    data: DecodingResult,
) -> Result<DynamicImage, String> {
    let (w, h) = (width as usize, height as usize);
    let image = match (color, data) {
        (ColorType::Gray(bits @ (1 | 2 | 4)), DecodingResult::U8(v)) => {
            let levels = unpack(&v, w, h, bits)?;
            let max = (1u16 << bits) - 1;
            #[allow(clippy::cast_possible_truncation)]
            let gray: Vec<u8> = levels
                .into_iter()
                .map(|l| (u16::from(l) * 255 / max) as u8)
                .collect();
            DynamicImage::ImageLuma8(GrayImage::from_raw(width, height, gray).ok_or_else(bad_size)?)
        }
        (ColorType::Gray(8), DecodingResult::U8(v)) => {
            DynamicImage::ImageLuma8(GrayImage::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::Gray(16), DecodingResult::U16(v)) => {
            DynamicImage::ImageLuma16(ImageBuffer::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::GrayA(8), DecodingResult::U8(v)) => DynamicImage::ImageLumaA8(
            GrayAlphaImage::from_raw(width, height, v).ok_or_else(bad_size)?,
        ),
        (ColorType::RGB(8), DecodingResult::U8(v)) => {
            DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::RGB(16), DecodingResult::U16(v)) => {
            DynamicImage::ImageRgb16(ImageBuffer::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::RGBA(8), DecodingResult::U8(v)) => {
            DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::RGBA(16), DecodingResult::U16(v)) => {
            DynamicImage::ImageRgba16(ImageBuffer::from_raw(width, height, v).ok_or_else(bad_size)?)
        }
        (ColorType::CMYK(8), DecodingResult::U8(v)) => {
            if v.len() != w * h * 4 {
                return Err(bad_size());
            }
            #[allow(clippy::cast_possible_truncation)]
            let rgb: Vec<u8> = v
                .chunks_exact(4)
                .flat_map(|p| {
                    let k = 255 - u16::from(p[3]);
                    [0, 1, 2].map(|i| ((255 - u16::from(p[i])) * k / 255) as u8)
                })
                .collect();
            DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, rgb).ok_or_else(bad_size)?)
        }
        (other, _) => return Err(format!("unsupported TIFF colour type {other:?}")),
    };
    Ok(match image {
        DynamicImage::ImageLuma8(_) | DynamicImage::ImageRgb8(_) => image,
        DynamicImage::ImageLuma16(_) | DynamicImage::ImageLumaA8(_) => {
            DynamicImage::ImageLuma8(image.to_luma8())
        }
        _ => DynamicImage::ImageRgb8(image.to_rgb8()),
    })
}

/// Sub-byte samples, MSB first, each row padded to a whole byte (TIFF's
/// layout), one byte per sample.
fn unpack(packed: &[u8], w: usize, h: usize, bits: u8) -> Result<Vec<u8>, String> {
    let bits = usize::from(bits);
    let row_bytes = (w * bits).div_ceil(8);
    if packed.len() < row_bytes * h {
        return Err(bad_size());
    }
    let mask = (1u16 << bits) - 1;
    let mut out = Vec::with_capacity(w * h);
    for row in packed.chunks_exact(row_bytes).take(h) {
        for x in 0..w {
            let bit = x * bits;
            let shift = 8 - bits - bit % 8;
            #[allow(clippy::cast_possible_truncation)]
            out.push(((u16::from(row[bit / 8]) >> shift) & mask) as u8);
        }
    }
    Ok(out)
}

/// OCR every page of a TIFF through `ocr`, one page per result page.
pub async fn ocr_tiff(
    ocr: &dyn ScannedPageOcr,
    bytes: &[u8],
    filename: &str,
) -> Result<ExtractionResult, ExtractError> {
    let pages = pages_as_png(bytes)?;
    let total = pages.len();
    let mut texts = Vec::with_capacity(total);
    for (index, png) in pages.iter().enumerate() {
        let page = index + 1;
        let result = ocr
            .ocr_image(png, "image/png", filename)
            .await
            .map_err(|e| ExtractError::Container(format!("page {page} of {total}: {e}")))?;
        if let Some(error) = result.error {
            return Err(ExtractError::Container(format!(
                "page {page} of {total}: {error}"
            )));
        }
        texts.push(result.content);
    }
    Ok(crate::page_ocr::paged_result(texts, "image"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tiff::encoder::{TiffEncoder, colortype};

    fn multi_page_tiff() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut encoder = TiffEncoder::new(&mut out).unwrap();
        encoder
            .write_image::<colortype::Gray8>(2, 2, &[0, 255, 255, 0])
            .unwrap();
        encoder
            .write_image::<colortype::RGB8>(1, 1, &[10, 20, 30])
            .unwrap();
        encoder
            .write_image::<colortype::RGBA8>(1, 1, &[1, 2, 3, 255])
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn every_page_becomes_a_png() {
        let pages = pages_as_png(&multi_page_tiff()).unwrap();
        assert_eq!(pages.len(), 3);
        for png in &pages {
            assert!(png.starts_with(b"\x89PNG"), "page is not PNG");
        }
        let first = image::load_from_memory(&pages[0]).unwrap().to_luma8();
        assert_eq!(first.into_raw(), vec![0, 255, 255, 0]);
        let second = image::load_from_memory(&pages[1]).unwrap().to_rgb8();
        assert_eq!(second.into_raw(), vec![10, 20, 30]);
    }

    #[test]
    fn one_bit_rows_are_unpacked_msb_first_with_row_padding() {
        // 10 pixels wide: two bytes per row, the last 6 bits are padding.
        let got = unpack(&[0b1010_0000, 0b0100_0000], 10, 1, 1).unwrap();
        assert_eq!(got, vec![1, 0, 1, 0, 0, 0, 0, 0, 0, 1]);
        let image = to_image(
            10,
            1,
            ColorType::Gray(1),
            DecodingResult::U8(vec![0b1010_0000, 0b0100_0000]),
        )
        .unwrap()
        .to_luma8();
        assert_eq!(image.into_raw()[..3], [255, 0, 255]);
    }

    #[test]
    fn bytes_that_are_not_a_tiff_are_a_reason() {
        let err = pages_as_png(b"not a tiff at all").unwrap_err();
        assert!(err.to_string().contains("TIFF"), "{err}");
    }

    /// Records the mime of every page it is handed.
    struct Recorder(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl ScannedPageOcr for Recorder {
        async fn ocr_scanned_pdf(
            &self,
            _: &[u8],
            _: &str,
        ) -> Result<ExtractionResult, ExtractError> {
            unreachable!("not a PDF")
        }
        async fn ocr_image(
            &self,
            bytes: &[u8],
            mime: &str,
            _: &str,
        ) -> Result<ExtractionResult, ExtractError> {
            let mut seen = self.0.lock().unwrap();
            assert!(bytes.starts_with(b"\x89PNG"));
            seen.push(mime.to_string());
            Ok(ExtractionResult::text(
                format!("page text {}", seen.len()),
                "image",
            ))
        }
    }

    #[tokio::test]
    async fn each_page_is_ocrd_as_png_and_keeps_its_page_number() {
        let ocr = Recorder(Mutex::new(Vec::new()));
        let result = ocr_tiff(&ocr, &multi_page_tiff(), "fax.tif").await.unwrap();
        assert_eq!(*ocr.0.lock().unwrap(), vec!["image/png"; 3]);
        assert_eq!(result.content, "page text 1\n\npage text 2\n\npage text 3");
        let pages: Vec<u32> = result.page_map.unwrap().iter().map(|p| p.page).collect();
        assert_eq!(pages, vec![0, 1, 2]);
        assert!(result.used_ocr);
    }
}
