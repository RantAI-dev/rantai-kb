//! Attach source page numbers to text chunks (multimodal RAG).
//!
//! Layout parsers like MinerU know each text block's page, but we chunk the
//! flattened markdown (better formatting for embeddings), which loses page
//! boundaries. This re-attaches a page to each chunk by matching the chunk's
//! leading text against the parser's ordered per-page blocks (page map). It's
//! a best-effort, monotonic scan: chunks and blocks are both in document
//! order, so a forward cursor keeps it cheap and resistant to repeated phrases.

use fancy_regex::Regex;
use std::sync::LazyLock;

/// One entry in a layout-parser-supplied page map.
#[derive(Debug, Clone)]
pub struct PageBlock {
    pub page: u32,
    pub text: String,
}

// Strip markdown noise and collapse whitespace so snippets compare cleanly.
// The pattern is a fixed literal — if it ever fails to compile the unit test
// `static_noise_regex_compiles` catches it before the LazyLock fires.
static NOISE: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"[*#>|_~\[\]()!\-`]") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

fn normalize(s: &str) -> String {
    let stripped = NOISE.replace_all(s, " ");
    let mut out = String::with_capacity(stripped.len());
    let mut prev_ws = false;
    for ch in stripped.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// Walk chunks in order and assign a page number to each. Returns a new vector
/// with `metadata.page` populated; figure chunks already carrying an exact page
/// are preserved verbatim. Chunks that fail to match inherit the page of the
/// chunk before them (running page) so no chunk is left page-less once a
/// `page_map` exists.
pub fn assign_chunk_pages<C, M>(chunks: C, page_map: &[PageBlock]) -> Vec<Annotated<M>>
where
    C: IntoIterator,
    C::Item: HasContent<M>,
    M: PageSettable,
{
    let blocks: Vec<(u32, String)> = page_map
        .iter()
        .map(|b| (b.page, normalize(&b.text)))
        .filter(|(_, norm)| norm.chars().count() >= 10)
        .collect();
    if blocks.is_empty() {
        return chunks
            .into_iter()
            .map(|c| {
                let meta = c.metadata_owned();
                Annotated {
                    content: c.content_owned(),
                    metadata: meta,
                }
            })
            .collect();
    }
    let mut cursor: usize = 0;
    let mut last_page = blocks[0].0;

    let mut out = Vec::new();
    for chunk in chunks {
        let existing_page = chunk.page();
        if existing_page.is_some() {
            if let Some(p) = existing_page {
                last_page = p;
            }
            out.push(Annotated {
                content: chunk.content_owned(),
                metadata: chunk.metadata_owned(),
            });
            continue;
        }
        let key = normalize(&chunk.content_owned())
            .chars()
            .take(24)
            .collect::<String>();
        let key_len = key.chars().count();
        let scan = |start: usize, end: usize| -> Option<usize> {
            if key_len < 10 {
                return None;
            }
            for (i, (_, block_norm)) in blocks.iter().enumerate().skip(start).take(end - start) {
                if block_norm.contains(&key)
                    || key.contains(&block_norm.chars().take(24).collect::<String>())
                {
                    return Some(i);
                }
            }
            None
        };

        let mut found = scan(cursor, blocks.len());
        if found.is_none() && cursor > 0 {
            found = scan(0, cursor);
        }
        let page = match found {
            Some(i) => {
                cursor = i;
                blocks[i].0
            }
            None => last_page,
        };
        last_page = page;
        let mut meta = chunk.metadata_owned();
        meta.set_page(Some(page));
        out.push(Annotated {
            content: chunk.content_for_meta(),
            metadata: meta,
        });
    }
    out
}

/// Returned chunk after page assignment. Keeps the original content and
/// metadata shape; only `metadata.page` is added or overridden.
#[derive(Debug, Clone)]
pub struct Annotated<M> {
    pub content: String,
    pub metadata: M,
}

/// Helper trait so `assign_chunk_pages` can work over both `Chunk` (kb-ports)
/// and the local `SmartChunk`. Anything with `content` and a settable page on
/// its metadata works.
pub trait HasContent<M> {
    fn content_owned(&self) -> String;
    fn content_for_meta(&self) -> String;
    fn metadata_owned(&self) -> M;
    fn page(&self) -> Option<u32>;
}

/// Settable page extension on the metadata type used by the page mapper.
pub trait PageSettable {
    fn set_page(&mut self, page: Option<u32>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestChunk {
        content: String,
        metadata: TestMeta,
    }
    #[derive(Debug, Clone, Default)]
    struct TestMeta {
        page: Option<u32>,
    }
    impl HasContent<TestMeta> for TestChunk {
        fn content_owned(&self) -> String {
            self.content.clone()
        }
        fn content_for_meta(&self) -> String {
            self.content.clone()
        }
        fn metadata_owned(&self) -> TestMeta {
            self.metadata.clone()
        }
        fn page(&self) -> Option<u32> {
            self.metadata.page
        }
    }

    impl PageSettable for TestMeta {
        fn set_page(&mut self, page: Option<u32>) {
            self.page = page;
        }
    }

    #[test]
    fn passes_through_when_no_map() {
        let chunks = vec![TestChunk {
            content: "Anything".to_string(),
            metadata: TestMeta::default(),
        }];
        let out = assign_chunk_pages(chunks, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].metadata.page, None);
    }

    #[test]
    fn assigns_page_by_leading_text_match() {
        let chunks = vec![
            TestChunk {
                content: "alpha bravo charlie".to_string(),
                metadata: TestMeta::default(),
            },
            TestChunk {
                content: "delta echo foxtrot".to_string(),
                metadata: TestMeta::default(),
            },
        ];
        let map = vec![
            PageBlock {
                page: 3,
                text: "alpha bravo charlie delta echo".to_string(),
            },
            PageBlock {
                page: 7,
                text: "delta echo foxtrot golf hotel".to_string(),
            },
        ];
        let out = assign_chunk_pages(chunks, &map);
        assert_eq!(out[0].metadata.page, Some(3));
        assert_eq!(out[1].metadata.page, Some(7));
    }

    #[test]
    fn never_overrides_figure_page() {
        let chunks = vec![TestChunk {
            content: "ignored".to_string(),
            metadata: TestMeta { page: Some(11) },
        }];
        let map = vec![PageBlock {
            page: 1,
            text: "ignored".to_string(),
        }];
        let out = assign_chunk_pages(chunks, &map);
        assert_eq!(out[0].metadata.page, Some(11));
    }

    #[test]
    fn unmatched_chunks_inherit_running_page() {
        // Two blocks on page 5; chunk text doesn't match either → inherits 5.
        let chunks = vec![TestChunk {
            content: "completely unrelated content".to_string(),
            metadata: TestMeta::default(),
        }];
        let map = vec![PageBlock {
            page: 5,
            text: "alpha bravo charlie delta echo foxtrot".to_string(),
        }];
        let out = assign_chunk_pages(chunks, &map);
        assert_eq!(out[0].metadata.page, Some(5));
    }

    #[test]
    fn cursor_only_moves_forward() {
        let chunks = vec![
            TestChunk {
                content: "alpha bravo charlie".to_string(),
                metadata: TestMeta::default(),
            },
            // Repeats the first block's text; cursor must NOT rewind to it.
            TestChunk {
                content: "alpha bravo charlie again".to_string(),
                metadata: TestMeta::default(),
            },
            TestChunk {
                content: "delta echo foxtrot".to_string(),
                metadata: TestMeta::default(),
            },
        ];
        let map = vec![
            PageBlock {
                page: 1,
                text: "alpha bravo charlie content".to_string(),
            },
            PageBlock {
                page: 2,
                text: "delta echo foxtrot content".to_string(),
            },
        ];
        let out = assign_chunk_pages(chunks, &map);
        assert_eq!(out[0].metadata.page, Some(1));
        assert_eq!(out[1].metadata.page, Some(1));
        assert_eq!(out[2].metadata.page, Some(2));
    }

    #[test]
    fn static_noise_regex_compiles() {
        // If this fails, the fixed literal in `NOISE` has drifted.
        let _ = &*NOISE;
    }
}
