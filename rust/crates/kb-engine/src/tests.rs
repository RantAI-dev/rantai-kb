#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(test)]
mod pipeline_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::fakes::{
        FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit,
        make_fake_scope,
    };
    use crate::graph_arm::{GraphArm, GraphArmConfig};
    use crate::in_memory_graph_store::InMemoryGraphStore;
    use crate::in_memory_index::InMemoryIndex;
    use crate::page_arm::{PageArm, PageArmConfig};
    use crate::retrieval::{RetrievalOptions, execute_pipeline};
    use crate::timing::{ManualClock, SystemClock};
    use async_trait::async_trait;
    use kb_ports::{
        ChunkId, ChunkToStore, ChunkType, DocumentFilter, DocumentId, DocumentMeta, DocumentScope,
        DocumentStore, Embedder, EngineError, EntityUpsert, GraphStore, IndexUnit, KnowledgeBaseId,
        PageEmbedder, PageImage, PortError, ProvenanceArm, RelationUpsert, RerankCandidate,
        RerankResult, Reranker, RetrievalDegrade, RetrievalIndex, ScopeRequest, TenantGraph,
        TenantId,
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
                sha256: None,
                id: DocumentId("d1".into()),
                title: "Title 1".into(),
                categories: vec![],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
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

    /// Neighbours are context, not ranking: the ranked hits come first in rank
    /// order, the neighbours after them, and the context text still reads
    /// each neighbour next to its hit.
    #[tokio::test]
    async fn neighbours_follow_the_ranked_hits_but_stay_woven_in_the_context() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        for (id, idx, sim) in [("hit_a", 10, 0.9), ("hit_b", 20, 0.8), ("hit_c", 30, 0.7)] {
            chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
                id,
                "d1",
                "Title 1",
                &format!("text of {id}"),
                idx,
                sim,
            ));
        }
        for (id, idx) in [("before_a", 9), ("after_a", 11), ("before_b", 19)] {
            chunk_store
                .neighbour_hits
                .write()
                .unwrap()
                .push(make_fake_hit(
                    id,
                    "d1",
                    "Title 1",
                    &format!("text of {id}"),
                    idx,
                    0.5,
                ));
        }
        let opts = RetrievalOptions {
            max_chunks: 3,
            neighbour_window: 1,
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

        let ids: Vec<&str> = outcome
            .chunks
            .iter()
            .map(|c| c.chunk.id.0.as_str())
            .collect();
        assert_eq!(
            &ids[..3],
            ["hit_a", "hit_b", "hit_c"],
            "the ranking comes first: {ids:?}"
        );
        assert_eq!(ids.len(), 6, "and the three neighbours follow: {ids:?}");
        let at = |s: &str| outcome.context.find(s).unwrap_or(usize::MAX);
        assert!(
            at("text of before_a") < at("text of hit_a")
                && at("text of hit_a") < at("text of after_a"),
            "the context reads a neighbour next to its hit"
        );
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
                sha256: None,
                id: DocumentId("doc_a".into()),
                title: "Doc in KB A".into(),
                categories: vec!["cat_a".into()],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
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
                sha256: None,
                id: DocumentId("doc_b".into()),
                title: "Doc in KB B".into(),
                categories: vec!["cat_b".into()],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
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
                    ..DocumentFilter::default()
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
                    ..DocumentFilter::default()
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

    /// `n` hits named `c1..c{n}`, in the fused order the arms returned them.
    /// The similarities stay above the engine's `min_similarity` floor (0.30),
    /// so the whole set reaches the reranker.
    fn many_hits(chunk_store: &FakeChunkStore, n: u32) {
        for i in 1..=n {
            chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("c{i}"),
                "d1",
                "Title 1",
                &"x".repeat(1_000),
                i,
                1.0 - i as f32 * 0.01,
            ));
        }
    }

    /// A reranker that picks its own top N and hands back ranks, like the LLM
    /// one: it does not score every candidate it saw, so the engine must not
    /// widen its head for it.
    struct RankPickingReranker;

    #[async_trait]
    impl Reranker for RankPickingReranker {
        async fn rerank(
            &self,
            _query: &str,
            candidates: Vec<RerankCandidate>,
            top_k: u32,
        ) -> Result<Vec<RerankResult>, PortError> {
            Ok(candidates
                .into_iter()
                .take(top_k as usize)
                .enumerate()
                .map(|(i, c)| RerankResult {
                    id: c.id,
                    final_rank: i as u32,
                    score: top_k as f32 - i as f32,
                })
                .collect())
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

    /// The reranked head is as wide as `KB_RERANK_MAX_K` (issue #31). Before,
    /// only the top `KB_RERANK_FINAL_K` (5) came back with a score and were
    /// ordered by it; the rest came back in the fused order without one, so a
    /// caller that grounds on `rerankScore` had to treat them as irrelevant.
    #[tokio::test]
    async fn the_reranked_head_is_as_wide_as_the_cap() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        many_hits(&chunk_store, 12);
        let opts = RetrievalOptions {
            max_chunks: 12,
            rerank_enabled: true,
            rerank_final_k: 5,
            rerank_max_k: 10,
            ..RetrievalOptions::default()
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

        let scored: Vec<&str> = outcome
            .chunks
            .iter()
            .filter(|c| c.rerank_score.is_some())
            .map(|c| c.chunk.id.0.as_str())
            .collect();
        assert_eq!(
            scored.len(),
            10,
            "the head is as wide as the cap, not as wide as final_k"
        );
        assert_eq!(outcome.reranked_count, Some(10));
        let ids: Vec<&str> = outcome
            .chunks
            .iter()
            .map(|c| c.chunk.id.0.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12"
            ],
            "the tail past the head keeps the fused order"
        );
        assert!(
            outcome.chunks[10].rerank_score.is_none(),
            "a hit past the head has no score"
        );
    }

    /// `KB_RERANK_FINAL_K` still works: an operator who set it above the cap
    /// keeps the head they had.
    #[tokio::test]
    async fn a_wider_final_k_is_not_shrunk_by_the_cap() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        many_hits(&chunk_store, 12);
        let opts = RetrievalOptions {
            max_chunks: 12,
            rerank_enabled: true,
            rerank_final_k: 12,
            rerank_max_k: 10,
            ..RetrievalOptions::default()
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

        assert_eq!(outcome.reranked_count, Some(12));
    }

    /// A reranker that picks its own top N (the LLM one) hands back ranks, not
    /// scores for every candidate: widening its head would put a rank where a
    /// relevance score belongs, so it keeps the head it had.
    #[tokio::test]
    async fn a_reranker_that_picks_its_own_top_n_keeps_the_old_head() {
        let (doc_store, chunk_store, embedder, _reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        many_hits(&chunk_store, 12);
        let opts = RetrievalOptions {
            max_chunks: 12,
            rerank_enabled: true,
            rerank_final_k: 5,
            rerank_max_k: 10,
            ..RetrievalOptions::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(Arc::new(RankPickingReranker) as Arc<dyn Reranker>),
            &noop_expander(),
            &scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.reranked_count,
            Some(5),
            "the head stays at final_k for a reranker that picks its own top N"
        );
    }

    /// The count is absent when no reranker ran, so a search without a
    /// reranker keeps exactly the earlier shape.
    #[tokio::test]
    async fn the_count_is_absent_when_no_reranker_ran() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
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

        assert_eq!(outcome.reranked_count, None);
        assert!(
            outcome.chunks.iter().all(|c| c.rerank_score.is_none()),
            "no reranker, no score"
        );
    }

    /// With a relevance floor the whole pool was scored — the floor compares
    /// every hit against its score — so the count is the scored pool, not the
    /// head. A caller that reads the count as "how much of the answer was
    /// reranked" gets the truth either way.
    #[tokio::test]
    async fn a_floor_scores_the_whole_pool_and_the_count_says_so() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        many_hits(&chunk_store, 12);
        let opts = RetrievalOptions {
            max_chunks: 12,
            rerank_enabled: true,
            rerank_final_k: 5,
            rerank_max_k: 10,
            min_score: Some(0.4),
            ..RetrievalOptions::default()
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

        assert_eq!(
            outcome.reranked_count,
            Some(12),
            "a floor scores every candidate, so every returned hit carries a score"
        );
        assert!(
            outcome.chunks.iter().all(|c| c.rerank_score.is_some()),
            "a floored answer has no unscored hit"
        );
    }

    fn hit_id(hit: &kb_ports::SearchHit) -> &str {
        hit.chunk.id.0.as_str()
    }

    #[tokio::test]
    async fn an_unset_floor_leaves_the_result_untouched() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("c_high", "d1", "Title 1", "High", 1, 0.9),
            make_fake_hit("c_low", "d1", "Title 1", "Low", 2, 0.4),
        ]);

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

        assert_eq!(outcome.chunks.len(), 2);
        assert!(outcome.floor.is_none(), "no floor ran");
        assert!(outcome.note.is_none());
        assert!(outcome.degrades.is_empty());
    }

    #[tokio::test]
    async fn a_dense_floor_drops_hits_below_the_threshold_and_reports_the_kind() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("c_high", "d1", "Title 1", "High", 1, 0.9),
            make_fake_hit("c_low", "d1", "Title 1", "Low", 2, 0.4),
        ]);

        let opts = RetrievalOptions {
            min_score: Some(0.5),
            ..RetrievalOptions::default()
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
        assert_eq!(hit_id(&outcome.chunks[0]), "c_high");
        let floor = outcome.floor.expect("a floor ran");
        assert_eq!(floor.score_kind, crate::retrieval::FloorScoreKind::Dense);
        assert_eq!(floor.dropped, 1);
        assert!(outcome.note.is_none(), "a non-empty answer needs no note");
    }

    #[tokio::test]
    async fn a_floor_scores_hits_past_rerank_final_k_instead_of_dropping_them() {
        // Before: the reranker returned only `rerank_final_k` results, the rest
        // had no rerank score, and any floor cut the answer to `final_k` hits.
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        five_hits(&chunk_store);
        let base = RetrievalOptions {
            max_chunks: 5,
            rerank_enabled: true,
            rerank_final_k: 2,
            ..RetrievalOptions::default()
        };
        let run = |opts: RetrievalOptions| {
            let (d, c, e, r) = (
                doc_store.clone(),
                chunk_store.clone(),
                embedder.clone(),
                reranker.clone(),
            );
            let scope = scope.clone();
            async move {
                execute_pipeline(
                    &(d as Arc<_>),
                    &(c as Arc<_>),
                    &(e as Arc<_>),
                    &(r as Arc<_>),
                    &noop_expander(),
                    &scope,
                    "query",
                    &opts,
                    &SystemClock,
                )
                .await
                .unwrap()
            }
        };
        let floorless = run(base.clone()).await;
        // The fake scores 1.0, 0.95, 0.90, 0.85, 0.80: a 0.5 floor keeps all five.
        let floored = run(RetrievalOptions {
            min_score: Some(0.5),
            ..base
        })
        .await;
        let ids = |o: &crate::retrieval::RetrievalOutcome| -> Vec<String> {
            o.chunks.iter().map(|c| c.chunk.id.0.clone()).collect()
        };
        assert_eq!(floored.chunks.len(), 5, "no hit lost for lack of a score");
        assert_eq!(
            ids(&floored),
            ids(&floorless),
            "the floor only removes, never reorders"
        );
        assert!(floored.chunks.iter().all(|c| c.rerank_score.is_some()));
        assert_eq!(floored.floor.expect("a floor ran").dropped, 0);
    }

    #[tokio::test]
    async fn a_reranked_search_floors_on_the_rerank_score() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        // The fake reranker scores 1.0, 0.95, 0.90, … in its output order, so
        // a 0.97 floor keeps exactly the first hit even though its dense score
        // is not the one being compared.
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("c_first", "d1", "Title 1", "First", 1, 0.40),
            make_fake_hit("c_second", "d1", "Title 1", "Second", 2, 0.39),
        ]);

        let opts = RetrievalOptions {
            rerank_enabled: true,
            min_score: Some(0.97),
            ..RetrievalOptions::default()
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
        assert_eq!(hit_id(&outcome.chunks[0]), "c_first");
        assert_eq!(outcome.chunks[0].rerank_score, Some(1.0));
        let floor = outcome.floor.expect("a floor ran");
        assert_eq!(floor.score_kind, crate::retrieval::FloorScoreKind::Rerank);
        assert_eq!(floor.dropped, 1);
    }

    #[tokio::test]
    async fn an_identifier_query_keeps_the_hit_the_keyword_arm_matched_exactly() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        // A dense hit below the floor, and a BM25-only hit whose text really
        // contains the identifier. The query mentions an ID, so the keyword
        // hit is exact evidence the dense arm cannot rank.
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "c_dense",
            "d1",
            "Title 1",
            "Unrelated",
            1,
            0.40,
        ));
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c_exact",
            "d1",
            "Title 1",
            "Acme X200 reset pin is under the flap",
            2,
            0.10,
        ));

        let opts = RetrievalOptions {
            min_score: Some(0.90),
            ..RetrievalOptions::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "how do I reset my Acme X200 router",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        assert_eq!(outcome.chunks.len(), 1);
        assert_eq!(hit_id(&outcome.chunks[0]), "c_exact");
        assert_eq!(outcome.floor.expect("a floor ran").dropped, 1);
    }

    #[tokio::test]
    async fn a_failed_dense_arm_skips_the_floor_instead_of_emptying_the_answer() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.fail_vector.store(true, Ordering::Relaxed);
        chunk_store.bm25_hits.write().unwrap().push(make_fake_hit(
            "c_bm25",
            "d1",
            "Title 1",
            "Keyword match",
            1,
            0.8,
        ));

        let opts = RetrievalOptions {
            min_score: Some(0.99),
            ..RetrievalOptions::default()
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

        assert_eq!(outcome.chunks.len(), 1, "normal results are returned");
        assert!(outcome.floor.is_none(), "the floor did not run");
        assert!(
            outcome.degrades.contains(&RetrievalDegrade::FloorSkipped),
            "a skipped floor is reported: {:?}",
            outcome.degrades
        );
        assert!(
            outcome
                .degrades
                .contains(&RetrievalDegrade::VectorArmFailed),
            "{:?}",
            outcome.degrades
        );
    }

    #[tokio::test]
    async fn a_failed_reranker_skips_the_floor_instead_of_floating_on_dense() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("c1", "d1", "Title 1", "One", 1, 0.8),
            make_fake_hit("c2", "d1", "Title 1", "Two", 2, 0.7),
        ]);
        reranker.fail.store(true, Ordering::Relaxed);

        let opts = RetrievalOptions {
            rerank_enabled: true,
            min_score: Some(0.99),
            ..RetrievalOptions::default()
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

        assert_eq!(outcome.chunks.len(), 2);
        assert!(outcome.floor.is_none());
        assert!(
            outcome
                .degrades
                .contains(&RetrievalDegrade::RerankUnavailable),
            "{:?}",
            outcome.degrades
        );
        assert!(
            outcome.degrades.contains(&RetrievalDegrade::FloorSkipped),
            "{:?}",
            outcome.degrades
        );
    }

    #[tokio::test]
    async fn a_floor_that_removes_every_hit_answers_empty_with_a_note() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store
            .vector_hits
            .write()
            .unwrap()
            .push(make_fake_hit("c1", "d1", "Title 1", "Nearest", 1, 0.40));

        let opts = RetrievalOptions {
            min_score: Some(0.75),
            ..RetrievalOptions::default()
        };
        let outcome = execute_pipeline(
            &(doc_store as Arc<_>),
            &(chunk_store as Arc<_>),
            &(embedder as Arc<_>),
            &(reranker as Arc<_>),
            &noop_expander(),
            &scope,
            "berapa harga tiket bioskop saya minggu lalu",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap();

        assert!(outcome.chunks.is_empty());
        assert!(outcome.sources.is_empty());
        assert!(outcome.context.is_empty());
        assert!(outcome.degrades.is_empty(), "nothing failed");
        let note = outcome
            .note
            .expect("an intentionally empty answer says why");
        assert!(note.contains("relevance floor"), "{note}");
        assert_eq!(outcome.floor.expect("a floor ran").dropped, 1);
    }

    /// A page embedder that counts query embeds, so "the page arm did not run"
    /// is a fact about a call and not an inference from an empty result.
    struct CountingPageEmbedder {
        calls: Arc<AtomicUsize>,
        vector: Vec<f32>,
    }

    #[async_trait]
    impl PageEmbedder for CountingPageEmbedder {
        async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
            Ok(pages.iter().map(|_| self.vector.clone()).collect())
        }

        async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.vector.clone())
        }

        fn dimension(&self) -> u32 {
            self.vector.len() as u32
        }
    }

    /// A graph store that counts every read the arm would make before deciding
    /// it has nothing to say.
    struct CountingGraphStore {
        inner: InMemoryGraphStore,
        reads: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GraphStore for CountingGraphStore {
        async fn upsert_entities(
            &self,
            tenant: &TenantId,
            document_id: &DocumentId,
            entities: Vec<EntityUpsert>,
        ) -> Result<(), PortError> {
            self.inner
                .upsert_entities(tenant, document_id, entities)
                .await
        }

        async fn relate(
            &self,
            tenant: &TenantId,
            document_id: &DocumentId,
            relations: Vec<RelationUpsert>,
        ) -> Result<(), PortError> {
            self.inner.relate(tenant, document_id, relations).await
        }

        async fn delete_document(
            &self,
            tenant: &TenantId,
            document_id: &DocumentId,
        ) -> Result<(), PortError> {
            self.inner.delete_document(tenant, document_id).await
        }

        async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.load_tenant_graph(scope).await
        }

        async fn graph_version(&self, tenant: &TenantId) -> Result<u64, PortError> {
            self.inner.graph_version(tenant).await
        }

        async fn match_entities(
            &self,
            scope: &DocumentScope,
            query_terms: &[String],
        ) -> Result<Vec<(String, f32)>, PortError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.match_entities(scope, query_terms).await
        }

        async fn entity_mentions(
            &self,
            scope: &DocumentScope,
            key: &str,
        ) -> Result<Vec<ChunkId>, PortError> {
            self.inner.entity_mentions(scope, key).await
        }
    }

    fn page_unit(doc: &str, page: u32, chunk_ids: &[&str]) -> IndexUnit {
        IndexUnit {
            id: ChunkId(format!("{doc}_page_{page}")),
            document_id: DocumentId(doc.to_string()),
            tenant: TenantId("t1".into()),
            knowledge_base_id: None,
            category: None,
            chunk_index: page,
            page: Some(page),
            chunk_type: ChunkType::Page,
            content: String::new(),
            contextual_prefix: None,
            metadata: serde_json::json!({ "chunkIds": chunk_ids, "page": page }),
            dense: vec![1.0, 0.0, 0.0],
            sparse: <kb_core::sparse::SparseVector>::default(),
        }
    }

    fn stored_chunk(index: u32, content: &str) -> ChunkToStore {
        ChunkToStore {
            content: content.to_string(),
            chunk_index: index,
            embedding: Vec::new(),
            metadata: serde_json::json!({ "chunkIndex": index }),
            contextual_prefix: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_with(
        doc_store: &Arc<FakeDocumentStore>,
        chunk_store: &Arc<FakeChunkStore>,
        embedder: &Arc<FakeEmbedder>,
        reranker: &Arc<FakeReranker>,
        scope: &DocumentScope,
        query: &str,
        opts: &RetrievalOptions,
    ) -> crate::RetrievalOutcome {
        execute_pipeline(
            &(doc_store.clone() as Arc<dyn DocumentStore>),
            &(chunk_store.clone() as Arc<dyn RetrievalIndex>),
            &(embedder.clone() as Arc<dyn Embedder>),
            &(reranker.clone() as Arc<dyn kb_ports::Reranker>),
            &noop_expander(),
            scope,
            query,
            opts,
            &SystemClock,
        )
        .await
        .unwrap()
    }

    /// With the switch off the pipeline must be the one every earlier release
    /// ran. A factual query's plan is the full plan, so its on/off outcomes have
    /// to match on every field that decides an answer.
    #[tokio::test]
    async fn adaptive_off_is_identical_to_the_full_plan() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("d1_0", "d1", "Title 1", "Alpha", 0, 0.9),
            make_fake_hit("d1_1", "d1", "Title 1", "Beta", 1, 0.8),
        ]);
        chunk_store
            .bm25_hits
            .write()
            .unwrap()
            .push(make_fake_hit("d1_0", "d1", "Title 1", "Alpha", 0, 0.7));
        chunk_store
            .neighbour_hits
            .write()
            .unwrap()
            .push(make_fake_hit("d1_2", "d1", "Title 1", "Gamma", 2, 0.6));
        chunk_store
            .stored_chunks
            .write()
            .unwrap()
            .insert(DocumentId("d1".into()), vec![stored_chunk(0, "Alpha")]);

        let page_calls = Arc::new(AtomicUsize::new(0));
        let page_index = Arc::new(InMemoryIndex::new(3));
        page_index
            .upsert(vec![page_unit("d1", 0, &["d1_0"])])
            .await
            .unwrap();
        let page_arm = Arc::new(PageArm {
            embedder: Arc::new(CountingPageEmbedder {
                calls: page_calls,
                vector: vec![1.0, 0.0, 0.0],
            }),
            index: page_index,
            config: PageArmConfig::default(),
            vision: None,
        });
        let graph_store: Arc<dyn GraphStore> = Arc::new(CountingGraphStore {
            inner: InMemoryGraphStore::new(),
            reads: Arc::new(AtomicUsize::new(0)),
        });
        let graph_arm = Arc::new(GraphArm::new(graph_store, GraphArmConfig::default()));

        let opts = RetrievalOptions {
            max_chunks: 5,
            neighbour_window: 1,
            figure_limit: 2,
            rerank_enabled: true,
            page_arm: Some(page_arm),
            graph_arm: Some(graph_arm),
            ..RetrievalOptions::default()
        };
        let off = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "who wrote the report?",
            &opts,
        )
        .await;
        let on = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "who wrote the report?",
            &RetrievalOptions {
                adaptive_arms: true,
                ..opts.clone()
            },
        )
        .await;

        assert!(off.adaptive.is_none(), "off reports no adaptive stats");
        let adaptive = on.adaptive.as_ref().expect("on reports the class");
        assert_eq!(adaptive.class, kb_core::QueryClass::Factual);
        assert!(adaptive.skipped_arms.is_empty(), "factual runs every arm");
        let ids = |outcome: &crate::RetrievalOutcome| {
            outcome
                .chunks
                .iter()
                .map(|hit| (hit.chunk.id.0.clone(), hit.rank))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&off), ids(&on));
        assert_eq!(off.degrades, on.degrades);
        assert!(off.context == on.context);
    }

    #[tokio::test]
    async fn an_exact_query_does_not_call_the_page_or_graph_arms() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
            "d1_0",
            "d1",
            "Title 1",
            "X200 manual",
            0,
            0.9,
        ));

        let page_calls = Arc::new(AtomicUsize::new(0));
        let graph_reads = Arc::new(AtomicUsize::new(0));
        let opts = RetrievalOptions {
            max_chunks: 5,
            adaptive_arms: true,
            page_arm: Some(Arc::new(PageArm {
                embedder: Arc::new(CountingPageEmbedder {
                    calls: page_calls.clone(),
                    vector: vec![1.0, 0.0, 0.0],
                }),
                index: Arc::new(InMemoryIndex::new(3)),
                config: PageArmConfig::default(),
                vision: None,
            })),
            graph_arm: Some(Arc::new(GraphArm::new(
                Arc::new(CountingGraphStore {
                    inner: InMemoryGraphStore::new(),
                    reads: graph_reads.clone(),
                }),
                GraphArmConfig::default(),
            ))),
            ..RetrievalOptions::default()
        };
        let outcome = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "reset Acme X200 router",
            &opts,
        )
        .await;

        assert_eq!(
            page_calls.load(Ordering::SeqCst),
            0,
            "the page-image arm must not embed the query"
        );
        assert_eq!(
            graph_reads.load(Ordering::SeqCst),
            0,
            "the graph arm must not read the graph"
        );
        let adaptive = outcome.adaptive.expect("adaptive stats are reported");
        assert_eq!(adaptive.class, kb_core::QueryClass::Exact);
        assert!(!adaptive.arm_plan_fallback);
        let arms: Vec<&str> = adaptive
            .skipped_arms
            .iter()
            .map(|skipped| skipped.arm.as_str())
            .collect();
        assert_eq!(arms, vec!["pageImage", "graph"]);
        assert!(
            adaptive
                .skipped_arms
                .iter()
                .all(|skipped| skipped.reason == "exact query")
        );
        assert!(!outcome.chunks.is_empty(), "the text arms still answered");
    }

    #[tokio::test]
    async fn an_empty_adaptive_plan_falls_back_to_the_full_plan_once() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        // The text arms find nothing; the page arm is the only thing that can
        // answer, and the exact-class plan would skip it.
        chunk_store.stored_chunks.write().unwrap().insert(
            DocumentId("d1".into()),
            vec![stored_chunk(0, "X200 manual")],
        );
        let page_calls = Arc::new(AtomicUsize::new(0));
        let page_index = Arc::new(InMemoryIndex::new(3));
        page_index
            .upsert(vec![page_unit("d1", 0, &["d1_0"])])
            .await
            .unwrap();
        let opts = RetrievalOptions {
            max_chunks: 5,
            adaptive_arms: true,
            page_arm: Some(Arc::new(PageArm {
                embedder: Arc::new(CountingPageEmbedder {
                    calls: page_calls.clone(),
                    vector: vec![1.0, 0.0, 0.0],
                }),
                index: page_index,
                config: PageArmConfig::default(),
                vision: None,
            })),
            ..RetrievalOptions::default()
        };
        let outcome = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "reset Acme X200 router",
            &opts,
        )
        .await;

        let adaptive = outcome.adaptive.expect("adaptive stats are reported");
        assert_eq!(adaptive.class, kb_core::QueryClass::Exact);
        assert!(
            adaptive.arm_plan_fallback,
            "the plan guessed wrong and must say so"
        );
        assert!(
            adaptive.skipped_arms.is_empty(),
            "a fallback ran every arm, so none were skipped in the end"
        );
        assert_eq!(
            page_calls.load(Ordering::SeqCst),
            1,
            "the page arm ran once, in the fallback"
        );
        assert_eq!(outcome.chunks.len(), 1, "{:?}", outcome.chunks);
        assert_eq!(outcome.chunks[0].chunk.id.0, "d1_0");
    }

    /// A plan may turn an arm off; it may never widen the scope. The arms that
    /// do run still receive the caller's narrowed `DocumentScope`.
    #[tokio::test]
    async fn adaptive_mode_keeps_every_running_arm_inside_the_scope() {
        let (doc_store, chunk_store, embedder, reranker) = setup();
        doc_store.insert_doc(
            TenantId("t1".into()),
            DocumentMeta {
                sha256: None,
                id: DocumentId("d2".into()),
                title: "Hidden".into(),
                categories: vec![],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
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
        // Both documents have a matching vector hit; only d1 is in scope.
        chunk_store.vector_hits.write().unwrap().extend([
            make_fake_hit("d1_0", "d1", "Title 1", "Alpha report", 0, 0.9),
            make_fake_hit("d2_0", "d2", "Hidden", "Alpha report", 0, 0.99),
        ]);
        let page_index = Arc::new(InMemoryIndex::new(3));
        page_index
            .upsert(vec![
                page_unit("d1", 0, &["d1_0"]),
                page_unit("d2", 0, &["d2_0"]),
            ])
            .await
            .unwrap();
        let opts = RetrievalOptions {
            max_chunks: 10,
            adaptive_arms: true,
            page_arm: Some(Arc::new(PageArm {
                embedder: Arc::new(CountingPageEmbedder {
                    calls: Arc::new(AtomicUsize::new(0)),
                    vector: vec![1.0, 0.0, 0.0],
                }),
                index: page_index,
                config: PageArmConfig::default(),
                vision: None,
            })),
            ..RetrievalOptions::default()
        };
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        let outcome = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "what does the report say about Alpha?",
            &opts,
        )
        .await;

        assert!(!outcome.chunks.is_empty());
        assert!(
            outcome
                .chunks
                .iter()
                .all(|hit| hit.chunk.document_id.0 == "d1"),
            "an arm ran outside the scope: {:?}",
            outcome
                .chunks
                .iter()
                .map(|hit| hit.chunk.document_id.0.clone())
                .collect::<Vec<_>>()
        );
    }

    // ---- RET-12: breadcrumb in the rerank input ----

    fn breadcrumb_setup() -> (
        Arc<FakeDocumentStore>,
        Arc<FakeChunkStore>,
        Arc<FakeEmbedder>,
        Arc<FakeReranker>,
    ) {
        let doc_store = Arc::new(FakeDocumentStore::default());
        doc_store.insert_doc(
            TenantId("t1".into()),
            DocumentMeta {
                sha256: None,
                id: DocumentId("d1".into()),
                title: "Doc".into(),
                categories: vec![],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
                chunk_count: 2,
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

    fn breadcrumb_hit(content: &str, index: u32, similarity: f32) -> kb_ports::SearchHit {
        let mut hit = make_fake_hit(
            &format!("c{index}"),
            "d1",
            "Doc",
            content,
            index,
            similarity,
        );
        hit.chunk.metadata.section = Some("Section".into());
        hit.chunk.metadata.page = Some(3);
        hit
    }

    async fn run_breadcrumb_mode(
        flag: bool,
        budget: usize,
    ) -> (crate::RetrievalOutcome, Vec<String>) {
        let (doc_store, chunk_store, embedder, reranker) = breadcrumb_setup();
        let scope = make_fake_scope(TenantId("t1".into()), vec![DocumentId("d1".into())]);
        chunk_store.vector_hits.write().unwrap().extend([
            breadcrumb_hit("éééééééé", 1, 0.9),
            breadcrumb_hit("berlaku 12 hari kerja", 2, 0.8),
        ]);
        let opts = RetrievalOptions {
            rerank_enabled: true,
            rerank_char_budget: budget,
            rerank_breadcrumbs: flag,
            ..RetrievalOptions::default()
        };
        let outcome = run_with(
            &doc_store,
            &chunk_store,
            &embedder,
            &reranker,
            &scope,
            "query",
            &opts,
        )
        .await;
        let seen = reranker
            .seen
            .read()
            .unwrap()
            .iter()
            .map(|c| c.text.clone())
            .collect();
        (outcome, seen)
    }

    #[tokio::test]
    async fn breadcrumbs_off_rerank_input_is_byte_identical() {
        let (outcome, seen) = run_breadcrumb_mode(false, 5).await;
        assert_eq!(seen.len(), 2, "both candidates reach the reranker");
        assert_eq!(
            seen[0], "ééé",
            "off: a UTF-8-safe 5-byte cut of the content"
        );
        assert_eq!(
            seen[1], "berlak",
            "off: the same cut, no breadcrumb (the existing budget is inclusive)"
        );
        assert!(
            seen.iter().all(|t| !t.contains('›') && !t.contains('\n')),
            "off: no breadcrumb and no separator may reach the reranker: {seen:?}"
        );
        assert!(
            outcome.chunks.iter().any(|h| h.chunk.content == "éééééééé"),
            "the response keeps the untruncated original content"
        );
    }

    #[tokio::test]
    async fn breadcrumbs_on_prefixes_title_section_and_page() {
        let (_, seen) = run_breadcrumb_mode(true, 0).await;
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "Doc › Section · p.3\néééééééé");
        assert_eq!(seen[1], "Doc › Section · p.3\nberlaku 12 hari kerja");
    }

    #[tokio::test]
    async fn breadcrumbs_on_with_a_budget_truncates_only_the_content() {
        let (_, seen) = run_breadcrumb_mode(true, 5).await;
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "Doc › Section · p.3\nééé");
        assert_eq!(seen[1], "Doc › Section · p.3\nberlak");
        assert!(
            seen.iter().all(|t| t.starts_with("Doc › Section · p.3\n")),
            "the breadcrumb is never clipped: {seen:?}"
        );
    }

    #[tokio::test]
    async fn response_chunk_content_is_unchanged_in_both_modes() {
        let (off, off_seen) = run_breadcrumb_mode(false, 0).await;
        let (on, on_seen) = run_breadcrumb_mode(true, 0).await;
        let off_contents: Vec<_> = off.chunks.iter().map(|h| h.chunk.content.clone()).collect();
        let on_contents: Vec<_> = on.chunks.iter().map(|h| h.chunk.content.clone()).collect();
        assert_eq!(off_contents, on_contents, "only the rerank input changes");
        assert!(off_contents.contains(&"éééééééé".to_string()));
        assert!(off_contents.contains(&"berlaku 12 hari kerja".to_string()));
        assert_ne!(off_seen, on_seen, "the reranker must see a different text");
    }
}
