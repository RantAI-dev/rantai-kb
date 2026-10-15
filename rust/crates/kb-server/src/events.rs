//! In-process progress fan-out for `GET /v1/events`.
//!
//! The TypeScript service keeps a `Map<tenantId, Set<subscriber>>` and pushes
//! one pre-rendered frame per subscriber (`src/service/events.ts`). A Rust
//! process that also runs the ingest worker has no such map to push into — the
//! worker is a task, not a callback — so the port the worker writes to is a
//! [`ProgressSink`] and the HTTP stream subscribes to the same channel.
//!
//! `tokio::sync::broadcast` is what makes "a slow subscriber must not block the
//! worker" true: `send` hands the frame to the channel and returns, and a
//! subscriber that stops reading lags and is skipped rather than awaited.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::response::Response;
use futures::StreamExt;
use kb_ports::{PortError, ProgressSink, TenantId};
use tokio::sync::broadcast;

use crate::auth::Authed;

/// Frames a subscriber may miss before the channel starts dropping for it.
const CHANNEL_CAPACITY: usize = 1024;

/// One rendered server-sent-event frame, already tagged with its tenant.
#[derive(Debug, Clone)]
pub struct EventFrame {
    /// Tenant the frame belongs to; subscribers of other tenants never see it.
    pub tenant_id: TenantId,
    /// The complete frame: `event: <name>\ndata: <json>\n\n`.
    pub text: String,
}

/// Process-wide event bus shared by the worker and the SSE route.
#[derive(Debug)]
pub struct EventBus {
    tx: broadcast::Sender<EventFrame>,
}

impl EventBus {
    /// Create a bus with the default capacity.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_capacity(CHANNEL_CAPACITY)
    }

    /// Create a bus holding up to `capacity` frames before subscribers start
    /// losing the oldest.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self { tx })
    }

    /// Subscribe to every frame this process emits, for any tenant.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<EventFrame> {
        self.tx.subscribe()
    }

    /// Render and publish one frame. Never fails and never blocks: with no
    /// subscribers the send is dropped, and a subscriber that cannot keep up is
    /// lagged by the channel rather than holding the worker.
    fn publish(&self, tenant_id: &TenantId, event: &str, payload: &serde_json::Value) {
        let text = format!("event: {event}\ndata: {payload}\n\n");
        let _ = self.tx.send(EventFrame {
            tenant_id: tenant_id.clone(),
            text,
        });
    }
}

#[async_trait]
impl ProgressSink for EventBus {
    async fn emit(
        &self,
        tenant_id: &TenantId,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), PortError> {
        self.publish(tenant_id, event, &payload);
        Ok(())
    }
}

/// `GET /v1/events` — server-sent ingest progress for the caller's tenant.
///
/// The wire format is the TypeScript one byte for byte: an immediate
/// `": connected\n\n"`, a `": ping\n\n"` keep-alive every 25 s, and one
/// `event: <name>\ndata: <json>\n\n` frame per progress update. Only this
/// tenant's frames are written; another tenant's are dropped without waking
/// the connection's writer.
pub async fn events_stream(
    State(state): State<Arc<crate::state::AppState>>,
    Authed(auth): Authed,
) -> Response {
    let mut receiver = state.events.subscribe();
    // A tokio interval fires immediately on its first tick; the TypeScript
    // `setInterval` waits a full period, and a ping on frame one would be a
    // frame no client expects.
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.reset();

    // `: connected\n\n` goes out before anything else, so a client can tell
    // an open stream from a dead one without waiting for a job.
    let tenant = auth.tenant_id.clone();
    let connected = futures::stream::iter(vec![Ok::<axum::body::Bytes, std::convert::Infallible>(
        axum::body::Bytes::from_static(CONNECTED_FRAME),
    )]);
    let stream = connected.chain(futures::stream::unfold(
        (receiver, ping, tenant),
        move |(mut receiver, mut ping, tenant)| async move {
            loop {
                tokio::select! {
                    frame = receiver.recv() => match frame {
                        Ok(frame) if frame.tenant_id == tenant => {
                            return Some((Ok::<_, std::convert::Infallible>(
                                axum::body::Bytes::from(frame.text),
                            ), (receiver, ping, tenant)));
                        }
                        // Another tenant's frame, or a lagged subscriber that
                        // missed frames. Neither ends the stream.
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                            tracing::debug!(missed, "sse subscriber lagged");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    },
                    _ = ping.tick() => {
                        return Some((
                            Ok(axum::body::Bytes::from_static(PING_FRAME)),
                            (receiver, ping, tenant),
                        ));
                    }
                }
            }
        },
    ));

    let body = axum::body::Body::from_stream(stream);
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        axum::http::header::CONNECTION,
        axum::http::HeaderValue::from_static("keep-alive"),
    );
    response
}

/// The first frame every subscriber receives.
pub const CONNECTED_FRAME: &[u8] = b": connected\n\n";

/// The keep-alive frame, written every [`PING_INTERVAL`].
pub const PING_FRAME: &[u8] = b": ping\n\n";

/// Keep-alive period, matching `setInterval(..., 25_000)` in the TypeScript
/// route.
pub const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(25);
