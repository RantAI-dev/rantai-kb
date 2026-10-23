//! Print what the production extractor makes of a file, page by page.
//!
//!     cargo run -p kb-extract --example extract_dump -- <file> [page (1-based)]
//!
//! Uses the same `build_production_extractor` and the same environment as
//! kb-server, so with no OCR/layout keys set it is the text-layer path a
//! default deployment runs.
use kb_ports::Extractor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: extract_dump <file> [page]")?;
    let page: Option<u32> = args.next().map(|p| p.parse()).transpose()?;
    let bytes = std::fs::read(&path)?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    let mime = kb_extract::ext_to_mime(&kb_extract::extension_of(&name)).unwrap_or("application/octet-stream");
    let cfg = kb_extract::ExtractConfig::from_lookup(|k| std::env::var(k).ok());
    let extractor = kb_extract::build_production_extractor(&cfg)?;
    let out = extractor.extract(&name, mime, &bytes).await?;
    eprintln!(
        "file_type={} used_ocr={} chars={} pages={} figures={} error={:?}",
        out.file_type,
        out.used_ocr,
        out.content.len(),
        out.page_map.as_ref().map_or(0, Vec::len),
        out.figures.len(),
        out.error
    );
    for p in out.page_map.unwrap_or_default() {
        if page.is_none_or(|want| want == p.page + 1) {
            println!("===== page {} =====\n{}", p.page + 1, p.text);
        }
    }
    Ok(())
}
