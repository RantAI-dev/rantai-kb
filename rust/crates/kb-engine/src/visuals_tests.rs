#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The visual stage of ingest (`ING-5`), end to end through the worker.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kb_ports::{
    BlobStore, DocumentId, DocumentMeta, ExtractionResult, ImageDescriber, ImageReading, JobRecord,
    NormalizedImage, PortError, RetrievalIndex, TenantId, Visual, VisualCollector, VisualKind,
    VisualOptions, VisualOrigin, VisualScan,
};

use crate::fakes::{
    FakeBlobStore, FakeDocumentStore, FakeEmbedder, FakeExtractor, FakeJobStore, make_fake_scope,
};
use crate::in_memory_index::InMemoryIndex;
use crate::ingest::{IngestSettings, IngestWorker};
use crate::visuals::{VisualPipeline, VisualSettings};

const TENANT: &str = "tenant-1";
const DOC: &str = "doc_1";

fn tenant() -> TenantId {
    TenantId::new_unchecked(TENANT)
}

fn visual(origin: VisualOrigin, page: u32, w: u32, h: u32, byte: u8, alt: Option<&str>) -> Visual {
    Visual {
        origin,
        page,
        mime: "image/jpeg".into(),
        // Byte 0 tells the fake describer what to answer; the page makes the
        // pixels distinct, so selection does not take two pictures for one.
        bytes: [vec![byte], vec![u8::try_from(page).unwrap(); 63]].concat(),
        width: w,
        height: h,
        alt_text: alt.map(str::to_string),
        caption: None,
        bbox: None,
        figure_id: None,
        kind_hint: None,
    }
}

/// Hands back a fixed scan.
struct Collector(Vec<Visual>);

#[async_trait]
impl VisualCollector for Collector {
    async fn collect(
        &self,
        _: &str,
        _: &str,
        _: &[u8],
        _: VisualOptions,
    ) -> Result<VisualScan, PortError> {
        Ok(VisualScan {
            visuals: self.0.clone(),
            skipped: kb_ports::VisualScanSkips {
                unsupported_format: u32::from(!self.0.is_empty()),
                undecodable: 0,
            },
        })
    }
    async fn normalize(&self, _: &[u8]) -> Result<Option<NormalizedImage>, PortError> {
        Ok(None)
    }
}

/// Reads byte 0xAA as a logo, 0xEE as a failure, anything else as a photo
/// whose description names the byte.
struct Describer(AtomicUsize);

#[async_trait]
impl ImageDescriber for Describer {
    async fn read(&self, _: &str, bytes: &[u8]) -> Result<ImageReading, PortError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        match bytes[0] {
            0xEE => Err(PortError::Unavailable("http 429: slow down".into())),
            0xAA => Ok(ImageReading {
                kind: VisualKind::Logo,
                description: "A company logo.".into(),
                text: "ACME".into(),
            }),
            b => Ok(ImageReading {
                kind: VisualKind::Photo,
                description: format!("A red bicycle leaning on a wall, picture {b}."),
                text: String::new(),
            }),
        }
    }
}

struct Run {
    outcome: crate::ingest::JobOutcome,
    index: Arc<InMemoryIndex>,
    documents: Arc<FakeDocumentStore>,
    blobs: Arc<FakeBlobStore>,
    describer: Arc<Describer>,
}

async fn ingest(
    filename: &str,
    extraction: ExtractionResult,
    visuals: Vec<Visual>,
    with_describer: bool,
) -> Run {
    let jobs = Arc::new(FakeJobStore::default());
    let documents = Arc::new(FakeDocumentStore::default());
    let blobs = Arc::new(FakeBlobStore::default());
    let key = format!("documents/{TENANT}/{DOC}/{filename}");
    documents.insert_doc(
        tenant(),
        DocumentMeta {
            id: DocumentId::new_unchecked(DOC),
            title: "t".into(),
            categories: vec!["c".into()],
            subcategory: None,
            status: "processing".into(),
            external_ref: None,
            chunk_count: 0,
            file_type: None,
            file_size: None,
            mime_type: None,
            s3_key: Some(key.clone()),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            knowledge_base_ids: vec![],
            metadata: None,
        },
    );
    blobs
        .upload(&key, b"bytes".to_vec(), "image/jpeg")
        .await
        .unwrap();
    let index = Arc::new(InMemoryIndex::new(4));
    let describer = Arc::new(Describer(AtomicUsize::new(0)));
    let pipeline = VisualPipeline::new(
        Arc::new(Collector(visuals)),
        with_describer.then(|| describer.clone() as Arc<dyn ImageDescriber>),
        VisualSettings::default(),
    );
    let worker = IngestWorker::new(
        jobs.clone(),
        documents.clone(),
        index.clone(),
        blobs.clone(),
        Arc::new(FakeExtractor::returning_result(extraction)),
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            embedding_model: Some("m".into()),
            ..IngestSettings::default()
        },
    )
    .with_visuals(Arc::new(pipeline));
    let job = JobRecord {
        id: "job_1".into(),
        tenant_id: tenant(),
        user_id: None,
        document_id: Some(DocumentId::new_unchecked(DOC)),
        s3_key: Some(key),
        filename: filename.into(),
        mime_type: None,
        attempt: 1,
        params: Some(serde_json::json!({"categories": ["c"]})),
    };
    let outcome = worker.run_job(job).await;
    Run {
        outcome,
        index,
        documents,
        blobs,
        describer,
    }
}

fn image_extraction(text: &str) -> ExtractionResult {
    ExtractionResult::text(text, "image")
}

fn metadata(run: &Run) -> serde_json::Value {
    run.documents.docs.read().unwrap()[&tenant()][0]
        .metadata
        .clone()
        .unwrap_or_default()
}

async fn figures_for(run: &Run, tenant: &str) -> Vec<kb_ports::Candidate> {
    let doc = DocumentId::new_unchecked(DOC);
    let scope = make_fake_scope(TenantId::new_unchecked(tenant), vec![doc.clone()]);
    run.index.figures(&scope, &[doc]).await.unwrap()
}

#[tokio::test]
async fn a_photo_with_no_text_is_found_by_what_it_shows() {
    let run = ingest(
        "bike.jpg",
        image_extraction(""),
        vec![visual(VisualOrigin::Upload, 0, 640, 480, 1, None)],
        true,
    )
    .await;
    assert!(
        matches!(run.outcome, crate::ingest::JobOutcome::Ready { chunks: 1 }),
        "{:?}",
        run.outcome
    );
    let figures = figures_for(&run, TENANT).await;
    assert_eq!(figures.len(), 1);
    let chunk = &figures[0];
    assert!(
        chunk.content.starts_with("[Photo] A red bicycle"),
        "{}",
        chunk.content
    );
    let key = chunk.metadata["assetKey"]
        .as_str()
        .or_else(|| chunk.metadata["asset_key"].as_str())
        .expect("an asset key")
        .to_string();
    assert!(key.ends_with("/assets/fig-p0-1.jpg"), "{key}");
    assert!(
        run.blobs.objects.read().unwrap().contains_key(&key),
        "the picture is stored"
    );
    let meta = metadata(&run);
    assert_eq!(meta["visuals"]["indexed"], 1);
    assert_eq!(meta["visuals"]["described"], 1);
    assert_eq!(meta["figures"][0]["origin"], "upload");
    assert_eq!(meta["figures"][0]["type"], "photo");
}

#[tokio::test]
async fn another_tenant_never_sees_a_picture_chunk() {
    let run = ingest(
        "bike.jpg",
        image_extraction(""),
        vec![visual(VisualOrigin::Upload, 0, 640, 480, 1, None)],
        true,
    )
    .await;
    assert_eq!(figures_for(&run, TENANT).await.len(), 1);
    assert!(figures_for(&run, "tenant-2").await.is_empty());
}

#[tokio::test]
async fn an_image_nothing_can_read_fails_instead_of_finishing_empty() {
    let run = ingest(
        "bike.jpg",
        image_extraction(""),
        vec![visual(VisualOrigin::Upload, 0, 640, 480, 1, None)],
        false,
    )
    .await;
    match &run.outcome {
        crate::ingest::JobOutcome::Failed { reason } => {
            assert!(
                reason.contains("nothing searchable in \"bike.jpg\""),
                "{reason}"
            );
            assert!(reason.contains("KB_IMAGE_DESCRIBE_MODEL"), "{reason}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_image_whose_reading_fails_says_why() {
    let run = ingest(
        "bike.jpg",
        image_extraction(""),
        vec![visual(VisualOrigin::Upload, 0, 640, 480, 0xEE, None)],
        true,
    )
    .await;
    match &run.outcome {
        crate::ingest::JobOutcome::Failed { reason } => {
            assert!(reason.contains("could not be described"), "{reason}");
            assert!(reason.contains("429"), "{reason}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_image_with_ocr_text_but_no_describer_is_still_searchable_by_its_text() {
    let run = ingest(
        "sign.jpg",
        image_extraction("STOP — road closed for repairs until Friday."),
        vec![visual(VisualOrigin::Upload, 0, 640, 480, 1, None)],
        false,
    )
    .await;
    assert!(
        matches!(run.outcome, crate::ingest::JobOutcome::Ready { .. }),
        "{:?}",
        run.outcome
    );
    assert_eq!(metadata(&run)["visuals"]["undescribed"], 1);
}

#[tokio::test]
async fn pictures_inside_a_document_skip_icons_logos_and_failures_visibly() {
    let body = "A field report on the bicycle lanes of the city, with photographs.";
    let visuals = vec![
        visual(VisualOrigin::Embedded, 0, 800, 600, 1, None), // photo, described
        visual(VisualOrigin::Embedded, 1, 24, 24, 2, None),   // icon: too small
        visual(VisualOrigin::Embedded, 1, 400, 300, 0xAA, None), // the describer's logo
        visual(
            VisualOrigin::Embedded,
            2,
            800,
            600,
            0xEE,
            Some("Lane map of the old town"),
        ), // fails, has alt text
        visual(VisualOrigin::Embedded, 3, 800, 600, 0xEE, None), // fails, nothing known
    ];
    let run = ingest(
        "report.docx",
        ExtractionResult::text(body, "markdown"),
        visuals,
        true,
    )
    .await;
    assert!(
        matches!(run.outcome, crate::ingest::JobOutcome::Ready { .. }),
        "{:?}",
        run.outcome
    );
    let figures = figures_for(&run, TENANT).await;
    let contents: Vec<&str> = figures.iter().map(|c| c.content.as_str()).collect();
    assert_eq!(figures.len(), 2, "{contents:?}");
    assert!(
        contents
            .iter()
            .any(|c| c.starts_with("[Photo] A red bicycle")),
        "{contents:?}"
    );
    assert!(
        contents
            .iter()
            .any(|c| c.contains("Alt text") || c.contains("Lane map of the old town")),
        "the alt text keeps a failed picture searchable: {contents:?}"
    );
    let v = &metadata(&run)["visuals"];
    assert_eq!(v["found"], 5);
    assert_eq!(v["indexed"], 2);
    assert_eq!(v["skipped"]["tooSmall"], 1);
    assert_eq!(v["ornaments"], 1);
    assert_eq!(v["describeFailed"], 2);
    assert_eq!(v["undescribed"], 1);
    assert_eq!(v["unsupportedFormat"], 1);
    assert!(v["error"].as_str().unwrap().contains("429"));
    assert_eq!(
        run.describer.0.load(Ordering::SeqCst),
        4,
        "the icon is never sent"
    );
}

#[tokio::test]
async fn a_text_document_with_no_pictures_is_untouched() {
    let run = ingest(
        "notes.md",
        ExtractionResult::text(
            "# Notes\n\nPlain text only, long enough to chunk once.",
            "markdown",
        ),
        Vec::new(),
        true,
    )
    .await;
    assert!(matches!(
        run.outcome,
        crate::ingest::JobOutcome::Ready { .. }
    ));
    assert!(figures_for(&run, TENANT).await.is_empty());
    assert!(
        metadata(&run).get("visuals").is_none(),
        "no metadata written"
    );
    assert_eq!(run.describer.0.load(Ordering::SeqCst), 0);
}
