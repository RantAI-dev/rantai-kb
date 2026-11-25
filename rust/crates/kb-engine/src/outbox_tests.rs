//! SEC-2b unit tests: the reconciler against in-memory ports.
//!
//! Every assertion here is on observable port state — the payload the index
//! holds, the rows left in the outbox, and the documents the index deleted —
//! never on a private counter the reconciler updates itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use kb_ports::{DocumentId, OutboxOp, ScopeOutbox, TenantId};

use crate::fakes::{FakeChunkPayload, FakeChunkStore, FakeDocumentStore, FakeScopeOutbox};
use crate::{CancelToken, OutboxReconciler};

const TENANT: &str = "tenant-a";
const DOC: &str = "doc-d";

fn document() -> kb_ports::DocumentMeta {
    kb_ports::DocumentMeta {
        sha256: None,
        id: DocumentId(DOC.to_string()),
        title: DOC.to_string(),
        categories: Vec::new(),
        subcategory: None,
        status: "ready".to_string(),
        external_ref: None,
        chunk_count: 1,
        file_type: None,
        file_size: None,
        mime_type: None,
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: vec!["kb-1".to_string()],
        metadata: None,
    }
}

struct Fixture {
    outbox: Arc<FakeScopeOutbox>,
    documents: Arc<FakeDocumentStore>,
    index: Arc<FakeChunkStore>,
}

fn fixture() -> Fixture {
    let outbox = Arc::new(FakeScopeOutbox::default());
    let documents = Arc::new(FakeDocumentStore::default());
    documents.insert_doc(TenantId(TENANT.to_string()), document());
    let index = Arc::new(FakeChunkStore::default());
    index.set_payload(
        &DocumentId(DOC.to_string()),
        FakeChunkPayload {
            tenant: Some(TENANT.to_string()),
            knowledge_base_ids: vec!["kb-1".to_string()],
            category: None,
        },
    );
    Fixture {
        outbox,
        documents,
        index,
    }
}

fn reconciler(f: &Fixture) -> OutboxReconciler {
    OutboxReconciler::new(f.outbox.clone(), f.documents.clone(), f.index.clone())
        .with_batch(10)
        .with_interval(Duration::from_millis(1))
}

fn move_to_kb2(f: &Fixture) {
    let mut docs = f.documents.docs.write().unwrap();
    let list = docs.get_mut(&TenantId(TENANT.to_string())).unwrap();
    list[0].knowledge_base_ids = vec!["kb-2".to_string()];
}

#[tokio::test]
async fn a_kb_move_gets_its_payload_fixed() {
    let f = fixture();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    );
    move_to_kb2(&f);

    let report = reconciler(&f).reconcile_once().await.unwrap();

    assert_eq!(report.claimed, 1);
    assert_eq!(report.repaired, 1);
    assert_eq!(report.failed, 0);
    assert!(
        f.outbox.rows.read().unwrap().is_empty(),
        "row must be completed"
    );
    let writes = f.index.kb_payload_writes.read().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0.as_str(), DOC);
    assert_eq!(writes[0].1, vec!["kb-2".to_string()]);
    let payloads = f.index.payloads.read().unwrap();
    assert_eq!(
        payloads[&DocumentId(DOC.to_string())].knowledge_base_ids,
        vec!["kb-2".to_string()]
    );
}

#[tokio::test]
async fn a_failed_repair_is_retried_instead_of_completed() {
    let f = fixture();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    );
    f.index
        .fail_payload
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let first = reconciler(&f).reconcile_once().await.unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.repaired, 0);
    assert_eq!(f.outbox.rows.read().unwrap().len(), 1, "row must survive");
    assert!(
        f.outbox.errors()[0]
            .as_deref()
            .is_some_and(|e| !e.is_empty())
    );

    // The retry re-reads Postgres, not the failed payload: the document moved
    // while the first attempt failed, and the second attempt sees the new KB.
    move_to_kb2(&f);
    f.index
        .fail_payload
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let second = reconciler(&f).reconcile_once().await.unwrap();
    assert_eq!(second.repaired, 1);
    assert!(f.outbox.rows.read().unwrap().is_empty());
    let writes = f.index.kb_payload_writes.read().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].1, vec!["kb-2".to_string()]);
}

#[tokio::test]
async fn a_deleted_documents_points_are_removed() {
    let f = fixture();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::Deleted,
    );

    let report = reconciler(&f).reconcile_once().await.unwrap();

    assert_eq!(report.deleted, 1);
    assert_eq!(report.repaired, 0);
    assert_eq!(
        f.index.deleted_documents.read().unwrap().as_slice(),
        &[DocumentId(DOC.to_string())]
    );
    assert!(f.outbox.rows.read().unwrap().is_empty());
}

#[tokio::test]
async fn a_row_at_the_retry_ceiling_is_left_for_an_operator() {
    let f = fixture();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    );
    f.outbox.rows.write().unwrap()[0].item.attempts = 9;
    f.index
        .fail_payload
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let report = reconciler(&f)
        .with_max_attempts(10)
        .reconcile_once()
        .await
        .unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.abandoned, 1);
    assert_eq!(f.outbox.rows.read().unwrap().len(), 1);
    let again = f.outbox.claim_batch(10).await.unwrap();
    assert!(
        again.is_empty(),
        "a row at the ceiling must not be claimed again"
    );
}

#[tokio::test]
async fn a_kb_changed_row_for_a_hard_deleted_document_removes_its_points() {
    let f = fixture();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    );
    f.documents
        .docs
        .write()
        .unwrap()
        .remove(&TenantId(TENANT.to_string()));

    let report = reconciler(&f).reconcile_once().await.unwrap();

    assert_eq!(
        report.deleted, 1,
        "no document to read: converge by deleting; report={report:?}"
    );
    assert_eq!(
        f.index.deleted_documents.read().unwrap().as_slice(),
        &[DocumentId(DOC.to_string())]
    );
}

#[tokio::test]
async fn the_running_loop_stops_on_cancellation() {
    let f = fixture();
    let cancel = CancelToken::new();
    let worker = Arc::new(reconciler(&f));
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { worker.run(cancel).await }
    });
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("the loop must observe cancellation")
        .unwrap();
}
