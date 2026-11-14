//! The one post-pass that turns markdown pipe tables in extracted text into
//! grids (`TBL-1`).
//!
//! OCR and layout extractors (Mistral, MinerU, the OCR router, the PDF text
//! layer) return markdown, not structure. Doing the parsing here means every
//! one of them gets tables without touching its parser, and the stitching and
//! letterhead rules run once for the whole document.

use kb_core::tables::{parse_markdown_tables, stitch_and_drop_furniture};
use kb_ports::ExtractionResult;

/// Fill `result.tables` from markdown pipe tables in its content.
///
/// Per-page `page_map` text is used when the extractor has it, so each grid
/// carries its page and a table split across pages can be stitched by page
/// adjacency. A result that already has tables (a spreadsheet or DOCX parser
/// built them) is left alone.
pub fn attach_markdown_tables(result: &mut ExtractionResult) {
    if result.error.is_some() || !result.tables.is_empty() {
        return;
    }
    let mut grids = Vec::new();
    let page_count = match &result.page_map {
        Some(pages) => {
            for page in pages {
                for mut grid in parse_markdown_tables(&page.text) {
                    grid.page_span = Some((page.page + 1, page.page + 1));
                    grids.push(grid);
                }
            }
            pages.len() as u32
        }
        None => {
            grids = parse_markdown_tables(&result.content);
            1
        }
    };
    if grids.is_empty() {
        return;
    }
    result.tables = stitch_and_drop_furniture(grids, page_count);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_ports::{ExtractionResult, PageText};

    fn table(title: &str) -> String {
        format!("{title}\n| Region | Value |\n| --- | --- |\n| A | 1 |")
    }

    #[test]
    fn a_plain_content_document_gets_its_tables() {
        let mut result = ExtractionResult::text(table("Tabel 1"), "markdown");
        attach_markdown_tables(&mut result);
        assert_eq!(result.tables.len(), 1);
        assert_eq!(result.tables[0].title.as_deref(), Some("Tabel 1"));
    }

    #[test]
    fn per_page_text_stitches_a_table_split_across_pages() {
        let mut result = ExtractionResult {
            content: String::new(),
            page_map: Some(vec![
                PageText {
                    page: 3,
                    text: "| Region | Value |\n| --- | --- |\n| A | 1 |".to_string(),
                },
                PageText {
                    page: 4,
                    text: "| Region | Value |\n| --- | --- |\n| B | 2 |".to_string(),
                },
            ]),
            ..ExtractionResult::default()
        };
        attach_markdown_tables(&mut result);
        assert_eq!(result.tables.len(), 1);
        assert_eq!(result.tables[0].rows.len(), 2);
        assert_eq!(result.tables[0].page_span, Some((4, 5)));
    }

    #[test]
    fn a_parser_that_already_built_tables_is_not_double_read() {
        let mut result = ExtractionResult::text(
            "| A | B |\n| --- | --- |\n| 1 | 2 |",
            "markdown",
        );
        result.tables.push(kb_core::tables::TableGrid {
            header: vec!["kept".to_string()],
            ..kb_core::tables::TableGrid::default()
        });
        attach_markdown_tables(&mut result);
        assert_eq!(result.tables.len(), 1);
        assert_eq!(result.tables[0].header, vec!["kept"]);
    }
}
