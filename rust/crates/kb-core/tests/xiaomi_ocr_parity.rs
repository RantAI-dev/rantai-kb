//! Golden parity with the vendor's Xiaomi-OCR-0 postprocessing.
//!
//! The port in `kb-core` has to agree with the Python it was ported from, byte
//! for byte: a page that the vendor renders one way and the port renders
//! another way is a page that indexes differently. The golden files under
//! `tests/fixtures/xiaomi_ocr` are the vendor's own output over real model
//! answers, produced by `rust/scripts/xiaomi-ocr-fixtures.py`.
//!
//! Two recipes are checked:
//!
//! * the whole-page recipe (`finalize_document`): the model answers a page in
//!   one go, and the answer is postprocessed into markdown with HTML tables;
//! * the region recipe (`merge_page_to_markdown`): the model answers each
//!   region of a page, and the regions are merged into one page of markdown.
//!
//! The whole corpus can be checked too, against a directory of raw answers and
//! the vendor's postprocessed answers:
//!
//! ```sh
//! KB_XIAOMI_PARITY_ROOT=/path/to/corpus \
//!   cargo test --release --manifest-path rust/Cargo.toml \
//!   -p kb-core --test xiaomi_ocr_parity -- --nocapture
//! ```
//!
//! The corpus directory holds `raw/<category>/<page>.md` (the model's answer)
//! and `final/<category>/<page>.md` (what the vendor made of it).
//!
//! The markdown table format has no vendor to check against: it is this
//! repository's own addition. It is checked against the reference generator
//! (`rust/scripts/xiaomi-ocr-markdown-ref.py`) instead, over the same corpus:
//!
//! ```sh
//! KB_XIAOMI_MARKDOWN_PARITY=/path/to/corpus \
//!   cargo test --release --manifest-path rust/Cargo.toml \
//!   -p kb-core --test xiaomi_ocr_parity -- --nocapture
//! ```
//!
//! The corpus directory holds `raw/` (the model's answers) and `mdref/` (what
//! the reference generator made of them).
//!
//! The vendor's region merge asks `wordfreq` whether a word split across two
//! blocks is a real word. That package is not installed in the deployment, so
//! the path that runs — and the one the fixtures were made with — is the
//! fallback that only looks at the shape of the word. Keep it that way when
//! regenerating: install nothing, or the expectations move.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use kb_core::xiaomi_ocr::document::{TableFormat, finalize_document, finalize_document_format};
use kb_core::xiaomi_ocr::region::{LayoutRegion, merge_page_to_markdown};
use serde::Deserialize;

/// A region page: the layout regions and the markdown the vendor merged them into.
#[derive(Debug, Deserialize)]
struct RegionFixture {
    regions: Vec<LayoutRegion>,
    expected: String,
}

/// The directory the golden files live in.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xiaomi_ocr")
}

/// Every file in `dir` whose name ends with `suffix`, sorted.
fn files_with_suffix(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.to_string_lossy().ends_with(suffix))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[test]
fn whole_page_answers_match_the_vendor_byte_for_byte() {
    let e2e = fixtures_dir().join("e2e");
    let raws = files_with_suffix(&e2e, ".raw.md");
    assert!(!raws.is_empty(), "no golden files in {}", e2e.display());

    let mut mismatches: Vec<String> = Vec::new();
    for raw_path in &raws {
        let expected_path = raw_path.with_file_name(
            raw_path
                .file_name()
                .map(|name| name.to_string_lossy().replace(".raw.md", ".expected.md"))
                .unwrap_or_default(),
        );
        let raw = fs::read_to_string(raw_path).expect("golden raw file");
        let expected = fs::read_to_string(&expected_path).expect("golden expected file");
        let got = finalize_document(&raw);
        if got != expected {
            mismatches.push(format!(
                "{}: got {} bytes, expected {} bytes",
                raw_path.display(),
                got.len(),
                expected.len()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} golden pages differ:\n{}",
        mismatches.len(),
        raws.len(),
        mismatches.join("\n")
    );
}

#[test]
fn merged_regions_match_the_vendor_byte_for_byte() {
    let regions = fixtures_dir().join("regions");
    let fixtures = files_with_suffix(&regions, ".json");
    assert!(
        !fixtures.is_empty(),
        "no golden files in {}",
        regions.display()
    );

    let mut mismatches: Vec<String> = Vec::new();
    for path in &fixtures {
        let raw = fs::read_to_string(path).expect("golden region file");
        let fixture: RegionFixture = serde_json::from_str(&raw).expect("golden region file parses");
        let got = merge_page_to_markdown(&fixture.regions);
        if got != fixture.expected {
            mismatches.push(format!(
                "{}: got {} bytes, expected {} bytes",
                path.display(),
                got.len(),
                fixture.expected.len()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} golden pages differ:\n{}",
        mismatches.len(),
        fixtures.len(),
        mismatches.join("\n")
    );
}

/// Every `.md` file under `dir`, relative to `dir`.
fn markdown_files(dir: &Path, prefix: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(markdown_files(&path, prefix));
        } else if path.extension().is_some_and(|ext| ext == "md") {
            found.push(
                path.strip_prefix(prefix)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|_| path.clone()),
            );
        }
    }
    found.sort();
    found
}

#[test]
fn the_whole_region_corpus_matches_the_vendor() {
    let Ok(path) = std::env::var("KB_XIAOMI_REGION_PARITY") else {
        eprintln!("KB_XIAOMI_REGION_PARITY is not set; skipping the region corpus check");
        return;
    };
    let raw = fs::read_to_string(&path).expect("region corpus");
    let mut identical = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    let mut pages = 0usize;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        pages += 1;
        let record: RegionCorpusRecord =
            serde_json::from_str(line).expect("region corpus line parses");
        let got = merge_page_to_markdown(&record.layout_result);
        if got == record.markdown_result {
            identical += 1;
        } else {
            mismatches.push(format!(
                "{}: {}",
                record.image_path,
                first_difference(&got, &record.markdown_result)
            ));
        }
    }
    eprintln!(
        "xiaomi-ocr region parity: {identical}/{} pages byte-identical",
        pages
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} pages differ:\n{}",
        mismatches.len(),
        pages,
        mismatches.join("\n")
    );
}

/// Where two answers first differ, and what surrounds the difference.
fn first_difference(got: &str, expected: &str) -> String {
    let got_bytes = got.as_bytes();
    let expected_bytes = expected.as_bytes();
    let mut offset = 0usize;
    while offset < got_bytes.len() && offset < expected_bytes.len() {
        if got_bytes[offset] != expected_bytes[offset] {
            break;
        }
        offset += 1;
    }
    // The offset is a byte offset; the excerpt has to start and end on a
    // character boundary, or the text cannot be sliced out of it.
    let start = floor_boundary(got, offset.saturating_sub(60));
    let got_end = ceil_boundary(got, (offset + 60).min(got.len()));
    let expected_start = floor_boundary(expected, offset.saturating_sub(60));
    let expected_end = ceil_boundary(expected, (offset + 60).min(expected.len()));
    format!(
        "at byte {offset}:\n  got      {:?}\n  expected {:?}",
        &got[start..got_end],
        &expected[expected_start..expected_end]
    )
}

/// The largest boundary at or below `offset`.
fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The smallest boundary at or above `offset`.
fn ceil_boundary(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// One line of the region corpus: the regions and the merged markdown.
#[derive(Debug, Deserialize)]
struct RegionCorpusRecord {
    image_path: String,
    layout_result: Vec<LayoutRegion>,
    markdown_result: String,
}

#[test]
fn the_whole_corpus_matches_the_vendor() {
    let Ok(root) = std::env::var("KB_XIAOMI_PARITY_ROOT") else {
        eprintln!("KB_XIAOMI_PARITY_ROOT is not set; skipping the corpus check");
        return;
    };
    let root = PathBuf::from(root);
    let raw_dir = root.join("raw");
    let final_dir = root.join("final");
    let pages = markdown_files(&raw_dir, &raw_dir);
    assert!(!pages.is_empty(), "no pages under {}", raw_dir.display());

    let mut identical = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for page in &pages {
        let raw = fs::read_to_string(raw_dir.join(page)).expect("raw page");
        let expected = fs::read_to_string(final_dir.join(page)).expect("vendor page");
        let got = finalize_document(&raw);
        if got == expected {
            identical += 1;
        } else {
            mismatches.push(page.display().to_string());
        }
    }
    eprintln!(
        "xiaomi-ocr e2e parity: {identical}/{} pages byte-identical",
        pages.len()
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} pages differ:\n{}",
        mismatches.len(),
        pages.len(),
        mismatches.join("\n")
    );
}

#[test]
fn the_whole_corpus_matches_the_markdown_reference() {
    let Ok(root) = std::env::var("KB_XIAOMI_MARKDOWN_PARITY") else {
        eprintln!("KB_XIAOMI_MARKDOWN_PARITY is not set; skipping the markdown check");
        return;
    };
    let root = PathBuf::from(root);
    let raw_dir = root.join("raw");
    let ref_dir = root.join("mdref");
    let pages = markdown_files(&raw_dir, &raw_dir);
    assert!(!pages.is_empty(), "no pages under {}", raw_dir.display());

    let mut identical = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for page in &pages {
        let raw = fs::read_to_string(raw_dir.join(page)).expect("raw page");
        let expected = fs::read_to_string(ref_dir.join(page)).expect("reference page");
        let got = finalize_document_format(&raw, TableFormat::Markdown);
        if got == expected {
            identical += 1;
        } else {
            mismatches.push(format!(
                "{}: {}",
                page.display(),
                first_difference(&got, &expected)
            ));
        }
    }
    eprintln!(
        "xiaomi-ocr markdown parity: {identical}/{} pages byte-identical",
        pages.len()
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} pages differ:\n{}",
        mismatches.len(),
        pages.len(),
        mismatches.join("\n")
    );
}
