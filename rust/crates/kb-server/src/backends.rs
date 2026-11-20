//! The state of the model backends, as `GET /v1/health` reports it (`#36`).
//!
//! A client that uploads a scanned document needs to know whether the OCR
//! backend can serve it *before* it uploads: the report used to say only
//! whether Postgres and the vector store answered, so a client could not tell
//! "the OCR model is still loading" from "the OCR container is stopped".
//!
//! Three states, one per backend:
//!
//! * `ready` — the backend answered; it can serve requests.
//! * `starting` — the backend is up but not ready yet (a model that is still
//!   loading answers `503`).
//! * `unavailable` — the backend did not answer, or answered that it cannot
//!   serve. A backend that is not configured at all is `unavailable` too:
//!   there is nothing to wait for.
//!
//! The probe is a `GET` on the endpoint the configuration names. It never
//! reads the body and never sends a prompt, so it costs nothing on a paid
//! provider and it never runs a model.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::config::Config;

/// How long one backend probe may take.
///
/// A health endpoint that hangs is worse than one that answers "I do not
/// know": the probe gives up and says `unavailable`.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long a probe result is reused before the backends are probed again.
///
/// Health checks are polled every few seconds by an orchestrator, and each
/// probe is an outbound request to a model server. Ten seconds is short
/// enough that an operator sees a restarted OCR container come back, and long
/// enough that a poll loop does not hammer the provider.
pub const PROBE_TTL: Duration = Duration::from_secs(10);

/// How usable a backend is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackendState {
    /// The backend answered: it can serve requests.
    Ready,
    /// The backend is up but not ready: the model is still loading.
    Starting,
    /// The backend did not answer, or answered that it cannot serve.
    Unavailable,
}

impl BackendState {
    /// The state as the API spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Starting => "starting",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One backend: its state, and why it is in that state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BackendStatus {
    /// `ready`, `starting` or `unavailable`.
    pub state: BackendState,
    /// Why: the HTTP status the backend answered, or the error it gave.
    ///
    /// A plain string, not a machine-readable code: this is what an operator
    /// reads, and the state field is what a client switches on.
    pub detail: String,
}

impl BackendStatus {
    /// A backend in a state, with a reason.
    #[must_use]
    pub fn new(state: BackendState, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }
}

/// The three backends a client can wait for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BackendHealth {
    /// The embedder: without it nothing can be indexed or searched.
    pub embedder: BackendStatus,
    /// The reranker: without it search still works, only less well.
    pub reranker: BackendStatus,
    /// The OCR backend: without it a scanned document cannot be read.
    pub ocr: BackendStatus,
}

/// Where a backend lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The URL the configuration names.
    pub url: String,
    /// The bearer token, when the endpoint wants one.
    pub api_key: Option<String>,
}

impl Endpoint {
    /// An endpoint with no authentication.
    #[must_use]
    pub fn anonymous(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            api_key: None,
        }
    }

    /// An endpoint with a bearer token.
    #[must_use]
    pub fn with_key(url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        Self {
            url: url.into(),
            api_key: if api_key.trim().is_empty() {
                None
            } else {
                Some(api_key)
            },
        }
    }
}

/// The endpoints the health route probes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendTargets {
    /// The embedder's endpoint.
    pub embedder: Option<Endpoint>,
    /// The reranker's endpoint. `None` when the reranker is disabled.
    pub reranker: Option<Endpoint>,
    /// The OCR backend's endpoint. `None` when no OCR provider is configured.
    pub ocr: Option<Endpoint>,
}

impl BackendTargets {
    /// The endpoints a resolved configuration describes.
    ///
    /// The OCR backend is the local OpenAI-compatible endpoint when one is
    /// configured, and Mistral otherwise: those are the two providers the
    /// extraction path can use.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let reranker = match &config.reranker {
            crate::config::RerankerConfig::Disabled => None,
            crate::config::RerankerConfig::Llm { api_key, .. } => Some(Endpoint::with_key(
                kb_inference::OPENROUTER_CHAT_URL,
                api_key.clone(),
            )),
            crate::config::RerankerConfig::Tei { base_url, .. }
            | crate::config::RerankerConfig::Vllm { base_url, .. } => Some(Endpoint::anonymous(
                format!("{}/rerank", base_url.trim_end_matches('/')),
            )),
            crate::config::RerankerConfig::Cohere {
                api_key, endpoint, ..
            } => Some(Endpoint::with_key(
                endpoint
                    .clone()
                    .unwrap_or_else(|| kb_inference::DEFAULT_COHERE_ENDPOINT.to_string()),
                api_key.clone(),
            )),
        };

        let ocr = if !config.extract.ocr_base_url.trim().is_empty() {
            Some(Endpoint::with_key(
                config.extract.ocr_base_url.clone(),
                config.extract.ocr_api_key.clone(),
            ))
        } else if !config.extract.mistral_ocr_key.trim().is_empty() {
            Some(Endpoint::with_key(
                format!(
                    "{}/v1/chat/completions",
                    config.extract.mistral_ocr_base.trim_end_matches('/')
                ),
                config.extract.mistral_ocr_key.clone(),
            ))
        } else {
            None
        };

        Self {
            embedder: Some(Endpoint::with_key(
                config.embedder.base_url.clone(),
                config.embedder.api_key.clone().unwrap_or_default(),
            )),
            reranker,
            ocr,
        }
    }
}

/// The backends a health check probes, with a short cache.
pub struct BackendProbe {
    targets: BackendTargets,
    /// The last result and when it was taken. `Mutex` (not an async lock):
    /// the lock is held for a copy, never across an await.
    cached: Mutex<Option<(Instant, BackendHealth)>>,
}

impl Default for BackendProbe {
    fn default() -> Self {
        Self::new(BackendTargets::default())
    }
}

impl BackendProbe {
    /// Probe the given endpoints.
    #[must_use]
    pub fn new(targets: BackendTargets) -> Self {
        Self {
            targets,
            cached: Mutex::new(None),
        }
    }

    /// The endpoints this probe watches.
    #[must_use]
    pub fn targets(&self) -> &BackendTargets {
        &self.targets
    }

    /// The state of every backend, probing the ones whose answer is stale.
    pub async fn health(&self) -> BackendHealth {
        if let Some((taken, health)) = self.cached.lock().unwrap().clone() {
            if taken.elapsed() < PROBE_TTL {
                return health;
            }
        }
        let health = self.targets.probe().await;
        *self.cached.lock().unwrap() = Some((Instant::now(), health.clone()));
        health
    }
}

impl BackendTargets {
    /// Probe every backend at once.
    ///
    /// The three probes run together: the health endpoint must not take three
    /// timeouts to answer.
    pub async fn probe(&self) -> BackendHealth {
        let (embedder, reranker, ocr) = tokio::join!(
            probe_endpoint(self.embedder.as_ref()),
            probe_endpoint(self.reranker.as_ref()),
            probe_endpoint(self.ocr.as_ref()),
        );
        BackendHealth {
            embedder,
            reranker,
            ocr,
        }
    }
}

/// Probe one backend.
///
/// A `GET` on the endpoint: the server answers, or it does not. The status
/// says which.
async fn probe_endpoint(endpoint: Option<&Endpoint>) -> BackendStatus {
    let Some(endpoint) = endpoint else {
        return BackendStatus::new(BackendState::Unavailable, "not configured");
    };
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return BackendStatus::new(BackendState::Unavailable, format!("client: {err}"));
        }
    };
    let mut request = client.get(&endpoint.url);
    if let Some(key) = &endpoint.api_key {
        request = request.bearer_auth(key);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let detail = format!("HTTP {status}");
            let state = if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED {
                // The endpoint answered. A `405` is what an OpenAI-compatible
                // server says to a `GET` on a `POST` route: it is up and it
                // knows the route.
                BackendState::Ready
            } else if status == StatusCode::SERVICE_UNAVAILABLE {
                // vLLM and its relatives answer `503` while the model loads.
                BackendState::Starting
            } else {
                BackendState::Unavailable
            };
            BackendStatus::new(state, detail)
        }
        Err(err) => BackendStatus::new(BackendState::Unavailable, err.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A one-line HTTP server that answers every request with `status`.
    ///
    /// The probe only looks at the status, so the server does nothing else.
    async fn serve_status(status: u16) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                // Read the request head, so the client is not still writing
                // when the answer goes out.
                loop {
                    let read = match socket.read(&mut buf).await {
                        Ok(read) => read,
                        Err(_) => break,
                    };
                    if read == 0 || buf.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    503 => "Service Unavailable",
                    _ => "Status",
                };
                let answer = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        format!("http://{addr}/v1/models")
    }

    #[tokio::test]
    async fn a_backend_that_is_not_configured_is_unavailable() {
        let status = probe_endpoint(None).await;
        assert_eq!(status.state, BackendState::Unavailable);
        assert_eq!(status.detail, "not configured");
    }

    #[tokio::test]
    async fn a_backend_that_answers_is_ready() {
        let url = serve_status(200).await;
        let status = probe_endpoint(Some(&Endpoint::anonymous(url))).await;
        assert_eq!(status.state, BackendState::Ready);
    }

    #[tokio::test]
    async fn a_backend_that_says_it_is_not_ready_is_starting() {
        let url = serve_status(503).await;
        let status = probe_endpoint(Some(&Endpoint::anonymous(url))).await;
        assert_eq!(status.state, BackendState::Starting);
    }

    #[tokio::test]
    async fn a_backend_that_refuses_the_request_is_unavailable() {
        let url = serve_status(401).await;
        let status = probe_endpoint(Some(&Endpoint::anonymous(url))).await;
        assert_eq!(status.state, BackendState::Unavailable);
    }

    #[tokio::test]
    async fn a_backend_that_is_not_there_is_unavailable() {
        // Port 1 on the loopback interface: nothing listens there.
        let status =
            probe_endpoint(Some(&Endpoint::anonymous("http://127.0.0.1:1/v1/models"))).await;
        assert_eq!(status.state, BackendState::Unavailable);
    }

    #[tokio::test]
    async fn the_state_is_spelled_the_way_a_client_reads_it() {
        assert_eq!(BackendState::Ready.as_str(), "ready");
        assert_eq!(BackendState::Starting.as_str(), "starting");
        assert_eq!(BackendState::Unavailable.as_str(), "unavailable");
        let json = serde_json::to_value(BackendStatus::new(BackendState::Starting, "HTTP 503"))
            .expect("json");
        assert_eq!(json["state"], "starting");
        assert_eq!(json["detail"], "HTTP 503");
    }
}
