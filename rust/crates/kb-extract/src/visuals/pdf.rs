//! Image objects of a PDF, through pdfium.

use pdfium_render::prelude::*;

use super::{RawVisual, Skips};
use crate::pdfium::get_pdfium;

/// Every image object, page by page, up to `limit`. The raw image (before
/// the page's transform) is taken: it is the picture the author inserted, at
/// its own resolution. A PDF pdfium cannot open has no images here; the text
/// extractor reports that failure.
pub(crate) fn image_objects(bytes: &[u8], limit: usize, skipped: &mut Skips) -> Vec<RawVisual> {
    let mut out = Vec::new();
    let Ok(pdfium) = get_pdfium() else {
        return out;
    };
    let Ok(document) = pdfium.load_pdf_from_byte_slice(bytes, None) else {
        return out;
    };
    for (index, page) in document.pages().iter().enumerate() {
        for object in page.objects().iter() {
            if out.len() >= limit {
                return out;
            }
            let Some(image_object) = object.as_image_object() else {
                continue;
            };
            let Ok(image) = image_object.get_raw_image() else {
                skipped.undecodable += 1;
                continue;
            };
            let bbox = object.bounds().ok().map(|q| {
                let r = q.to_rect();
                [
                    r.left().value,
                    r.bottom().value,
                    r.right().value,
                    r.top().value,
                ]
            });
            #[allow(clippy::cast_possible_truncation)]
            out.push(RawVisual {
                page: index as u32,
                bytes: Vec::new(),
                decoded: Some(image),
                media_name: None,
                alt_text: None,
                bbox,
            });
        }
    }
    out
}
