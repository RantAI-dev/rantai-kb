//! Extract every file given on the command line with the local (no OCR)
//! extractor, writing `<out>/<file name>.txt` and one TSV line per file to
//! stdout: name, status (`ok` or `error`), characters, milliseconds, reason.
//!
//!     cargo run -p kb-extract --example extract_batch -- <out dir> <file>...
//!
//! Used to run the parsers over a corpus (the legacy-format parity check).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out_dir =
        std::path::PathBuf::from(args.next().ok_or("usage: extract_batch <out> <file>...")?);
    std::fs::create_dir_all(&out_dir)?;
    for path in args {
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        let bytes = std::fs::read(&path)?;
        let mime = kb_extract::ext_to_mime(&kb_extract::extension_of(&name)).unwrap_or("");
        let started = std::time::Instant::now();
        let result = std::panic::catch_unwind(|| kb_extract::extract(&name, mime, &bytes));
        let ms = started.elapsed().as_millis();
        let (status, chars, reason) = match result {
            Ok(Ok(r)) => {
                std::fs::write(out_dir.join(format!("{name}.txt")), &r.content)?;
                match r.error {
                    Some(e) => ("error", 0, e),
                    None => ("ok", r.content.chars().count(), String::new()),
                }
            }
            Ok(Err(e)) => ("error", 0, e.to_string()),
            Err(_) => ("PANIC", 0, String::new()),
        };
        println!(
            "{name}\t{status}\t{chars}\t{ms}\t{}",
            reason.replace(['\t', '\n'], " ")
        );
    }
    Ok(())
}
