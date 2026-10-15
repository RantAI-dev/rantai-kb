//! Process behaviour: the ingest worker and reaper run in the HTTP process,
//! and a shutdown stops the listener before the worker, between jobs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use std::sync::Arc;
use std::time::Duration;

use kb_engine::fakes::{
    FakeBlobStore, FakeDocumentStore, FakeEmbedder, FakeExtractor, FakeJobStore,
};
use kb_engine::{CancelToken, IngestSettings, IngestWorker};
use kb_ports::{BlobStore, JobRecord, TenantId};
use kb_server::runtime::{Maintenance, serve};
use tokio::net::TcpListener;

const TENANT: &str = "tenant-a";

/// A worker over fakes, with a poll interval short enough for a test and a
/// heartbeat long enough never to fire.
async fn test_worker() -> (IngestWorker, FakeJobStore, FakeDocumentStore, FakeBlobStore) {
    let jobs = FakeJobStore::default();
    let documents = FakeDocumentStore::default();
    let blobs = FakeBlobStore::default();
    let chunks = kb_engine::fakes::FakeChunkStore::default();

    let tenant = TenantId(TENANT.to_string());
    let document_id = kb_ports::DocumentId("doc-1".to_string());
    let mut doc = harness::doc_meta("doc-1", "Queued");
    doc.s3_key = Some("documents/tenant-a/doc-1/notes.txt".to_string());
    documents.insert_doc(tenant.clone(), doc);
    blobs
        .upload(
            "documents/tenant-a/doc-1/notes.txt",
            b"hello from the queue".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    jobs.push(JobRecord {
        id: "job-1".into(),
        tenant_id: tenant,
        user_id: None,
        document_id: Some(document_id),
        s3_key: Some("documents/tenant-a/doc-1/notes.txt".into()),
        filename: "notes.txt".into(),
        mime_type: Some("text/plain".into()),
        attempt: 1,
        params: None,
    });

    let settings = IngestSettings {
        claims_per_tick: 1,
        poll_interval: Duration::from_millis(20),
        heartbeat_interval: Duration::from_secs(3600),
        ..IngestSettings::default()
    };
    let worker = IngestWorker::new(
        Arc::new(jobs.clone()),
        Arc::new(documents.clone()),
        Arc::new(chunks),
        Arc::new(blobs.clone()),
        Arc::new(FakeExtractor::returning("extracted text", "text")),
        Arc::new(FakeEmbedder::default()),
        settings,
    );
    (worker, jobs, documents, blobs)
}

#[tokio::test]
async fn serve_stops_the_listener_and_then_the_worker_on_cancel() {
    let (worker, jobs, documents, blobs) = test_worker().await;
    let harness = harness::Harness::new();
    let app = harness.app.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancelToken::new();

    let maintenance = Maintenance {
        jobs: Arc::new(jobs.clone()),
        blobs: Arc::new(blobs.clone()),
        reap_days: 7,
        reap_interval: Duration::from_millis(50),
        reap_batch: 100,
    };

    let server = tokio::spawn(serve(
        app,
        listener,
        Some(worker),
        Some(maintenance),
        cancel.clone(),
    ));

    // The process answers HTTP while it also runs the worker.
    let body = reqwest_get(addr).await;
    assert!(body.contains("\"ok\""), "health body was {body}");

    // The worker claimed and finished the queued job.
    let finished = wait_for(|| !jobs.finished.read().unwrap().is_empty()).await;
    assert!(finished, "the worker never finished the queued job");

    let _ = cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returned after the cancel")
        .expect("no join error");

    // Cancelling again is harmless, and the documents the worker wrote stayed.
    cancel.cancel();

    let docs = documents.docs.read().unwrap();
    assert_eq!(docs[&TenantId(TENANT.to_string())].len(), 1);
}

#[tokio::test]
async fn serve_works_without_a_worker() {
    let harness = harness::Harness::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancelToken::new();
    let server = tokio::spawn(serve(
        harness.app.clone(),
        listener,
        None,
        None,
        cancel.clone(),
    ));

    let body = reqwest_get(addr).await;
    assert!(body.contains("\"ok\""));
    let _ = cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returned")
        .expect("no join error");
}

#[tokio::test]
async fn reap_once_deletes_the_stored_file_of_a_failed_job_and_clears_its_key() {
    let (worker, jobs, _documents, blobs) = test_worker().await;
    drop(worker);
    blobs
        .upload(
            "documents/tenant-a/doc-1/orphan.txt",
            b"orphan".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    jobs.reapables.write().unwrap().push(kb_ports::ReapableJob {
        id: "job-orphan".into(),
        s3_key: "documents/tenant-a/doc-1/orphan.txt".into(),
    });

    let maintenance = Maintenance {
        jobs: Arc::new(jobs.clone()),
        blobs: Arc::new(blobs.clone()),
        reap_days: 7,
        reap_interval: Duration::from_secs(3600),
        reap_batch: 100,
    };
    maintenance.reap_once().await;

    assert!(
        !blobs
            .objects
            .read()
            .unwrap()
            .contains_key("documents/tenant-a/doc-1/orphan.txt")
    );
    assert!(jobs.reapables.read().unwrap().is_empty());
}

#[tokio::test]
async fn a_slow_subscriber_never_blocks_the_worker() {
    // The bus is a broadcast: emitting with no subscriber, or with one that
    // never reads, returns immediately instead of awaiting the reader.
    let bus = kb_server::EventBus::with_capacity(4);
    let tenant = TenantId(TENANT.to_string());
    let mut stalled = bus.subscribe();
    for i in 0..32 {
        kb_ports::ProgressSink::emit(
            &*bus,
            &tenant,
            "ingest:job:update",
            serde_json::json!({"i": i}),
        )
        .await
        .unwrap();
    }
    // The stalled receiver lagged instead of applying back-pressure; draining
    // it must not panic and must not have blocked any of the emits.
    let _ = stalled.try_recv();
}

async fn wait_for<F: Fn() -> bool>(condition: F) -> bool {
    for _ in 0..200 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Minimal HTTP GET: the tests only need the status line and body, so this
/// avoids a client dependency.
async fn reqwest_get(addr: std::net::SocketAddr) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).to_string()
}
