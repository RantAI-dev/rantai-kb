#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kb_core::chunk::SmartChunkingOptions;
use kb_core::outline::{OutlineChunk, build_outline};
use kb_core::tables::{Cell, TableGrid, TableSource};
use kb_ports::{
    Embedder, Entity, EntityExtractor, ExtractedFigure, ExtractionOptions, ExtractionResult,
    Extractor, PageBlock, PageBlockKind, PageText, PortError, Relation, RerankCandidate,
    RerankResult, Reranker,
};
use sha2::{Digest, Sha256};

use crate::processor::{
    DocumentProcessor, EncodeMode, ProcessError, ProcessRequest, ProcessorSettings,
};
use crate::seats::SeatCapacity;

enum StubExtractOutcome {
    Ok(Box<ExtractionResult>),
    Unavailable(String),
}

struct StubExtractor {
    outcome: StubExtractOutcome,
}

impl StubExtractor {
    fn ok(result: ExtractionResult) -> Self {
        Self {
            outcome: StubExtractOutcome::Ok(Box::new(result)),
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            outcome: StubExtractOutcome::Unavailable(reason.into()),
        }
    }
}

#[async_trait]
impl Extractor for StubExtractor {
    async fn extract_with_options(
        &self,
        _file_name: &str,
        _mime_type: &str,
        _bytes: &[u8],
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        match &self.outcome {
            StubExtractOutcome::Ok(r) => Ok((**r).clone()),
            StubExtractOutcome::Unavailable(msg) => Err(PortError::Unavailable(msg.clone())),
        }
    }
}

struct StubEmbedder {
    dim: usize,
    unavailable: Option<String>,
}

#[async_trait]
impl Embedder for StubEmbedder {
    fn dimension(&self) -> u32 {
        self.dim as u32
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        if let Some(msg) = &self.unavailable {
            return Err(PortError::Unavailable(msg.clone()));
        }
        Ok(vec![text.len() as f32; self.dim])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        if let Some(msg) = &self.unavailable {
            return Err(PortError::Unavailable(msg.clone()));
        }
        Ok(texts
            .iter()
            .map(|t| vec![t.len() as f32; self.dim])
            .collect())
    }
}

struct StubEntityExtractor;

#[async_trait]
impl EntityExtractor for StubEntityExtractor {
    async fn extract_entities(
        &self,
        _text: &str,
        _doc_id: Option<&str>,
        _user_id: Option<&str>,
    ) -> Result<Vec<Entity>, PortError> {
        Ok(vec![Entity {
            name: "RantAI".into(),
            entity_type: "ORG".into(),
            confidence: 0.95,
            chunk_id: None,
            document_id: None,
            metadata: None,
        }])
    }

    async fn extract_entities_and_relations(
        &self,
        _text: &str,
        _doc_id: Option<&str>,
        _user_id: Option<&str>,
    ) -> Result<(Vec<Entity>, Vec<Relation>), PortError> {
        Ok((
            vec![
                Entity {
                    name: "RantAI".into(),
                    entity_type: "ORG".into(),
                    confidence: 0.95,
                    chunk_id: None,
                    document_id: None,
                    metadata: None,
                },
                Entity {
                    name: "Qdrant".into(),
                    entity_type: "PRODUCT".into(),
                    confidence: 0.9,
                    chunk_id: None,
                    document_id: None,
                    metadata: None,
                },
            ],
            vec![Relation {
                source_entity: "RantAI".into(),
                target_entity: "Qdrant".into(),
                relation_type: "USES".into(),
                confidence: 0.9,
                context: None,
                metadata: None,
            }],
        ))
    }
}

fn ok_extraction(content: &str) -> ExtractionResult {
    ExtractionResult {
        layout_figures: None,
        content: content.to_string(),
        file_type: "markdown".to_string(),
        used_ocr: false,
        figures: Vec::new(),
        pages_blocks: None,
        page_map: None,
        outline: None,
        ocr: None,
        tables: Vec::new(),
        error: None,
    }
}

#[tokio::test]
async fn processes_markdown_without_embedder_and_emits_sparse_vectors() {
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction(
            "# Handbook\n\nParental leave is twelve weeks of paid leave.",
        ))),
        ProcessorSettings::default(),
    );

    assert!(!processor.capabilities().embed);
    assert!(processor.capabilities().sparse);

    let out = processor
        .process(ProcessRequest::new(
            "handbook.md",
            "text/markdown",
            b"# Handbook\n\nParental leave is twelve weeks of paid leave.".to_vec(),
        ))
        .await
        .expect("process");

    assert_eq!(out.title, "Handbook");
    assert_eq!(out.file_type, "markdown");
    assert!(out.embedding_model.is_none());
    assert!(!out.chunks.is_empty());
    assert!(out.chunks[0].embedding.is_none());
    let sparse = out.chunks[0]
        .sparse_vector
        .as_ref()
        .expect("sparse vector present");
    assert!(!sparse.indices.is_empty());
    assert_eq!(sparse.indices.len(), sparse.values.len());
}

#[tokio::test]
async fn requesting_embed_without_embedder_returns_bad_request() {
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction("hello world"))),
        ProcessorSettings::default(),
    );
    let mut req = ProcessRequest::new("note.md", "text/markdown", b"hello world".to_vec());
    req.embed = Some(true);

    let err = processor.process(req).await.unwrap_err();
    assert!(matches!(err, ProcessError::BadRequest(_)));
}

#[tokio::test]
async fn processes_with_embedder_and_respects_embed_false_override() {
    let settings = ProcessorSettings {
        embedding_model: Some("test-embed-1b".into()),
        ..ProcessorSettings::default()
    };
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction(
            "Paragraph one about Qdrant indexing.",
        ))),
        settings,
    )
    .with_embedder(Arc::new(StubEmbedder {
        dim: 4,
        unavailable: None,
    }));

    assert!(processor.capabilities().embed);

    let out = processor
        .process(ProcessRequest::new(
            "doc.md",
            "text/markdown",
            b"Paragraph one about Qdrant indexing.".to_vec(),
        ))
        .await
        .expect("process with embed");
    assert_eq!(out.embedding_model.as_deref(), Some("test-embed-1b"));
    assert_eq!(out.chunks[0].embedding.as_ref().map(Vec::len), Some(4));

    let mut no_embed = ProcessRequest::new(
        "doc.md",
        "text/markdown",
        b"Paragraph one about Qdrant indexing.".to_vec(),
    );
    no_embed.embed = Some(false);
    let out_no_embed = processor
        .process(no_embed)
        .await
        .expect("process without embed");
    assert!(out_no_embed.embedding_model.is_none());
    assert!(out_no_embed.chunks[0].embedding.is_none());
}

#[tokio::test]
async fn emits_tables_and_figures_with_optional_base64_and_anchors() {
    let mut ext = ok_extraction("Intro paragraph explaining the quarterly revenue chart.");
    ext.page_map = Some(vec![PageText {
        page: 0,
        text: "Intro paragraph explaining the quarterly revenue chart.".into(),
    }]);
    ext.tables = vec![TableGrid {
        source: Some(TableSource::Csv),
        page_span: Some((0, 0)),
        title: Some("Revenue".into()),
        header: vec!["Region".into(), "USD".into()],
        rows: vec![vec![Cell::Text("APAC".into()), Cell::Number(1200.0)]],
        ..TableGrid::default()
    }];
    ext.figures = vec![ExtractedFigure {
        id: Some("fig_1".into()),
        page: 0,
        kind: "chart".into(),
        caption: Some("Quarterly Revenue".into()),
        bbox: [0.0, 0.0, 100.0, 100.0],
        image_bytes: vec![137, 80, 78, 71],
    }];
    ext.pages_blocks = Some(vec![vec![
        PageBlock {
            kind: PageBlockKind::Text,
            id: None,
            text: Some("Intro paragraph explaining the quarterly revenue chart.".into()),
        },
        PageBlock {
            kind: PageBlockKind::Figure,
            id: Some("fig_1".into()),
            text: None,
        },
    ]]);

    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ext)),
        ProcessorSettings::default(),
    );

    let mut req = ProcessRequest::new("report.pdf", "application/pdf", b"%PDF".to_vec());
    req.include_figure_bytes = true;

    let out = processor.process(req).await.expect("process");
    assert_eq!(out.tables.len(), 1);
    assert_eq!(out.tables[0].manifest.n_rows, 1);
    assert_eq!(out.figures.len(), 1);
    assert_eq!(out.figures[0].anchor_chunk_index, Some(0));
    assert_eq!(out.figures[0].png_base64.as_deref(), Some("iVBORw=="));

    assert!(
        out.chunks
            .iter()
            .any(|c| c.chunk_type == "table_summary" && c.table_index == Some(0))
    );
    assert!(
        out.chunks
            .iter()
            .any(|c| c.chunk_type == "table_row" && c.table_index == Some(0))
    );
    assert!(
        out.chunks
            .iter()
            .any(|c| c.chunk_type == "figure" && c.figure_index == Some(0))
    );

    // Chunk indices must be contiguous 0..N
    for (idx, chunk) in out.chunks.iter().enumerate() {
        assert_eq!(chunk.chunk_index, idx as u32);
    }
}

#[tokio::test]
async fn entities_are_opt_in_and_use_configured_extractor() {
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction(
            "RantAI uses Qdrant for vector retrieval.",
        ))),
        ProcessorSettings::default(),
    )
    .with_entity_extractor(Arc::new(StubEntityExtractor));

    // Default: entities=false -> empty
    let default_out = processor
        .process(ProcessRequest::new(
            "arch.md",
            "text/markdown",
            b"RantAI uses Qdrant for vector retrieval.".to_vec(),
        ))
        .await
        .expect("default");
    assert!(default_out.entities.is_empty());

    // Opt-in: entities=true -> populated
    let mut req = ProcessRequest::new(
        "arch.md",
        "text/markdown",
        b"RantAI uses Qdrant for vector retrieval.".to_vec(),
    );
    req.entities = true;
    let out = processor.process(req).await.expect("with entities");
    assert_eq!(out.entities.len(), 2);
    assert_eq!(out.relations.len(), 1);
}

#[tokio::test]
async fn unavailable_backend_surfaces_as_unavailable_error() {
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::unavailable("ocr server offline")),
        ProcessorSettings::default(),
    );
    let err = processor
        .process(ProcessRequest::new(
            "scan.pdf",
            "application/pdf",
            b"%PDF".to_vec(),
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, ProcessError::Unavailable(_)));
}

#[tokio::test]
async fn encode_matches_kb_core_sparse_for_query_and_passage() {
    let settings = ProcessorSettings {
        embedding_model: Some("test-embed".into()),
        ..ProcessorSettings::default()
    };
    let processor =
        DocumentProcessor::new(Arc::new(StubExtractor::ok(ok_extraction(""))), settings)
            .with_embedder(Arc::new(StubEmbedder {
                dim: 3,
                unavailable: None,
            }));

    let texts = vec!["Qdrant hybrid BM25 search".to_string()];
    let q = processor
        .encode(&texts, EncodeMode::Query, Some(true), true)
        .await
        .expect("encode query");
    assert_eq!(q.mode, EncodeMode::Query);
    assert_eq!(q.embedding_model.as_deref(), Some("test-embed"));
    assert_eq!(q.embeddings.as_ref().map(|e| e[0].len()), Some(3));
    let expected_q = kb_core::sparse::encode_query(&texts[0]);
    assert_eq!(
        q.sparse_vectors.as_ref().unwrap()[0].indices,
        expected_q.indices
    );
    assert_eq!(
        q.sparse_vectors.as_ref().unwrap()[0].values,
        expected_q.values
    );

    let p = processor
        .encode(&texts, EncodeMode::Passage, Some(false), true)
        .await
        .expect("encode passage");
    assert!(p.embeddings.is_none());
    let expected_p = kb_core::sparse::encode_document(&texts[0]);
    assert_eq!(
        p.sparse_vectors.as_ref().unwrap()[0].indices,
        expected_p.indices
    );
    assert_eq!(
        p.sparse_vectors.as_ref().unwrap()[0].values,
        expected_p.values
    );
}

/// Records the text sent through `embed_passages`, so a parity test can prove
/// that ingest and `/v1/encode mode=passage` take the same batch route.
#[derive(Default)]
struct RecordingEmbedder {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Embedder for RecordingEmbedder {
    fn dimension(&self) -> u32 {
        4
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![text.len() as f32; 4])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        self.seen
            .lock()
            .expect("mutex")
            .extend(texts.iter().cloned());
        Ok(texts.iter().map(|t| vec![t.len() as f32; 4]).collect())
    }
}

/// The stateless pipeline must embed a processed chunk through exactly the
/// same `embed_passages` route as `/v1/encode mode=passage`; otherwise a caller
/// that encodes its own chunks indexes vectors in a different space (ING-9).
#[tokio::test]
async fn process_and_encode_passage_embed_the_same_text() {
    let text = "Cuti tahunan selama dua belas hari.";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction(text))),
        ProcessorSettings {
            embedding_model: Some("test-embed".into()),
            ..ProcessorSettings::default()
        },
    )
    .with_embedder(Arc::new(RecordingEmbedder {
        seen: Arc::clone(&seen),
    }));

    let out = processor
        .process(ProcessRequest::new(
            "",
            "text/markdown",
            text.as_bytes().to_vec(),
        ))
        .await
        .expect("process");
    let processed_text = seen.lock().expect("mutex")[0].clone();
    let processed_vector = out.chunks[0].embedding.clone().expect("embedding");

    let encoded = processor
        .encode(
            std::slice::from_ref(&processed_text),
            EncodeMode::Passage,
            Some(true),
            false,
        )
        .await
        .expect("encode passage");
    assert_eq!(encoded.embeddings.expect("embeddings")[0], processed_vector);

    let seen = seen.lock().expect("mutex");
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0], seen[1],
        "process and /v1/encode mode=passage send the same text to the embedder"
    );
}

struct BlockingPassageEmbedder {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Embedder for BlockingPassageEmbedder {
    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![0.5, 0.5])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(vec![vec![0.1, 0.2]; texts.len()])
    }

    fn dimension(&self) -> u32 {
        2
    }
}

#[tokio::test]
async fn encode_query_bypasses_busy_ingest_embed_seats() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let settings = ProcessorSettings {
        seats: SeatCapacity {
            extract: 1,
            embed: 1,
            store: 1,
        },
        embedding_model: Some("test-embed".into()),
        ..ProcessorSettings::default()
    };
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction("hello"))),
        settings,
    )
    .with_embedder(Arc::new(BlockingPassageEmbedder {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    }));

    // Occupy the single ingest embed seat with a blocked passage batch.
    let proc_bg = processor.clone();
    let passage_task = tokio::spawn(async move {
        proc_bg
            .encode(
                &["slow bulk chunk".to_string()],
                EncodeMode::Passage,
                Some(true),
                false,
            )
            .await
    });
    started.notified().await;

    // Live query encoding must complete immediately rather than stalling behind ingest.
    let query_res = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        processor.encode(
            &["live user query".to_string()],
            EncodeMode::Query,
            Some(true),
            true,
        ),
    )
    .await
    .expect("query encode should not block on busy ingest embed seats")
    .expect("query encode ok");
    assert_eq!(query_res.embeddings.as_ref().map(|v| v.len()), Some(1));

    release.notify_one();
    passage_task.await.expect("join").expect("passage ok");
}

struct StubReranker;

#[async_trait]
impl Reranker for StubReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        let mut scored: Vec<(RerankCandidate, f32)> = candidates
            .into_iter()
            .map(|c| {
                let score = if c.text.to_lowercase().contains(&query.to_lowercase()) {
                    0.95
                } else {
                    0.15
                };
                (c, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(final_k as usize);
        Ok(scored
            .into_iter()
            .enumerate()
            .map(|(rank, (c, score))| RerankResult {
                id: c.id,
                final_rank: rank as u32,
                score,
            })
            .collect())
    }
}

/// ING-14: the stateless response carries the same spans as ingest, and the
/// documented stitching rule (full first chunk, then skip the overlap prefix)
/// rebuilds the chunker's own normalised source without duplicated overlap.
#[tokio::test]
async fn process_emits_chunk_spans_that_reconstruct_the_normalised_text() {
    let text = "Paragraph one is deliberately long enough that the smart chunker has to \
split it and repeat an overlap prefix into the next chunk. Paragraph two keeps the same \
document going well past the budget so several chunks are emitted. Paragraph three adds \
more material with a few Indonesian legal words: cuti tahunan, berlaku, danQdrant. \
Paragraph four closes the document with yet another long stretch of ordinary prose that \
ensures the overlap is present on at least one later chunk.\n\n";
    let text = text.repeat(4);
    let options = SmartChunkingOptions {
        max_chunk_size: Some(220),
        overlap_size: Some(40),
        ..SmartChunkingOptions::default()
    };
    let settings = ProcessorSettings {
        chunking: options,
        ..ProcessorSettings::default()
    };
    let processor =
        DocumentProcessor::new(Arc::new(StubExtractor::ok(ok_extraction(&text))), settings);
    let out = processor
        .process(ProcessRequest::new(
            "spans.md",
            "text/markdown",
            text.as_bytes().to_vec(),
        ))
        .await
        .expect("process");

    assert!(out.chunks.len() > 1, "fixture must produce multiple chunks");
    for chunk in &out.chunks {
        assert!(chunk.char_start.is_some(), "chunk has charStart");
        assert!(chunk.char_end.is_some(), "chunk has charEnd");
        assert!(
            chunk.overlap_prefix_len.is_some(),
            "chunk has overlapPrefixLen"
        );
    }
    assert!(
        out.chunks
            .iter()
            .any(|c| c.overlap_prefix_len.unwrap_or(0) > 0),
        "fixture must produce at least one overlapping chunk"
    );

    // RET-14 stitching rule: full text for the first chunk; then skip the
    // overlap prefix when there is one, otherwise join with a blank line.
    let mut stitched = String::new();
    for (i, chunk) in out.chunks.iter().enumerate() {
        if i == 0 {
            stitched.push_str(&chunk.content);
        } else if chunk.overlap_prefix_len.unwrap_or(0) > 0 {
            let skip = usize::try_from(chunk.overlap_prefix_len.unwrap()).unwrap();
            stitched.extend(chunk.content.chars().skip(skip));
        } else {
            stitched.push_str("\n\n");
            stitched.push_str(&chunk.content);
        }
    }
    let title = kb_core::title::document_title(None, "spans.md", &text, true);
    let core_chunks = kb_core::chunk::smart_chunk_document(&text, &title, "", None, options);
    let expected: String = core_chunks
        .iter()
        .flat_map(|c| {
            c.content
                .chars()
                .skip(c.metadata.overlap_prefix_len.unwrap_or(0) as usize)
        })
        .collect();
    assert_eq!(
        stitched, expected,
        "stitched /v1/process chunks equal the ING-13 reconstruction"
    );
    for (i, (processed, core)) in out.chunks.iter().zip(&core_chunks).enumerate() {
        assert_eq!(processed.char_start, core.metadata.char_start, "chunk {i}");
        assert_eq!(processed.char_end, core.metadata.char_end, "chunk {i}");
        assert_eq!(
            processed.overlap_prefix_len, core.metadata.overlap_prefix_len,
            "chunk {i}"
        );
    }
}

/// ING-14: the stateless outline is the same `kb_core` build as `/structure`
/// and a BAB keeps both of its Pasal in its chunk range.
#[tokio::test]
async fn process_emits_outline_v2_sections_for_regulation_markdown() {
    let body = " Ketentuan ini mengatur pelaksanaan kerja dan berlaku secara nasional di \
seluruh unit kerja tanpa pengecualian, termasuk cabang daerah."
        .repeat(3);
    let pages: Vec<(u32, String)> = vec![
        (0, format!("BAB I KETENTUAN UMUM\n\n{body}")),
        (1, format!("Pasal 1\n\n{body}")),
        (2, format!("Pasal 2\n\n{body}")),
        (3, format!("BAB II PELAKSANAAN\n\n{body}")),
        (4, format!("Bagian Kesatu\n\n{body}")),
        (5, format!("Pasal 3\n\n{body}")),
    ];
    let content = pages
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut extraction = ok_extraction(&content);
    extraction.page_map = Some(
        pages
            .iter()
            .map(|(page, text)| PageText {
                page: *page,
                text: text.clone(),
            })
            .collect(),
    );
    let processor = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(extraction)),
        ProcessorSettings {
            chunking: SmartChunkingOptions {
                max_chunk_size: Some(180),
                overlap_size: Some(0),
                ..SmartChunkingOptions::default()
            },
            ..ProcessorSettings::default()
        },
    );
    let out = processor
        .process(ProcessRequest::new(
            "regulation.md",
            "text/markdown",
            content.as_bytes().to_vec(),
        ))
        .await
        .expect("process");

    assert_eq!(out.outline_version, 2);
    assert!(!out.sections.is_empty());

    let outline_chunks: Vec<OutlineChunk> = out
        .chunks
        .iter()
        .map(|c| OutlineChunk {
            chunk_index: c.chunk_index,
            page: c.page,
            section: c.section.clone(),
            content: c.content.clone(),
            char_start: c.char_start,
            char_end: c.char_end,
            overlap_prefix_len: c.overlap_prefix_len,
        })
        .collect();
    assert_eq!(
        out.sections,
        build_outline(&outline_chunks, out.outline.as_ref()),
        "sections are exactly what build_outline gives for the same chunks"
    );

    let bab = out
        .sections
        .iter()
        .find(|s| s.title.starts_with("BAB I"))
        .expect("BAB I section");
    let pasal1 = out
        .sections
        .iter()
        .find(|s| s.title == "Pasal 1")
        .expect("Pasal 1 section");
    let pasal2 = out
        .sections
        .iter()
        .find(|s| s.title == "Pasal 2")
        .expect("Pasal 2 section");
    assert!(
        bab.chunk_start <= pasal1.chunk_start && bab.chunk_end >= pasal2.chunk_end,
        "BAB I ({:?}) must cover Pasal 1 ({:?}) and Pasal 2 ({:?})",
        bab,
        pasal1,
        pasal2
    );
}

/// ING-14: per-page digests are lowercase hex SHA-256 of the page text that is
/// returned in `pageMap`, stable for the same input and changed when one page
/// changes. Existing fields stay present.
#[tokio::test]
async fn process_emits_stable_page_sha256_and_keeps_existing_fields() {
    let first = "Halaman satu berisi ketentuan awal yang tidak berubah.";
    let second = "Halaman dua berisi ketentuan lanjutan.";
    let content = format!("{first}\n\n{second}");
    let extraction = |second_text: &str| {
        let mut ext = ok_extraction(&content);
        ext.page_map = Some(vec![
            PageText {
                page: 0,
                text: first.to_string(),
            },
            PageText {
                page: 1,
                text: second_text.to_string(),
            },
        ]);
        ext
    };
    let run = |second_text: &str| {
        let processor = DocumentProcessor::new(
            Arc::new(StubExtractor::ok(extraction(second_text))),
            ProcessorSettings::default(),
        );
        let bytes = content.clone().into_bytes();
        async move {
            processor
                .process(ProcessRequest::new("pages.md", "text/markdown", bytes))
                .await
                .expect("process")
        }
    };

    let a = run(second).await;
    let b = run(second).await;
    let changed = run("Halaman dua berisi ketentuan lanjutan yang sudah direvisi.").await;

    assert_eq!(a.pages.len(), 2);
    assert_eq!(a.pages, b.pages, "pageSha256 is stable for the same input");
    for (page, expected_text) in a.pages.iter().zip([first, second]) {
        assert_eq!(
            page.page_sha256,
            hex::encode(Sha256::digest(expected_text.as_bytes()))
        );
        assert_eq!(page.page_sha256.len(), 64);
        assert!(
            page.page_sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex"
        );
    }
    assert_eq!(a.pages[0].page_sha256, changed.pages[0].page_sha256);
    assert_ne!(a.pages[1].page_sha256, changed.pages[1].page_sha256);

    // The old response fields are unchanged by the additive fields.
    assert_eq!(a.title, "pages");
    assert_eq!(a.sha256.len(), 64);
    assert_eq!(a.content, content);
    assert_eq!(a.page_map.len(), 2);
    assert!(!a.chunks.is_empty());
    assert!(a.tables.is_empty());
    assert!(a.figures.is_empty());
    assert!(a.entities.is_empty());
    assert!(a.timings.total_ms < u64::MAX);
}

#[tokio::test]
async fn rerank_scores_orders_and_validates_inputs() {
    let base = DocumentProcessor::new(
        Arc::new(StubExtractor::ok(ok_extraction(""))),
        ProcessorSettings::default(),
    );
    assert!(!base.capabilities().rerank);
    let err = base
        .rerank("qdrant", &["doc".to_string()], None, false)
        .await
        .unwrap_err();
    assert!(matches!(err, ProcessError::BadRequest(_)));

    let processor = base.with_reranker(
        Arc::new(StubReranker),
        Some("voyageai/rerank-2.5".to_string()),
    );
    assert!(processor.capabilities().rerank);
    assert_eq!(processor.rerank_model(), Some("voyageai/rerank-2.5"));

    let docs = vec![
        "Bananas are yellow fruit.".to_string(),
        "Qdrant stores dense and BM25 sparse vectors.".to_string(),
        "Weather forecast for tomorrow.".to_string(),
    ];
    let out = processor
        .rerank("qdrant", &docs, Some(2), true)
        .await
        .expect("rerank");
    assert_eq!(out.model.as_deref(), Some("voyageai/rerank-2.5"));
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].index, 1);
    assert_eq!(out.results[0].rank, 0);
    assert!(out.results[0].score > out.results[1].score);
    assert_eq!(
        out.results[0].document.as_deref(),
        Some("Qdrant stores dense and BM25 sparse vectors.")
    );

    // Without return_documents, document text is omitted.
    let slim = processor
        .rerank("qdrant", &docs, Some(1), false)
        .await
        .expect("rerank slim");
    assert_eq!(slim.results.len(), 1);
    assert!(slim.results[0].document.is_none());

    // Validation errors.
    assert!(matches!(
        processor.rerank("   ", &docs, None, false).await,
        Err(ProcessError::BadRequest(_))
    ));
    assert!(matches!(
        processor.rerank("qdrant", &[], None, false).await,
        Err(ProcessError::BadRequest(_))
    ));
    assert!(matches!(
        processor.rerank("qdrant", &docs, Some(0), false).await,
        Err(ProcessError::BadRequest(_))
    ));
}
