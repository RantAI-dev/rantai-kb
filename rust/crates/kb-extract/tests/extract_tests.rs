#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for kb-extract adapters and HTTP clients.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    http::StatusCode,
    response::Json,
    routing::post,
};
use kb_extract::*;
use kb_ports::*;
use serde_json::json;
use tokio::net::TcpListener;

#[test]
fn test_odt_xml_parser() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Chapter 1: The Beginning</text:h>
      <text:p>This is a paragraph with <text:span>formatted text</text:span> and<text:s text:c="3"/>spaces.</text:p>
      <text:p>Second paragraph.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

    let text = odt::parse_odt_content_xml(xml).unwrap();
    assert!(text.contains("Chapter 1: The Beginning"));
    assert!(text.contains("This is a paragraph with formatted text and   spaces."));
    assert!(text.contains("Second paragraph."));
}

#[test]
fn test_ods_xml_parser() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell><text:p>Name</text:p></table:table-cell>
          <table:table-cell><text:p>Role, Title</text:p></table:table-cell>
          <table:table-cell><text:p>Count</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>Alice</text:p></table:table-cell>
          <table:table-cell><text:p>Engineer</text:p></table:table-cell>
          <table:table-cell><text:p>42</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

    let text = ods::parse_ods_content_xml(xml).unwrap();
    assert!(text.contains("Name,\"Role, Title\",Count"));
    assert!(text.contains("Alice,Engineer,42"));
}

#[test]
fn test_scanned_detection_and_pdfium_text() {
    let Ok(pdfium) = get_pdfium() else {
        eprintln!("Skipping pdfium test: libpdfium not loaded");
        return;
    };

    // Hello world has 10 chars (< 100 non-whitespace chars/page threshold) -> is considered scanned
    assert!(is_pdf_scanned(pdfium, MINIMAL_PDF_HELLO_WORLD));

    // Blank PDF has 0 chars -> scanned
    assert!(is_pdf_scanned(pdfium, MINIMAL_BLANK_PDF));
}

#[test]
fn test_hybrid_merge_anchor_matching() {
    let structural = "# Title\n\nFirst five words of paragraph one and then something OCR garbled before last five words of end.\n\n| Col A | Col B |\n|---|---|\n| 1 | 2 |\n";
    let text_layer = "First five words of paragraph one and pristine high fidelity character text before last five words of end.";

    let merged = merge_structural_with_text_layer(structural, text_layer);
    assert!(merged.contains("# Title"));
    assert!(merged.contains("| Col A | Col B |"));
    assert!(merged.contains("pristine high fidelity character text"));
}

#[tokio::test]
async fn test_mineru_sidecar_mock() {
    let app = Router::new().route(
        "/extract",
        post(|| async {
            Json(json!({
                "text": "# Extracted Title\n\nSome body text.",
                "ms": 120,
                "pages": 1,
                "pages_text": ["# Extracted Title\n\nSome body text."],
                "figures": []
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let extractor =
        MineruExtractor::new(format!("http://{addr}"), Some(Duration::from_secs(5))).unwrap();
    let res = extractor
        .extract_pdf(b"dummy pdf bytes", ExtractionOptions::default())
        .await
        .unwrap();

    assert_eq!(res.content, "# Extracted Title\n\nSome body text.");
    assert_eq!(res.file_type, "pdf");
    assert!(res.used_ocr);
    assert_eq!(res.page_map.unwrap()[0].text, "# Extracted Title\n\nSome body text.");
}

#[tokio::test]
async fn test_mistral_ocr_mock() {
    let app = Router::new().route(
        "/v1/ocr",
        post(|| async {
            Json(json!({
                "pages": [
                    {
                        "index": 0,
                        "markdown": "## Mistral Page 1\n\nExtracted content.",
                        "images": []
                    }
                ]
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let extractor = MistralOcrExtractor::new(
        "test-token",
        Some(format!("http://{addr}")),
        Some("mistral-ocr-latest".into()),
        Some(Duration::from_secs(5)),
    )
    .unwrap();

    let res = extractor
        .extract_pdf(b"dummy pdf bytes", ExtractionOptions::default())
        .await
        .unwrap();

    assert_eq!(res.content, "## Mistral Page 1\n\nExtracted content.");
    assert_eq!(res.file_type, "pdf");
    assert!(res.used_ocr);
    assert_eq!(res.page_map.unwrap()[0].page, 0);
}

#[tokio::test]
async fn test_openrouter_vision_image_formatting() {
    let app = Router::new().route(
        "/api/v1/chat/completions",
        post(|| async {
            Json(json!({
                "choices": [
                    {
                        "message": {
                            "content": "A photo of an invoice totaling $42."
                        }
                    }
                ]
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let ocr = OpenRouterVisionProvider::new(
        "test-token",
        Some("test-model".into()),
        Some(format!("http://{addr}/api/v1/chat/completions")),
    )
    .unwrap();

    let res = ocr
        .extract_image_file("receipt.png", "png", b"fake-png-bytes")
        .await
        .unwrap();

    assert_eq!(
        res.content,
        "[Image: receipt.png]\n\nA photo of an invoice totaling $42."
    );
    assert!(res.used_ocr);
    assert_eq!(res.file_type, "image");
}

#[tokio::test]
async fn test_layout_chain_fallthrough() {
    // Mock server where sidecar returns 500 error, mistral returns 200 OK
    let app = Router::new()
        .route("/sidecar/extract", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route(
            "/mistral/v1/ocr",
            post(|| async {
                Json(json!({
                    "pages": [
                        {
                            "index": 0,
                            "markdown": "Success from Mistral fallback",
                            "images": []
                        }
                    ]
                }))
            }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let sidecar = MineruExtractor::new(
        format!("http://{addr}/sidecar"),
        Some(Duration::from_secs(2)),
    )
    .unwrap();

    let mistral = MistralOcrExtractor::new(
        "test-token",
        Some(format!("http://{addr}/mistral")),
        Some("mistral-ocr-latest".into()),
        Some(Duration::from_secs(2)),
    )
    .unwrap();

    let chain = LayoutExtractorChain::new(
        vec![
            ("sidecar".to_string(), Arc::new(sidecar)),
            ("mistral".to_string(), Arc::new(mistral)),
        ],
        None,
    );

    let res = chain
        .extract_scanned_pdf("doc.pdf", b"pdf-bytes", ExtractionOptions::default())
        .await
        .unwrap();

    assert_eq!(res.content, "Success from Mistral fallback");
}

struct FixedFallback;

#[async_trait::async_trait]
impl Extractor for FixedFallback {
    async fn extract(
        &self,
        _filename: &str,
        _mime_type: &str,
        _file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        Ok(ExtractionResult::text("Fallback OCR output", "pdf"))
    }
}

#[tokio::test]
async fn test_smart_router_decision() {
    let Ok(_pdfium) = get_pdfium() else {
        eprintln!("Skipping pdfium test: libpdfium not loaded");
        return;
    };

    let text_layer = Arc::new(PdfiumTextExtractor);
    let fallback = Arc::new(FixedFallback);

    // Default opts: min_chars_per_page = 300. Hello world has 10 chars -> routes to fallback
    let default_router = SmartRouterExtractor::new(text_layer.clone(), fallback.clone(), None);
    let res_default = default_router
        .extract("hello.pdf", "application/pdf", MINIMAL_PDF_HELLO_WORLD)
        .await
        .unwrap();
    assert_eq!(res_default.content, "Fallback OCR output");

    // Tuned opts: min_chars_per_page = 5. Hello world has 10 chars -> routes to text layer
    let opts = kb_core::signals::RouterOpts {
        min_chars_per_page: 5,
        ..Default::default()
    };
    let router = SmartRouterExtractor::new(text_layer, fallback, Some(opts));

    let res1 = router
        .extract("hello.pdf", "application/pdf", MINIMAL_PDF_HELLO_WORLD)
        .await
        .unwrap();
    assert!(res1.content.contains("Hello World"));

    // Blank PDF has 0 chars (< 5) -> routes to fallback
    let res2 = router
        .extract("blank.pdf", "application/pdf", MINIMAL_BLANK_PDF)
        .await
        .unwrap();
    assert_eq!(res2.content, "Fallback OCR output");
}
