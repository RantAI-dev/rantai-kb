//! The layout detector's HTTP client: where a page's regions are.
//!
//! The detector is a model, so it is a service like an OCR provider: it can be
//! down, slow, or wrong, and the caller has to degrade visibly rather than
//! pretend the page had no regions. This adapter is the client for the
//! sidecar in `rust/deploy/ocr-layout`:
//!
//! ```text
//! POST {base}/v1/layout   {"image": "<base64 png>", "mime": "image/png"}
//!   -> {"regions": [{"label": "table", "score": 0.94,
//!                    "bbox_2d": [x1, y1, x2, y2], "task_type": "table"}]}
//! GET  {base}/health      -> {"status": "ok", …}
//! ```
//!
//! The boxes come back normalised to 0–1000, so they travel with the page's
//! own resolution: the region a detector saw in a 150 dpi page is the same
//! region in a 300 dpi page.
//!
//! This crate is an inner one, so it never names an adapter: the detector
//! itself is a [`kb_ports::LayoutDetector`], and the composition root decides
//! which implementation is bound.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use kb_ports::{LayoutDetector, PageLayout, PortError};
use serde::Deserialize;
use serde_json::json;

use crate::ExtractError;

/// How long a layout call may take before it is treated as a failure.
///
/// A layout model on a busy GPU answers in a second or two; a minute is
/// already a stall, and a page that waits a minute for a layout the OCR
/// provider could have read whole is a page that never gets read.
pub const DEFAULT_LAYOUT_TIMEOUT: Duration = Duration::from_secs(60);

/// The layout detector, reached over HTTP.
#[derive(Debug, Clone)]
pub struct HttpLayoutDetector {
    client: reqwest::Client,
    /// The base URL of the sidecar, without a trailing slash.
    base_url: String,
    /// The URL of the layout endpoint.
    url: String,
}

impl HttpLayoutDetector {
    /// Build a detector for the sidecar at `base_url`.
    ///
    /// # Errors
    ///
    /// When the base URL is empty or the HTTP client cannot be built.
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, ExtractError> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(ExtractError::Unsupported(
                "the layout detector requires KB_OCR_LAYOUT_URL — the base URL of \
                 the layout sidecar"
                    .to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("layout HTTP client failed: {e}")))?;
        let url = format!("{base_url}/v1/layout");
        Ok(Self {
            client,
            base_url,
            url,
        })
    }

    /// The sidecar this detector talks to.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The health endpoint of the sidecar.
    #[must_use]
    pub fn health_url(&self) -> String {
        format!("{}/health", self.base_url)
    }
}

#[async_trait]
impl LayoutDetector for HttpLayoutDetector {
    async fn detect(&self, image_bytes: &[u8], mime: &str) -> Result<PageLayout, PortError> {
        if image_bytes.is_empty() {
            return Err(PortError::InvalidArgument(
                "the layout detector was given an empty page".to_string(),
            ));
        }
        let image = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let body = json!({"image": image, "mime": mime});
        let response = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap_or_default())
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("layout request failed: {e}")))?;

        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| PortError::Unavailable(format!("layout response body: {e}")))?;
        if !(200..300).contains(&status) {
            let message: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
            return Err(PortError::Unavailable(format!(
                "layout http {status}: {message}"
            )));
        }
        let parsed: LayoutResponse = serde_json::from_slice(&bytes).map_err(|e| {
            PortError::Malformed(format!("layout response is not the expected JSON: {e}"))
        })?;
        Ok(PageLayout {
            regions: parsed.regions,
        })
    }
}

/// The sidecar's answer.
#[derive(Debug, Deserialize)]
struct LayoutResponse {
    #[serde(default)]
    regions: Vec<kb_ports::LayoutBox>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_url_is_refused() {
        assert!(HttpLayoutDetector::new("  ", DEFAULT_LAYOUT_TIMEOUT).is_err());
    }

    #[test]
    fn the_health_url_is_the_sidecar_root() {
        let detector = HttpLayoutDetector::new("http://127.0.0.1:8193/", DEFAULT_LAYOUT_TIMEOUT)
            .expect("detector");
        assert_eq!(detector.health_url(), "http://127.0.0.1:8193/health");
        assert_eq!(detector.base_url(), "http://127.0.0.1:8193");
    }

    #[tokio::test]
    async fn an_empty_page_is_refused_before_a_request() {
        let detector = HttpLayoutDetector::new("http://127.0.0.1:8193", DEFAULT_LAYOUT_TIMEOUT)
            .expect("detector");
        let error = detector.detect(b"", "image/png").await.expect_err("error");
        assert!(matches!(error, PortError::InvalidArgument(_)));
    }
}
