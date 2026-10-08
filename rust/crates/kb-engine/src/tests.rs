#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(test)]
mod pipeline_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::fakes::{
        FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit,
        make_fake_scope,
    };
    use crate::retrieval::{RetrievalOptions, execute_pipeline};
    use kb_ports::{ChunkType, DocumentId, EngineError, RetrievalDegrade, TenantId};

    fn setup() -> (
        Arc<FakeDocumentStore>,
        Arc<FakeChunkStore>,
        Arc<FakeEmbedder>,
        Arc<FakeReranker>,
    ) {
        (
            Arc::new(FakeDocumentStore::default()),
            Arc::new(FakeChunkStore::default()),
            Arc::new(FakeEmbedder::default()),
            Arc::new(FakeReranker::default()),
        )
    }

    fn noop_expander() -> Arc<dyn crate::QueryExpander> {
        Arc::new(crate::NoopExpander)
    }

    #[tokio::test]
    async fn test_1_both_arms_succeed_fused_with_rrf() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c1",
            "d1",
            "Title 1",
            "Content 1",
            1,
            0.9,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c2",
            "d1",
            "Title 1",
            "Content 2",
            2,
            0.8,
        ));

        let opts = RetrievalOptions::default();
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 2);
        assert!(outcome.degrades.is_empty());
    }

    #[tokio::test]
    async fn test_2_vector_fails_bm25_succeeds_degrade_recorded() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        chunk_store.fail_vector.store(true, Ordering::Relaxed);
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c1",
            "d1",
            "Title 1",
            "Content 1",
            1,
            0.8,
        ));

        let opts = RetrievalOptions::default();
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(outcome.degrades, vec![RetrievalDegrade::VectorArmFailed]);
    }

    #[tokio::test]
    async fn test_3_bm25_fails_vector_succeeds_degrade_recorded() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        chunk_store.fail_bm25.store(true, Ordering::Relaxed);
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c1",
            "d1",
            "Title 1",
            "Content 1",
            1,
            0.8,
        ));

        let opts = RetrievalOptions::default();
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(outcome.degrades, vec![RetrievalDegrade::Bm25ArmFailed]);
    }

    #[tokio::test]
    async fn test_4_both_arms_fail_returns_retrieval_failed() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        chunk_store.fail_vector.store(true, Ordering::Relaxed);
        chunk_store.fail_bm25.store(true, Ordering::Relaxed);

        let opts = RetrievalOptions::default();
        let result = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await;

        assert!(matches!(result, Err(EngineError::RetrievalFailed(_))));
    }

    #[tokio::test]
    async fn test_5_vector_only_hits_below_min_similarity_dropped() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        // Vector hit with low similarity (< 0.30)
        chunk_store
            .vector_hits
            .write()
            .unwrap()
            .push(make_fake_hit("c_low", "d1", "Title 1", "Low sim", 1, 0.15));
        // BM25 hit with any score
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c_bm25",
            "d1",
            "Title 1",
            "BM25 match",
            2,
            0.10,
        ));

        let mut opts = RetrievalOptions::default();
        opts.min_similarity = 0.30;

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        // c_low dropped because vector-only below 0.30; c_bm25 kept
        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(outcome.chunks[0].chunk.id.0, "c_bm25");
    }

    #[tokio::test]
    async fn test_6_rerank_failure_keeps_fused_order_and_records_degrade() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        reranker.fail.store(true, Ordering::Relaxed);
        chunk_store
            .bm25_hits
            .write()
            .unwrap()
            .push(make_fake_hit("c1", "d1", "Title 1", "First", 1, 0.9));

        let opts = RetrievalOptions::default();
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(outcome.degrades, vec![RetrievalDegrade::RerankUnavailable]);
    }

    #[tokio::test]
    async fn test_7_and_8_max_chunks_cuts_before_neighbours_and_figures() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        // Add 5 hits
        for i in 1..=5 {
            chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
                &format!("c{i}"),
                "d1",
                "Title 1",
                &format!("Content {i}"),
                i,
                1.0 - (i as f32 * 0.1),
            ));
        }

        // Add 1 neighbour
        chunk_store
            .neighbour_hits
            .write()
            .unwrap()
            .push(make_fake_hit(
                "c_neigh",
                "d1",
                "Title 1",
                "Neighbour content",
                10,
                0.5,
            ));

        // Add 1 figure
        let mut fig_hit = make_fake_hit("c_fig", "d1", "Title 1", "Figure caption", 20, 0.5);
        fig_hit.chunk.chunk_type = ChunkType::Figure;
        chunk_store.figure_hits.write().unwrap().push(fig_hit);

        let mut opts = RetrievalOptions::default();
        opts.max_chunks = 2; // Cut to 2 chunks before adding neighbour and figure
        opts.neighbour_window = 10;
        opts.figure_limit = 2;

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        // 2 base hits + 1 neighbour + 1 figure = 4 total chunks!
        assert_eq!(outcome.chunks.len(), 4);
    }

    /// Expander that returns fixed paraphrases, or fails.
    struct FixedExpander {
        paraphrases: Vec<String>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl crate::QueryExpander for FixedExpander {
        async fn expand(&self, query: &str) -> Result<Vec<String>, kb_ports::PortError> {
            if self.fail {
                return Err(kb_ports::PortError::Unavailable(
                    "forced expander failure".into(),
                ));
            }
            let mut out = vec![query.to_string()];
            out.extend(self.paraphrases.iter().cloned());
            Ok(out)
        }
    }

    #[tokio::test]
    async fn test_8_expansion_unions_variant_hits_and_bm25_uses_original_only() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        // The fake vector store returns the same list for every variant, so the
        // union must keep one entry per chunk id with its best similarity.
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c_v",
            "d1",
            "Title 1",
            "Vector hit",
            1,
            0.9,
        ));
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c_v",
            "d1",
            "Title 1",
            "Vector hit",
            1,
            0.5,
        ));

        let expander: Arc<dyn crate::QueryExpander> = Arc::new(FixedExpander {
            paraphrases: vec!["para one".into(), "para two".into()],
            fail: false,
        });
        let mut opts = RetrievalOptions::default();
        opts.expansion_enabled = true;
        opts.expansion_paraphrases = 2;
        opts.neighbour_window = 0;
        opts.figure_limit = 0;

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &expander,
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        // One chunk id after union, no degrade.
        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(outcome.chunks[0].chunk.id.0, "c_v");
        assert!(outcome.degrades.is_empty());
    }

    #[tokio::test]
    async fn test_9_expander_failure_records_degrade_and_continues() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant.clone(), vec![DocumentId("d1".into())]);

        chunk_store
            .bm25_hits
            .write()
            .unwrap()
            .push(make_fake_hit("c_b", "d1", "Title 1", "BM25 hit", 1, 0.8));

        let expander: Arc<dyn crate::QueryExpander> = Arc::new(FixedExpander {
            paraphrases: Vec::new(),
            fail: true,
        });
        let mut opts = RetrievalOptions::default();
        opts.expansion_enabled = true;
        opts.neighbour_window = 0;
        opts.figure_limit = 0;

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &expander,
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(
            outcome.degrades,
            vec![RetrievalDegrade::ExpansionUnavailable]
        );
    }

    #[tokio::test]
    async fn test_10_scope_excludes_other_tenants_documents() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant_a = TenantId("tenant-a".into());
        // Tenant A may see only d_a. The store also holds a hit for tenant B's
        // document d_b, which must never leave the scope filter.
        let scope = make_fake_scope(tenant_a.clone(), vec![DocumentId("d_a".into())]);

        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c_b",
            "d_b",
            "Other",
            "Tenant B content",
            1,
            0.99,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c_b",
            "d_b",
            "Other",
            "Tenant B content",
            1,
            0.99,
        ));
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c_a",
            "d_a",
            "Mine",
            "Tenant A content",
            1,
            0.7,
        ));

        let mut opts = RetrievalOptions::default();
        opts.neighbour_window = 0;
        opts.figure_limit = 0;

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
        )
        .await
        .unwrap();

        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 == "d_a")
        );
        assert!(outcome.chunks.iter().all(|h| h.chunk.id.0 != "c_b"));
    }
}
