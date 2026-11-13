//! Decode every page of a TIFF the way OCR receives it, writing
//! `<out>/page-<n>.png` and printing each page's size.
//!
//!     cargo run -p kb-extract --example tiff_dump -- <file.tif> <out dir>

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: tiff_dump <file.tif> <out dir>")?;
    let out = std::path::PathBuf::from(args.next().ok_or("usage: tiff_dump <file.tif> <out dir>")?);
    std::fs::create_dir_all(&out)?;
    let started = std::time::Instant::now();
    let pages = kb_extract::tiff_pages::pages_as_png(&std::fs::read(&path)?)?;
    let ms = started.elapsed().as_millis();
    for (i, png) in pages.iter().enumerate() {
        std::fs::write(out.join(format!("page-{}.png", i + 1)), png)?;
    }
    println!("{path}\tpages={}\tms={ms}", pages.len());
    Ok(())
}
