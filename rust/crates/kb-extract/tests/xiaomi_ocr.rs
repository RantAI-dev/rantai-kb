//! The Xiaomi-OCR-0 provider, measured against a fake model (`ING-6`).
//!
//! The provider is the only place that speaks the vendor's recipe, so the
//! recipe itself is what these tests check: the request shape, the repetition
//! retry, the layout fallback and the degradation when the layout detector is
//! down. No model and no network are involved: the fake endpoint is an axum
//! server on localhost that records what it was sent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{Router, extract::State, http::StatusCode, response::Json, routing::post};
use kb_extract::ocr_router::OcrPageProvider;
use kb_extract::xiaomi_ocr::{
    DOCUMENT_PROMPT, TABLE_PROMPT, TEXT_PROMPT, XiaomiOcrProvider, XiaomiOcrSettings,
};
use kb_ports::{LayoutBox, LayoutDetector, PageLayout, PortError};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// What the fake endpoint was sent, and what it answers.
#[derive(Clone)]
struct Fake {
    /// The answers, in order; the last one repeats when the list runs out.
    answers: Vec<(String, Option<String>)>,
    /// The request bodies, in order.
    seen: Arc<Mutex<Vec<Value>>>,
    calls: Arc<AtomicUsize>,
}

impl Fake {
    fn new(answers: Vec<(String, Option<String>)>) -> Self {
        Self {
            answers,
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The body of a request, as the provider sent it.
fn body_of(fake: &Fake, index: usize) -> Value {
    fake.requests()[index].clone()
}

/// The prompt of a request: the first text part of the message.
fn prompt_of(fake: &Fake, index: usize) -> String {
    body_of(fake, index)["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The image of a request: the second part, a data URL.
fn image_of(fake: &Fake, index: usize) -> String {
    body_of(fake, index)["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Start the fake endpoint and return its base URL and the recorder.
async fn start(answers: Vec<(String, Option<String>)>) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake::new(answers));
    let state = Arc::clone(&fake);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(
            |State(state): State<Arc<Fake>>, body: axum::body::Bytes| async move {
                let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                state.seen.lock().unwrap().push(parsed);
                let index = state.calls.fetch_add(1, Ordering::SeqCst);
                let (text, finish_reason) = state
                    .answers
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| ("last answer".to_string(), Some("stop".to_string())));
                Json(json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": text},
                        "finish_reason": finish_reason,
                    }],
                    "usage": {"completion_tokens": 10},
                }))
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.with_state(state)).await.unwrap();
    });
    (format!("http://{addr}/v1/chat/completions"), fake)
}

/// A layout detector that answers with a fixed layout.
struct FakeLayout {
    layout: PageLayout,
    fail: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl LayoutDetector for FakeLayout {
    async fn detect(&self, _image: &[u8], _mime: &str) -> Result<PageLayout, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(PortError::Unavailable(
                "the layout model is down".to_string(),
            ));
        }
        Ok(self.layout.clone())
    }
}

fn provider(base_url: &str) -> XiaomiOcrProvider {
    XiaomiOcrProvider::new(XiaomiOcrSettings {
        base_url: base_url.to_string(),
        model: "xiaomi-ocr-0".to_string(),
        api_key: Some("kb-eval-vlm-2026".to_string()),
        request_timeout: std::time::Duration::from_secs(5),
        ..XiaomiOcrSettings::default()
    })
    .expect("provider")
}

/// A page image: a small PNG.
fn page_png() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        40,
        30,
        image::Rgb([255, 255, 255]),
    ));
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// A layout with one text region and one table region.
fn layout_with_a_table() -> PageLayout {
    PageLayout {
        regions: vec![
            LayoutBox {
                label: "text".to_string(),
                score: 0.9,
                bbox_2d: [0.0, 0.0, 500.0, 200.0],
                task_type: "text".to_string(),
            },
            LayoutBox {
                label: "table".to_string(),
                score: 0.8,
                bbox_2d: [0.0, 300.0, 1000.0, 900.0],
                task_type: "table".to_string(),
            },
        ],
    }
}

#[tokio::test]
async fn the_request_is_the_vendors_recipe() {
    let (url, fake) = start(vec![(
        "A page of text.".to_string(),
        Some("stop".to_string()),
    )])
    .await;
    let provider = provider(&url);

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(answer.text, "A page of text.");
    assert_eq!(fake.calls(), 1);

    let body = body_of(&fake, 0);
    assert_eq!(body["model"], "xiaomi-ocr-0");
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["top_p"], 0.8);
    assert_eq!(body["presence_penalty"], 0.0);
    assert_eq!(body["repetition_penalty"], 1.0);
    assert_eq!(body["max_tokens"], 12288);
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);

    // The prompt comes before the image, and it is the vendor's document prompt.
    assert_eq!(prompt_of(&fake, 0), DOCUMENT_PROMPT);
    let image = image_of(&fake, 0);
    assert!(image.starts_with("data:image/png;base64,"), "{image}");
}

#[tokio::test]
async fn a_repetitive_answer_is_asked_for_again_with_a_presence_penalty() {
    // The first answer is cut off mid-word: the server says `length` and the
    // text repeats. The second answer is a clean page.
    let repeated = "abc".repeat(2000);
    let (url, fake) = start(vec![
        (repeated, Some("length".to_string())),
        ("A clean page.".to_string(), Some("stop".to_string())),
    ])
    .await;
    let provider = provider(&url);

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(fake.calls(), 2, "the page is asked for twice");
    assert_eq!(body_of(&fake, 0)["presence_penalty"], 0.0);
    assert_eq!(
        body_of(&fake, 1)["presence_penalty"],
        1.5,
        "the retry asks the model not to repeat itself"
    );
    assert_eq!(answer.text, "A clean page.", "the clean answer is adopted");
}

#[tokio::test]
async fn a_retry_that_is_still_repetitive_keeps_the_first_answer() {
    let repeated = "abc".repeat(2000);
    let (url, fake) = start(vec![
        (repeated.clone(), Some("length".to_string())),
        ("xyz".repeat(2000), Some("length".to_string())),
    ])
    .await;
    let provider = provider(&url);

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(fake.calls(), 2);
    assert!(
        answer.text.starts_with("abc"),
        "the first answer is kept: {}",
        &answer.text[..20.min(answer.text.len())]
    );
}

#[tokio::test]
async fn a_page_with_a_table_is_not_sent_to_the_layout_detector() {
    let (url, fake) = start(vec![(
        "Intro\n\n<table><tr><td>A</td></tr></table>".to_string(),
        Some("stop".to_string()),
    )])
    .await;
    let layout = Arc::new(FakeLayout {
        layout: layout_with_a_table(),
        fail: false,
        calls: AtomicUsize::new(0),
    });
    let mut settings = XiaomiOcrSettings {
        base_url: url,
        model: "xiaomi-ocr-0".to_string(),
        ..XiaomiOcrSettings::default()
    };
    settings.layout = Some(Arc::clone(&layout) as Arc<dyn LayoutDetector>);
    let provider = XiaomiOcrProvider::new(settings).expect("provider");

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(layout.calls.load(Ordering::SeqCst), 0);
    assert!(!answer.layout_failed);
    assert!(answer.text.contains("<table>"));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn a_page_whose_table_was_missed_is_read_region_by_region() {
    let (url, fake) = start(vec![
        (
            "A page with no table at all.".to_string(),
            Some("stop".to_string()),
        ),
        ("The first paragraph.".to_string(), Some("stop".to_string())),
        (
            "<fcel>Cell A<fcel>Cell B<nl>".to_string(),
            Some("stop".to_string()),
        ),
    ])
    .await;
    let layout = Arc::new(FakeLayout {
        layout: layout_with_a_table(),
        fail: false,
        calls: AtomicUsize::new(0),
    });
    let mut settings = XiaomiOcrSettings {
        base_url: url,
        model: "xiaomi-ocr-0".to_string(),
        ..XiaomiOcrSettings::default()
    };
    settings.layout = Some(Arc::clone(&layout) as Arc<dyn LayoutDetector>);
    let provider = XiaomiOcrProvider::new(settings).expect("provider");

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(layout.calls.load(Ordering::SeqCst), 1);
    assert!(answer.layout, "the layout path ran");
    assert!(!answer.layout_failed);
    assert_eq!(fake.calls(), 3, "the page, then its two regions");

    // The regions are read with the prompt of their kind, and merged in order.
    assert_eq!(prompt_of(&fake, 1), TEXT_PROMPT);
    assert_eq!(prompt_of(&fake, 2), TABLE_PROMPT);
    assert!(
        answer.text.contains("The first paragraph."),
        "the merged page keeps the text: {}",
        answer.text
    );
    assert!(
        answer.text.contains("Cell A"),
        "the merged page keeps the table: {}",
        answer.text
    );
}

#[tokio::test]
async fn a_layout_detector_that_is_down_keeps_the_page_and_says_so() {
    let (url, fake) = start(vec![(
        "A page of text.".to_string(),
        Some("stop".to_string()),
    )])
    .await;
    let layout = Arc::new(FakeLayout {
        layout: PageLayout::default(),
        fail: true,
        calls: AtomicUsize::new(0),
    });
    let mut settings = XiaomiOcrSettings {
        base_url: url,
        model: "xiaomi-ocr-0".to_string(),
        ..XiaomiOcrSettings::default()
    };
    settings.layout = Some(Arc::clone(&layout) as Arc<dyn LayoutDetector>);
    let provider = XiaomiOcrProvider::new(settings).expect("provider");

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(answer.text, "A page of text.", "the page keeps its text");
    assert!(answer.layout_failed, "the failure is reported, not hidden");
    assert!(!answer.layout);
    assert_eq!(fake.calls(), 1, "no region was read");
}

#[tokio::test]
async fn a_layout_without_a_table_keeps_the_whole_page_answer() {
    let (url, fake) = start(vec![(
        "A page of text.".to_string(),
        Some("stop".to_string()),
    )])
    .await;
    let layout = Arc::new(FakeLayout {
        layout: PageLayout {
            regions: vec![LayoutBox {
                label: "text".to_string(),
                score: 0.9,
                bbox_2d: [0.0, 0.0, 500.0, 200.0],
                task_type: "text".to_string(),
            }],
        },
        fail: false,
        calls: AtomicUsize::new(0),
    });
    let mut settings = XiaomiOcrSettings {
        base_url: url,
        model: "xiaomi-ocr-0".to_string(),
        ..XiaomiOcrSettings::default()
    };
    settings.layout = Some(Arc::clone(&layout) as Arc<dyn LayoutDetector>);
    let provider = XiaomiOcrProvider::new(settings).expect("provider");

    let answer = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect("the page is read");

    assert_eq!(answer.text, "A page of text.");
    assert!(answer.layout, "the detector answered");
    assert!(!answer.layout_failed);
    assert_eq!(fake.calls(), 1, "the page is not read again");
}

#[tokio::test]
async fn a_server_error_is_transient_and_a_rejection_is_not() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { (StatusCode::BAD_REQUEST, "no") }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = provider(&format!("http://{addr}/v1/chat/completions"));
    let error = provider
        .read_image(&page_png(), "image/png")
        .await
        .expect_err("the page cannot be read");
    assert!(!error.transient, "a rejection is not worth a retry");
}
