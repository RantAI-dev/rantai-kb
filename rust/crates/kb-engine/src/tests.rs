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
    use crate::timing::{ManualClock, SystemClock};
    use kb_ports::{
        ChunkType, DocumentFilter, DocumentId, DocumentMeta, DocumentStore, EngineError,
        KnowledgeBaseId, ProvenanceArm, RetrievalDegrade, ScopeRequest, TenantId,
    };

    fn setup() -> (
        Arc<FakeDocumentStore>,
        Arc<FakeChunkStore>,
        Arc<FakeEmbedder>,
        Arc<FakeReranker>,
    ) {
        let doc_store = Arc::new(FakeDocumentStore::default());
        doc_store.insert_doc(
            TenantId("t1".into()),
            DocumentMeta {
                id: DocumentId("d1".into()),
                title: "Title 1".into(),
                categories: vec![],
                subcategory: None,
                status: "READY".into(),
                chunk_count: 1,
                file_type: None,
                file_size: None,
                mime_type: None,
                s3_key: None,
                created_at: "".into(),
                updated_at: "".into(),
                knowledge_base_ids: vec![],
                metadata: None,
            },
        );
        (
            doc_store,
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
            &SystemClock,
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

        let opts = RetrievalOptions {
            rerank_enabled: true,
            max_chunks: 0,
            ..Default::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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

        let opts = RetrievalOptions {
            rerank_enabled: true,
            max_chunks: 0,
            ..Default::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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
            &SystemClock,
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

        let opts = RetrievalOptions {
            min_similarity: 0.30,
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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
        chunk_store
            .bm25_hits
            .write()
            .unwrap()
            .push(make_fake_hit("c2", "d1", "Title 1", "Second", 2, 0.8));

        let opts = RetrievalOptions {
            rerank_enabled: true,
            max_chunks: 1,
            ..Default::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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

        // Cut to 2 chunks before adding neighbour and figure.
        let opts = RetrievalOptions {
            max_chunks: 2,
            neighbour_window: 10,
            figure_limit: 2,
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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
        let opts = RetrievalOptions {
            expansion_enabled: true,
            expansion_paraphrases: 2,
            neighbour_window: 0,
            figure_limit: 0,
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &expander,
            &scope,
            "query",
            &opts,
            &SystemClock,
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
        let opts = RetrievalOptions {
            expansion_enabled: true,
            neighbour_window: 0,
            figure_limit: 0,
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &expander,
            &scope,
            "query",
            &opts,
            &SystemClock,
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

        let opts = RetrievalOptions {
            neighbour_window: 0,
            figure_limit: 0,
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
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

    #[tokio::test]
    async fn retrieval_applies_figure_gate_veto() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(
            TenantId::new_unchecked("t1"),
            vec![DocumentId::new_unchecked("d1")],
        );

        let hit = make_fake_hit("c1", "d1", "Title", "Text passage", 0, 0.9);
        chunk_store.vector_hits.write().unwrap().push(hit);

        let mut fig1 = make_fake_hit("fig_1", "d1", "Title", "[Gambar] Figure 1", 1, 0.8);
        fig1.chunk.chunk_type = ChunkType::Figure;
        fig1.chunk.metadata.asset_key = Some("assets/fig1.png".to_string());

        let mut fig2 = make_fake_hit("fig_2", "d1", "Title", "[Gambar] Figure 2", 2, 0.7);
        fig2.chunk.chunk_type = ChunkType::Figure;
        fig2.chunk.metadata.asset_key = Some("assets/fig2.png".to_string());

        chunk_store.figure_hits.write().unwrap().push(fig1);
        chunk_store.figure_hits.write().unwrap().push(fig2);

        let gate = Arc::new(crate::fakes::FakeFigureGate::default());
        *gate.kept_ids.write().unwrap() = vec!["fig_1".to_string()];

        let opts = RetrievalOptions {
            figure_limit: 2,
            figure_gate: Some(gate),
            ..Default::default()
        };

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        let fig_ids: Vec<&str> = outcome
            .chunks
            .iter()
            .filter(|c| c.chunk.chunk_type == ChunkType::Figure)
            .map(|c| c.chunk.id.0.as_str())
            .collect();
        assert_eq!(fig_ids, vec!["fig_1"]);
    }

    #[tokio::test]
    async fn every_phase_says_which_arm_produced_its_chunk() {
        // The engine is what contributes the neighbour and figure arms, so this
        // is where they are proven: the search arms come back on the candidate,
        // the weave and the co-retrieval are the engine's own.
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(
            TenantId::new_unchecked("t1"),
            vec![DocumentId::new_unchecked("d1")],
        );

        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c-vector",
            "d1",
            "Title",
            "dense passage",
            0,
            0.9,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c-keyword",
            "d1",
            "Title",
            "keyword passage",
            4,
            0.7,
        ));
        chunk_store
            .neighbour_hits
            .write()
            .unwrap()
            .push(make_fake_hit(
                "c-neighbour",
                "d1",
                "Title",
                "adjacent passage",
                5,
                0.0,
            ));
        let mut fig = make_fake_hit("fig_1", "d1", "Title", "[Gambar] Figure 1", 6, 0.5);
        fig.chunk.chunk_type = ChunkType::Figure;
        fig.chunk.metadata.asset_key = Some("assets/fig1.png".to_string());
        chunk_store.figure_hits.write().unwrap().push(fig);

        let opts = RetrievalOptions {
            neighbour_window: 1,
            figure_limit: 1,
            ..Default::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        let arms_of = |id: &str| {
            outcome
                .chunks
                .iter()
                .find(|c| c.chunk.id.0 == id)
                .unwrap_or_else(|| panic!("no chunk {id}"))
                .retrieval
                .as_ref()
                .map(|p| p.arms.clone())
        };
        assert_eq!(
            arms_of("c-vector"),
            Some(vec![ProvenanceArm::Dense]),
            "a dense hit says so"
        );
        assert_eq!(
            arms_of("c-keyword"),
            Some(vec![ProvenanceArm::Sparse]),
            "a keyword hit says so"
        );
        assert_eq!(
            arms_of("c-neighbour"),
            Some(vec![ProvenanceArm::Neighbour]),
            "a woven neighbour says so"
        );
        assert_eq!(
            arms_of("fig_1"),
            Some(vec![ProvenanceArm::Figure]),
            "a co-retrieved figure says so"
        );
    }

    fn setup_two_kb_docs(doc_store: &FakeDocumentStore, tenant: &TenantId) {
        doc_store.insert_doc(
            tenant.clone(),
            DocumentMeta {
                id: DocumentId("doc_a".into()),
                title: "Doc in KB A".into(),
                categories: vec!["cat_a".into()],
                subcategory: None,
                status: "READY".into(),
                chunk_count: 1,
                file_type: None,
                file_size: None,
                mime_type: None,
                s3_key: None,
                created_at: "".into(),
                updated_at: "".into(),
                knowledge_base_ids: vec!["kb_a".into()],
                metadata: None,
            },
        );
        doc_store.insert_doc(
            tenant.clone(),
            DocumentMeta {
                id: DocumentId("doc_b".into()),
                title: "Doc in KB B".into(),
                categories: vec!["cat_b".into()],
                subcategory: None,
                status: "READY".into(),
                chunk_count: 1,
                file_type: None,
                file_size: None,
                mime_type: None,
                s3_key: None,
                created_at: "".into(),
                updated_at: "".into(),
                knowledge_base_ids: vec!["kb_b".into()],
                metadata: None,
            },
        );
    }

    #[tokio::test]
    async fn test_bm25_scope_isolation_kb_restriction() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        setup_two_kb_docs(&doc_store, &tenant);

        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_a",
            "doc_a",
            "Doc in KB A",
            "BM25 match for A",
            0,
            1.0,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_b",
            "doc_b",
            "Doc in KB B",
            "BM25 match for B",
            1,
            0.9,
        ));

        let scope_kb_a = doc_store
            .build_scope(&ScopeRequest {
                tenant: tenant.clone(),
                filter: DocumentFilter {
                    knowledge_base_ids: vec![KnowledgeBaseId("kb_a".into())],
                    category: None,
                },
            })
            .await
            .unwrap();

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope_kb_a,
            "query",
            &RetrievalOptions::default(),
            &SystemClock,
        )
        .await
        .unwrap();

        assert!(!outcome.chunks.is_empty());
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 == "doc_a")
        );
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 != "doc_b")
        );
    }

    #[tokio::test]
    async fn test_bm25_scope_isolation_category_restriction() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        setup_two_kb_docs(&doc_store, &tenant);

        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_a",
            "doc_a",
            "Doc in KB A",
            "BM25 match for A",
            0,
            1.0,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_b",
            "doc_b",
            "Doc in KB B",
            "BM25 match for B",
            1,
            0.9,
        ));

        let scope_cat_a = doc_store
            .build_scope(&ScopeRequest {
                tenant: tenant.clone(),
                filter: DocumentFilter {
                    knowledge_base_ids: vec![],
                    category: Some("cat_a".into()),
                },
            })
            .await
            .unwrap();

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope_cat_a,
            "query",
            &RetrievalOptions::default(),
            &SystemClock,
        )
        .await
        .unwrap();

        assert!(!outcome.chunks.is_empty());
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 == "doc_a")
        );
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 != "doc_b")
        );
    }

    #[tokio::test]
    async fn test_bm25_scope_isolation_document_ids_restriction() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        setup_two_kb_docs(&doc_store, &tenant);

        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_a",
            "doc_a",
            "Doc in KB A",
            "BM25 match for A",
            0,
            1.0,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_b",
            "doc_b",
            "Doc in KB B",
            "BM25 match for B",
            1,
            0.9,
        ));

        let full_scope = doc_store
            .build_scope(&ScopeRequest {
                tenant: tenant.clone(),
                filter: DocumentFilter::default(),
            })
            .await
            .unwrap();
        let scope_doc_a = full_scope.narrowed_to(&[DocumentId("doc_a".into())]);

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope_doc_a,
            "query",
            &RetrievalOptions::default(),
            &SystemClock,
        )
        .await
        .unwrap();

        assert!(!outcome.chunks.is_empty());
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 == "doc_a")
        );
        assert!(
            outcome
                .chunks
                .iter()
                .all(|h| h.chunk.document_id.0 != "doc_b")
        );
    }

    #[tokio::test]
    async fn test_bm25_scope_unrestricted_returns_both() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        setup_two_kb_docs(&doc_store, &tenant);

        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_a",
            "doc_a",
            "Doc in KB A",
            "BM25 match for A",
            0,
            1.0,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "chunk_b",
            "doc_b",
            "Doc in KB B",
            "BM25 match for B",
            1,
            0.9,
        ));

        let full_scope = doc_store
            .build_scope(&ScopeRequest {
                tenant: tenant.clone(),
                filter: DocumentFilter::default(),
            })
            .await
            .unwrap();

        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &full_scope,
            "query",
            &RetrievalOptions::default(),
            &SystemClock,
        )
        .await
        .unwrap();

        let found_docs: std::collections::HashSet<String> = outcome
            .chunks
            .iter()
            .map(|h| h.chunk.document_id.0.clone())
            .collect();
        assert!(found_docs.contains("doc_a"));
        assert!(found_docs.contains("doc_b"));
    }

    /// The point of the timing module: a request that ran records every stage,
    /// and what it recorded adds up to no more than the time it took.
    #[tokio::test]
    async fn a_retrieval_reports_every_stage_it_ran() {
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

        let clock = ManualClock::stepping(std::time::Duration::from_micros(200));
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &RetrievalOptions::default(),
            &clock,
        )
        .await
        .unwrap();

        let t = outcome.timings;
        assert!(t.total_ms > 0.0, "a pipeline that ran was timed");
        assert_eq!(t.engine_total_ms, t.total_ms);
        assert!(t.arms_ms > 0.0, "both arms ran inside one phase");
        assert!(t.query_embed_ms > 0.0, "the query was embedded");
        assert!(t.vector_arm_ms > 0.0);
        assert!(t.keyword_arm_ms > 0.0);
        assert!(t.scope_check_ms > 0.0, "the keyword arm checked Postgres");
        assert_eq!(
            t.entity_arm_ms, 0.0,
            "there is no entity arm yet; it reports zero rather than going missing"
        );
        assert!(
            t.accounted_sum_ms() <= t.total_ms,
            "accounted {} exceeded the total {}",
            t.accounted_sum_ms(),
            t.total_ms
        );
    }

    /// The cache flag has to survive the fold over the query variants.
    ///
    /// It used not to: the conjunction over variants was written as
    /// `cached.is_some_and(..) && qe.cached`, and on the *first* variant
    /// `is_some_and` on a `None` is false, so a single-variant pipeline — the
    /// only one that exists while expansion is off — always reported a miss.
    /// The symptom was invisible in the fake embedder (which always misses) and
    /// obvious the first time a repeated query came back in 0.07 ms with
    /// `queryEmbedCached: false`.
    #[tokio::test]
    async fn a_cache_hit_reaches_the_stage_timings() {
        for cached in [true, false] {
            let (doc_store, chunk_store, embedder, reranker) = setup();
            embedder.cached.store(cached, Ordering::Relaxed);
            let tenant = TenantId("t1".into());
            let scope = make_fake_scope(tenant, vec![DocumentId("d1".into())]);
            chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
                "c1",
                "d1",
                "Title 1",
                "Content 1",
                1,
                0.9,
            ));

            let outcome = execute_pipeline(
                &(doc_store as Arc<_>),
                &(chunk_store as Arc<_>),
                &(embedder as Arc<_>),
                &(reranker as Arc<_>),
                &noop_expander(),
                &scope,
                "query",
                &RetrievalOptions::default(),
                &SystemClock,
            )
            .await
            .unwrap();

            assert_eq!(
                outcome.timings.query_embed_cached, cached,
                "the embedder's answer has to reach the report"
            );
        }
    }

    /// A reranker that costs 40 ms of clock has to be visible as 40 ms, and a
    /// fusion that costs nothing must not borrow the reranker's time.
    #[tokio::test]
    async fn the_rerank_stage_is_timed_separately_from_the_arms() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant, vec![DocumentId("d1".into())]);
        for i in 0..4 {
            chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("c{i}"),
                "d1",
                "Title 1",
                &format!("Content {i}"),
                i,
                0.9,
            ));
        }
        let opts = RetrievalOptions {
            max_chunks: 2,
            rerank_enabled: true,
            ..RetrievalOptions::default()
        };

        let clock = ManualClock::stepping(std::time::Duration::from_micros(500));
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &clock,
        )
        .await
        .unwrap();

        let t = outcome.timings;
        assert!(t.rerank_ms > 0.0, "the rerank stage was measured");
        assert!(
            t.fusion_ms > 0.0,
            "fusion is its own stage, not part of the arms' time"
        );
        assert!(
            t.rerank_ms >= t.fusion_ms,
            "on a stepping clock the rerank future is polled no fewer times than RRF sorting"
        );
        assert!(t.accounted_sum_ms() <= t.total_ms);
    }

    /// The empty-result early return must still be timed: "no results" and
    /// "nothing measured" are different answers.
    #[tokio::test]
    async fn an_empty_result_is_timed_too() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let tenant = TenantId("t1".into());
        let scope = make_fake_scope(tenant, vec![DocumentId("d1".into())]);

        let clock = ManualClock::stepping(std::time::Duration::from_micros(500));
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &RetrievalOptions::default(),
            &clock,
        )
        .await
        .unwrap();

        assert!(outcome.chunks.is_empty());
        assert!(outcome.timings.total_ms > 0.0);
        assert!(outcome.timings.arms_ms > 0.0);
    }

    fn five_hits(chunk_store: &FakeChunkStore) {
        for i in 1..=5u32 {
            chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("c{i}"),
                "d1",
                "Title 1",
                &"x".repeat(1_000),
                i,
                1.0 - i as f32 * 0.1,
            ));
        }
    }

    #[tokio::test]
    async fn by_default_the_reranker_sees_every_candidate_in_full() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
        let opts = RetrievalOptions {
            max_chunks: 3,
            rerank_enabled: true,
            ..RetrievalOptions::default()
        };
        execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker.clone() as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        let seen = reranker.seen.read().unwrap();
        assert_eq!(seen.len(), 5, "no candidate limit unless one is configured");
        assert!(
            seen.iter().all(|c| c.text.len() == 1_000),
            "no character budget unless one is configured"
        );
    }

    #[tokio::test]
    async fn a_configured_budget_limits_and_truncates_candidates() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
        let opts = RetrievalOptions {
            max_chunks: 3,
            rerank_enabled: true,
            rerank_candidate_limit: 2,
            rerank_char_budget: 200,
            ..RetrievalOptions::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker.clone() as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        let seen = reranker.seen.read().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|c| c.text.len() <= 201));
        // The candidates beyond the limit still come back, after the reranked ones.
        assert_eq!(outcome.chunks.len(), 3);
    }

    /// A caller asking for as many chunks as the pool holds still gets them
    /// reranked. Before 2026-10-23 the reranker ran only when the pool was
    /// larger than `max_chunks`, so asking for 10 of 5 candidates returned the
    /// fused order untouched.
    #[tokio::test]
    async fn the_reranker_runs_when_the_caller_asks_for_every_candidate() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
        reranker.only_last.store(true, Ordering::Relaxed);
        let opts = RetrievalOptions {
            max_chunks: 10,
            rerank_enabled: true,
            ..RetrievalOptions::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker.clone() as Arc<_>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        assert_eq!(
            reranker.seen.read().unwrap().len(),
            5,
            "every candidate was reranked"
        );
        let ids: Vec<&str> = outcome
            .chunks
            .iter()
            .map(|c| c.chunk.id.0.as_str())
            .collect();
        assert_eq!(
            ids,
            ["c5", "c1", "c2", "c3", "c4"],
            "the reranker's pick comes first"
        );
    }

    #[tokio::test]
    async fn hits_the_reranker_drops_keep_their_fused_order() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
        reranker.only_last.store(true, Ordering::Relaxed);
        let opts = RetrievalOptions {
            max_chunks: 4,
            rerank_enabled: true,
            ..RetrievalOptions::default()
        };

        // Run it repeatedly: HashMap iteration order differs between maps, so an
        // order that came from a HashMap would not survive many runs unchanged.
        for _ in 0..20 {
            reranker.seen.write().unwrap().clear();
            let outcome = execute_pipeline(
                &(doc_store.clone() as Arc<_>),
                &(chunk_store.clone() as Arc<_>),
                &(embedder.clone() as Arc<_>),
                &(reranker.clone() as Arc<_>),
                &noop_expander(),
                &scope,
                "query",
                &opts,
                &SystemClock,
            )
            .await
            .unwrap();
            let ids: Vec<&str> = outcome
                .chunks
                .iter()
                .map(|c| c.chunk.id.0.as_str())
                .collect();
            assert_eq!(ids, ["c5", "c1", "c2", "c3"]);
        }
    }
}
