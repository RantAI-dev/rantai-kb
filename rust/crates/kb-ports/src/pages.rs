//! The page-image arm's ports: rendering a document's pages to images, and
//! embedding those images and text queries into one vector space.
//!
//! Why an arm of its own: on ViDoRe v3 HR, page images embedded with a
//! multimodal model and fused with the text arms lifted page nDCG@10 from
//! 0.561 to 0.629 on held-out queries, and images alone beat text on table
//! questions (0.589 vs 0.495), `docs/eval-ab/vidore3-hr-en-image`.
//!
//! Page vectors live in a [`RetrievalIndex`](crate::RetrievalIndex) of their
//! own (one per page-embedding model), as dense-only units of
//! [`ChunkType::Page`](crate::ChunkType::Page), so they get the same scoped
//! search as every other arm and never mix with the text vector space.

use async_trait::async_trait;

use crate::error::PortError;

/// One rendered page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageImage {
    /// 0-based page, the same numbering as `ChunkMetadata::page`.
    pub page: u32,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
    /// The page's own text (its text layer), embedded together with the image
    /// when not empty. On ViDoRe v3 HR, text + image page vectors scored 0.605
    /// alone against 0.575 for the image alone (`docs/eval-ab/vidore3-hr-en-image`).
    pub text: String,
}

/// Renders a stored file's pages to images.
#[async_trait]
pub trait PageRenderer: Send + Sync {
    /// Render the first `max_pages` pages of `file_bytes`. An empty list for a
    /// format that has no pages (only PDFs do today); an error only when a paged
    /// file cannot be rendered at all. A single page that fails to render comes
    /// back with empty `bytes`, so the caller can count it rather than lose the
    /// whole document.
    async fn render(
        &self,
        mime_type: &str,
        file_bytes: &[u8],
        max_pages: u32,
    ) -> Result<Vec<PageImage>, PortError>;

    /// Render pages `first .. first + count` (0-based), fewer at the end of the
    /// document. Lets a caller embed one window of pages while the next one
    /// renders. The default renders from the start and drops the pages before
    /// `first`; a real renderer should do better.
    async fn render_range(
        &self,
        mime_type: &str,
        file_bytes: &[u8],
        first: u32,
        count: u32,
    ) -> Result<Vec<PageImage>, PortError> {
        let pages = self
            .render(mime_type, file_bytes, first.saturating_add(count))
            .await?;
        Ok(pages.into_iter().filter(|p| p.page >= first).collect())
    }
}

/// Embeds page images, and text queries into the same space.
#[async_trait]
pub trait PageEmbedder: Send + Sync {
    /// One vector per image, in order.
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError>;

    /// A text query, embedded to be compared against page vectors.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError>;

    /// Width of every vector this embedder returns.
    fn dimension(&self) -> u32;
}

/// Scores page images against a query with a vision model.
///
/// One call per query, the pages in the order given; the result is one 0-10
/// relevance score per page, in the same order. A scorer that cannot answer
/// (transport error, a malformed reply, a score count that does not match)
/// returns [`PortError`]; the engine treats that as a visible degradation and
/// keeps the page order it already had.
#[async_trait]
pub trait PageVisionScorer: Send + Sync {
    /// One score per page, in order.
    async fn score(&self, query: &str, pages: &[PageImage]) -> Result<Vec<f32>, PortError>;
}
