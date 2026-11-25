//! Run the layout figure pass (`ING-15`) over a real PDF and write the crops.
//!
//! ```text
//! cargo run -p kb-extract --example layout_figures -- \
//!     book.pdf http://127.0.0.1:8193 out/ [max_pages]
//! ```
//!
//! For each crop it writes `p{page}-{n}.png` and one line of
//! `figures.jsonl` (page, kind, bbox, caption). It also counts the PDF's image
//! objects on the same pages, the only figures the old path could see.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kb_extract::{HttpLayoutDetector, LayoutFigurePass};
use kb_ports::SeatRelease;
use pdfium_render::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, pdf_path, url, out, rest @ ..] = args.as_slice() else {
        eprintln!("usage: layout_figures <pdf> <layout url> <out dir> [max_pages]");
        std::process::exit(2);
    };
    let max_pages: u32 = rest.first().map_or(1000, |s| s.parse().unwrap());
    let pdf = std::fs::read(pdf_path).unwrap();
    std::fs::create_dir_all(out).unwrap();

    let detector = HttpLayoutDetector::new(url, Duration::from_secs(600)).unwrap();
    let pass = LayoutFigurePass::new(Arc::new(detector), max_pages);
    let started = Instant::now();
    let (figures, report) = pass
        .run(&pdf, &HashSet::new(), &SeatRelease::default())
        .await
        .unwrap();
    let elapsed = started.elapsed();

    let mut lines = String::new();
    for (n, figure) in figures.iter().enumerate() {
        let name = format!("p{:04}-{n}.png", figure.page + 1);
        std::fs::write(format!("{out}/{name}"), &figure.image_bytes).unwrap();
        lines.push_str(
            &serde_json::json!({
                "file": name,
                "page": figure.page + 1,
                "kind": figure.kind,
                "bbox": figure.bbox,
                "caption": figure.caption,
            })
            .to_string(),
        );
        lines.push('\n');
    }
    std::fs::write(format!("{out}/figures.jsonl"), lines).unwrap();

    let pdfium = kb_extract::get_pdfium().unwrap();
    let document = pdfium.load_pdf_from_byte_slice(&pdf, None).unwrap();
    let mut image_objects = 0usize;
    for (index, page) in document.pages().iter().enumerate() {
        if index as u32 >= max_pages {
            break;
        }
        image_objects += page
            .objects()
            .iter()
            .filter(|o| o.object_type() == PdfPageObjectType::Image)
            .count();
    }

    let captioned = figures.iter().filter(|f| f.caption.is_some()).count();
    let charts = figures.iter().filter(|f| f.kind == "chart").count();
    println!(
        "{}",
        serde_json::json!({
            "pagesLooked": report.pages,
            "failedPages": report.failed_pages,
            "skippedPages": report.skipped_pages,
            "repeatedDropped": report.repeated,
            "figures": figures.len(),
            "charts": charts,
            "captioned": captioned,
            "pdfImageObjects": image_objects,
            "seconds": elapsed.as_secs_f64(),
            "secondsPerPage": elapsed.as_secs_f64() / f64::from(report.pages.max(1)),
        })
    );
}
