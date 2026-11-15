//! `kb-server eval ocr` — read a page set through the OCR router and write it out.
//!
//! The command exists to answer one question with a number: how good is this
//! OCR recipe, and how fast? It takes a list of PDFs, reads **the first page**
//! of each through the same router the ingest pipeline uses (`KB_OCR_MODE`
//! forced to `full`, one page per document), and writes the page's markdown to
//! `<out>/<relative path minus .pdf>.md`.
//!
//! The output layout is the one the olmOCR benchmark expects: a directory tree
//! that mirrors the PDF tree, one `.md` per PDF. That is what makes a run
//! comparable with a reference run — the same PDFs, the same file names, and a
//! scorer that reads both.
//!
//! What it deliberately does not do: it does not use the page cache (a run
//! measures the model, not the cache), and it does not run the ingest path
//! (a run measures OCR, not chunking). What it does keep is the recipe: the
//! same provider, the same postprocessing, the same layout fallback, and the
//! same degradation rules — a page the layout detector could not read is
//! reported, not hidden.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use kb_extract::config::{ExtractConfig, OcrMode, OcrTableFormat};
use kb_extract::ocr_router::{OcrRouter, OcrRouterSettings};
use kb_ports::ExtractionOptions;

use crate::cli::CliError;

/// How many documents are read at the same time unless `--concurrency` says
/// otherwise.
///
/// The router has its own page pool (`KB_OCR_CONCURRENCY`); this is the number
/// of *documents* in flight, so a run of 40 single-page documents does not
/// serialise on one document at a time.
pub const DEFAULT_DOCUMENT_CONCURRENCY: usize = 4;

/// Parsed `eval ocr` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrEvalArgs {
    /// The file listing the PDFs, one relative path per line.
    pub pdfs: PathBuf,
    /// The directory the relative paths are resolved against.
    pub root: PathBuf,
    /// Where the `.md` files are written.
    pub out: PathBuf,
    /// Write tables as HTML (the vendor's rendering) instead of markdown.
    pub html: bool,
    /// How many documents are read at the same time.
    pub concurrency: usize,
}

impl OcrEvalArgs {
    /// Parse `eval ocr` arguments.
    ///
    /// # Errors
    ///
    /// When a flag is missing its value, or a value is not what it must be.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut pdfs: Option<PathBuf> = None;
        let mut root: Option<PathBuf> = None;
        let mut out: Option<PathBuf> = None;
        let mut html = false;
        let mut concurrency = DEFAULT_DOCUMENT_CONCURRENCY;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--pdfs" => {
                    pdfs = Some(PathBuf::from(value_of(args, &mut index, "--pdfs")?));
                }
                "--root" => {
                    root = Some(PathBuf::from(value_of(args, &mut index, "--root")?));
                }
                "--out" => {
                    out = Some(PathBuf::from(value_of(args, &mut index, "--out")?));
                }
                "--html" => {
                    html = true;
                    index += 1;
                }
                "--concurrency" => {
                    let raw = value_of(args, &mut index, "--concurrency")?;
                    concurrency = raw.trim().parse::<usize>().map_err(|_| {
                        CliError::Usage(format!("--concurrency must be a number, got {raw:?}"))
                    })?;
                    if concurrency == 0 {
                        return Err(CliError::Usage(
                            "--concurrency must be at least 1".to_string(),
                        ));
                    }
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown eval ocr argument {other:?}; expected \
                         --pdfs <file> --root <dir> --out <dir> [--html] [--concurrency N]"
                    )));
                }
            }
        }
        Ok(Self {
            pdfs: pdfs
                .ok_or_else(|| CliError::Usage("eval ocr needs --pdfs <file>".to_string()))?,
            root: root.ok_or_else(|| CliError::Usage("eval ocr needs --root <dir>".to_string()))?,
            out: out.ok_or_else(|| CliError::Usage("eval ocr needs --out <dir>".to_string()))?,
            html,
            concurrency,
        })
    }
}

/// The value of a flag: the argument right after it.
fn value_of(args: &[String], index: &mut usize, name: &str) -> Result<String, CliError> {
    let value = args
        .get(*index + 1)
        .ok_or_else(|| CliError::Usage(format!("{name} needs a value")))?;
    if value.starts_with("--") {
        return Err(CliError::Usage(format!("{name} needs a value")));
    }
    *index += 2;
    Ok(value.clone())
}

/// One document's outcome.
#[derive(Debug, Default)]
struct Outcome {
    /// The page was read and written.
    ok: bool,
    /// The document could not be read.
    failed: bool,
    /// The page's markdown, when it was read.
    chars: usize,
    /// The layout detector answered for this page.
    layout: u32,
    /// The layout detector failed for this page.
    layout_failed: u32,
}

/// Run `kb-server eval ocr`.
///
/// # Errors
///
/// When the arguments are wrong, the PDF list cannot be read, or no OCR
/// provider is configured.
pub async fn run(args: &[String]) -> Result<(), CliError> {
    let parsed = OcrEvalArgs::parse(args)?;

    let mut cfg = ExtractConfig::from_env();
    // The run is a measurement of the OCR path: every page goes to the
    // provider, and only the first page of each document does.
    cfg.ocr_mode = OcrMode::Full;
    cfg.ocr_max_pages = 1;
    if parsed.html {
        cfg.ocr_table_format = OcrTableFormat::Html;
    }
    if cfg.ocr_base_url.trim().is_empty() {
        return Err(CliError::Failed(
            "eval ocr needs KB_OCR_BASE_URL: the OCR endpoint to measure".to_string(),
        ));
    }

    let provider = kb_extract::builder::build_ocr_provider(&cfg).ok_or_else(|| {
        CliError::Failed(
            "eval ocr could not build an OCR provider from the configuration".to_string(),
        )
    })?;
    let settings = OcrRouterSettings::from_config(&cfg);
    let router = Arc::new(OcrRouter::new(settings, Some(provider), None, None));

    let documents = read_pdf_list(&parsed.pdfs)?;
    if documents.is_empty() {
        return Err(CliError::Failed(format!(
            "{}: no PDFs listed",
            parsed.pdfs.display()
        )));
    }
    std::fs::create_dir_all(&parsed.out)
        .map_err(|err| CliError::Failed(format!("create {}: {err}", parsed.out.display())))?;

    let total = documents.len();
    println!(
        "eval ocr: {} document(s) from {}, recipe {}, tables {}",
        total,
        parsed.root.display(),
        cfg.ocr_recipe.as_str(),
        cfg.ocr_table_format.as_str(),
    );

    let started = Instant::now();
    let outcomes: Vec<Outcome> = stream::iter(documents)
        .map(|relative| {
            let router = Arc::clone(&router);
            let root = parsed.root.clone();
            let out = parsed.out.clone();
            async move { read_one(&router, &root, &out, &relative).await }
        })
        .buffered(parsed.concurrency)
        .collect()
        .await;
    let elapsed = started.elapsed();

    let ok = outcomes.iter().filter(|o| o.ok).count();
    let failed = outcomes.iter().filter(|o| o.failed).count();
    let chars: usize = outcomes.iter().map(|o| o.chars).sum();
    let layout: u32 = outcomes.iter().map(|o| o.layout).sum();
    let layout_failed: u32 = outcomes.iter().map(|o| o.layout_failed).sum();
    let seconds = elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    println!(
        "eval ocr: {ok} read, {failed} failed, {chars} chars, layout pages {layout}, \
         layout failed {layout_failed}"
    );
    println!(
        "eval ocr: wall {:.1}s | {:.2} pages/s | {:.2} s/page",
        elapsed.as_secs_f64(),
        total as f64 / seconds,
        seconds / total as f64
    );
    if failed > 0 {
        return Err(CliError::Failed(format!(
            "{failed} of {total} document(s) could not be read"
        )));
    }
    Ok(())
}

/// Read the PDF list: one relative path per line, blanks and `#` comments
/// skipped.
fn read_pdf_list(path: &Path) -> Result<Vec<String>, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| CliError::Failed(format!("read {}: {err}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Read one document's first page and write it out.
async fn read_one(router: &OcrRouter, root: &Path, out: &Path, relative: &str) -> Outcome {
    let mut outcome = Outcome::default();
    let path = root.join(relative);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("eval ocr: {}: {err}", relative);
            outcome.failed = true;
            return outcome;
        }
    };
    let options = ExtractionOptions::default();
    let result = match router.extract_pdf(&bytes, relative, &options).await {
        Ok(result) => result,
        Err(err) => {
            eprintln!("eval ocr: {}: {err}", relative);
            outcome.failed = true;
            return outcome;
        }
    };
    if let Some(report) = result.ocr.as_ref() {
        outcome.layout = report.layout_pages;
        outcome.layout_failed = report.layout_failed;
    }
    let text = result.content.trim().to_string();
    if text.is_empty() {
        eprintln!("eval ocr: {relative}: the page came back empty");
        outcome.failed = true;
        return outcome;
    }
    let target = markdown_path(out, relative);
    if let Some(parent) = target.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            eprintln!("eval ocr: {}: {err}", relative);
            outcome.failed = true;
            return outcome;
        }
    }
    if let Err(err) = std::fs::write(&target, &text) {
        eprintln!("eval ocr: {}: {err}", relative);
        outcome.failed = true;
        return outcome;
    }
    outcome.chars = text.chars().count();
    outcome.ok = true;
    outcome
}

/// `<out>/<relative without .pdf>.md`. Not `with_extension`: that replaces
/// everything after the last dot, so `2503.05218_pg5.pdf` became `2503.md`
/// and the pages of one arXiv month overwrote each other.
fn markdown_path(out: &std::path::Path, relative: &str) -> std::path::PathBuf {
    let stem = relative.strip_suffix(".pdf").unwrap_or(relative);
    out.join(format!("{stem}.md"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    #[test]
    fn a_dotted_file_name_keeps_its_whole_stem() {
        let out = std::path::Path::new("/o");
        assert_eq!(
            markdown_path(out, "arxiv_math/2503.05218_pg5.pdf"),
            std::path::PathBuf::from("/o/arxiv_math/2503.05218_pg5.md")
        );
        assert_eq!(
            markdown_path(out, "tables/a.pdf"),
            std::path::PathBuf::from("/o/tables/a.md")
        );
    }

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_three_paths_are_required() {
        for missing in ["--pdfs", "--root", "--out"] {
            let mut all = args(&["--pdfs", "p.txt", "--root", "r", "--out", "o"]);
            let index = all.iter().position(|a| a == missing).expect("flag");
            all.drain(index..index + 2);
            let err = OcrEvalArgs::parse(&all).unwrap_err();
            assert!(matches!(err, CliError::Usage(_)), "{missing}");
        }
    }

    #[test]
    fn the_flags_are_read_in_order() {
        let parsed = OcrEvalArgs::parse(&args(&[
            "--pdfs",
            "list.txt",
            "--root",
            "/data",
            "--out",
            "/out",
            "--html",
            "--concurrency",
            "8",
        ]))
        .expect("parse");
        assert_eq!(parsed.pdfs, PathBuf::from("list.txt"));
        assert_eq!(parsed.root, PathBuf::from("/data"));
        assert_eq!(parsed.out, PathBuf::from("/out"));
        assert!(parsed.html);
        assert_eq!(parsed.concurrency, 8);
    }

    #[test]
    fn a_flag_without_a_value_is_a_usage_error() {
        let err = OcrEvalArgs::parse(&args(&["--pdfs", "list.txt", "--root"])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
        let err = OcrEvalArgs::parse(&args(&["--pdfs", "--root", "r"])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }

    #[test]
    fn a_zero_concurrency_is_refused() {
        let err = OcrEvalArgs::parse(&args(&[
            "--pdfs",
            "l",
            "--root",
            "r",
            "--out",
            "o",
            "--concurrency",
            "0",
        ]))
        .unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }

    #[test]
    fn the_pdf_list_skips_blanks_and_comments() {
        let dir = std::env::temp_dir().join("kb-eval-ocr-list-test");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("list.txt");
        std::fs::write(&path, "a.pdf\n\n# a comment\nb/c.pdf\n").expect("write");
        let list = read_pdf_list(&path).expect("list");
        assert_eq!(list, vec!["a.pdf".to_string(), "b/c.pdf".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
