//! How often does ingest tag a chunk with the wrong page?
//!
//!     cargo run -p kb-server --example page_assign_check -- <pdf>...
//!
//! Runs the production extractor, then exactly ingest's chunking and page
//! assignment (`smart_chunk_pages`, else `smart_chunk_document`, with
//! `INGEST_CHUNKING`, then `assign_chunk_pages`), and checks each chunk against the page text: a chunk
//! is "right" when a 40-character probe from its middle occurs on the page it
//! was tagged with, "wrong" when the probe occurs only on another page, and
//! "unknown" when the probe is on no page or on several.
use kb_core::chunk::{smart_chunk_document, smart_chunk_pages};
use kb_core::page_map::{PageBlock, assign_chunk_pages};
use kb_ports::Extractor;

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = kb_extract::ExtractConfig::from_lookup(|k| std::env::var(k).ok());
    let extractor = kb_extract::build_production_extractor(&cfg)?;
    let (mut right, mut wrong, mut unknown, mut cross) = (0usize, 0usize, 0usize, 0usize);
    let mut offsets: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path)?;
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        let out = extractor.extract(&name, "application/pdf", &bytes).await?;
        let pages = out.page_map.clone().unwrap_or_default();
        let page_norm: Vec<(u32, String)> = pages.iter().map(|p| (p.page, norm(&p.text))).collect();
        // Exactly ingest's path: page by page when the content is its pages,
        // else the whole document, then tag pages.
        let texts: Vec<(u32, String)> = pages.iter().map(|p| (p.page, p.text.clone())).collect();
        let chunks = smart_chunk_pages(
            &out.content,
            &texts,
            &name,
            "",
            None,
            kb_engine::ingest::INGEST_CHUNKING,
        )
        .unwrap_or_else(|| {
            smart_chunk_document(
                &out.content,
                &name,
                "",
                None,
                kb_engine::ingest::INGEST_CHUNKING,
            )
        });
        let blocks: Vec<PageBlock> = pages
            .iter()
            .map(|p| PageBlock {
                page: p.page,
                text: p.text.clone(),
            })
            .collect();
        let annotated = assign_chunk_pages(chunks, &blocks);
        for a in &annotated {
            let text = norm(&a.content);
            let chars: Vec<char> = text.chars().collect();
            if chars.len() < 80 {
                continue;
            }
            let mid = chars.len() / 2;
            let probe: String = chars[mid - 20..mid + 20].iter().collect();
            let hits: Vec<u32> = page_norm
                .iter()
                .filter(|(_, t)| t.contains(&probe))
                .map(|(p, _)| *p)
                .collect();
            let head: String = chars[..40].iter().collect();
            let tail: String = chars[chars.len() - 40..].iter().collect();
            let hp: Vec<u32> = page_norm
                .iter()
                .filter(|(_, t)| t.contains(&head))
                .map(|(p, _)| *p)
                .collect();
            let tp: Vec<u32> = page_norm
                .iter()
                .filter(|(_, t)| t.contains(&tail))
                .map(|(p, _)| *p)
                .collect();
            if hp.len() == 1 && tp.len() == 1 && hp[0] != tp[0] {
                cross += 1;
            }
            let tagged = a.metadata.page;
            match (hits.as_slice(), tagged) {
                ([p], Some(t)) if *p == t => right += 1,
                ([p], Some(t)) => {
                    wrong += 1;
                    *offsets.entry(i64::from(t) - i64::from(*p)).or_default() += 1;
                }
                _ => unknown += 1,
            }
        }
    }
    let total = right + wrong + unknown;
    println!(
        "chunks {total}: right {right}, wrong {wrong}, unknown {unknown}; head and tail on different pages: {cross}"
    );
    println!("tagged page minus true page, for the wrong ones: {offsets:?}");
    Ok(())
}
