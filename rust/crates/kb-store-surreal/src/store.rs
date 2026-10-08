//! `ChunkStore` port implementation for `SurrealChunkStore`.

use async_trait::async_trait;
use kb_ports::{ChunkStore, DocumentId, DocumentScope, NeighbourAnchor, PortError, SearchHit};

use crate::bm25::execute_bm25_search;
use crate::client::SurrealChunkStore;
use crate::figures::execute_figures;
use crate::neighbours::execute_neighbours;
use crate::vector::execute_vector_search;

#[async_trait]
impl ChunkStore for SurrealChunkStore {
    async fn vector_search(
        &self,
        scope: &DocumentScope,
        embedding: &[f32],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        execute_vector_search(self, scope, embedding, limit).await
    }

    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        execute_bm25_search(self, scope, query, limit).await
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        anchors: &[NeighbourAnchor],
        window: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        execute_neighbours(self, scope, anchors, window).await
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        query: &str,
        retrieved_text: &str,
        retrieved_doc_ids: &[DocumentId],
        anchored_chunks: &[(DocumentId, u32)],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        execute_figures(
            self,
            scope,
            query,
            retrieved_text,
            retrieved_doc_ids,
            anchored_chunks,
            limit,
        )
        .await
    }
}
