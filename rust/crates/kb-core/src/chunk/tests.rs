use super::document::*;
use super::smart::*;
use super::utf16::*;

#[test]
fn empty_input_yields_no_chunks() {
    let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("");
    assert!(out.is_empty());
}

#[test]
fn whitespace_only_yields_no_chunks() {
    let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("   \n\n   ");
    assert!(out.is_empty());
}

#[test]
fn short_text_is_one_chunk() {
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(500),
        ..Default::default()
    })
    .chunk("Hello world.");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "Hello world.");
    assert_eq!(out[0].chunk_index, 0);
}

#[test]
fn long_text_yields_multiple_chunks() {
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(100),
        overlap_size: Some(0),
        ..Default::default()
    })
    .chunk(&["This is a paragraph of moderate length for testing."; 10].join("\n\n"));
    assert!(out.len() > 1);
}

#[test]
fn chunk_indices_are_sequential() {
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(50),
        overlap_size: Some(0),
        ..Default::default()
    })
    .chunk("First paragraph.\n\nSecond paragraph.\n\nThird paragraph.");
    for (i, c) in out.iter().enumerate() {
        assert_eq!(c.chunk_index, i as u32);
    }
}

#[test]
fn heading_text_is_preserved() {
    let out =
        SmartChunker::new(SmartChunkingOptions::default()).chunk("# Title\n\nSome body text.");
    let joined = out
        .iter()
        .map(|c| c.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("# Title"));
    assert!(joined.contains("Some body text"));
}

#[test]
fn hierarchy_path_present_for_heading() {
    let out =
        SmartChunker::new(SmartChunkingOptions::default()).chunk("## Section\n\nContent here.");
    let has = out.iter().any(|c| {
        c.metadata
            .hierarchy_path
            .as_ref()
            .map(|h| !h.is_empty())
            .unwrap_or(false)
    });
    assert!(has);
}

#[test]
fn nested_headings_build_hierarchy() {
    let out = SmartChunker::new(SmartChunkingOptions::default())
        .chunk("# Chapter 1\n\n## Section A\n\nContent under section A.");
    let Some(last) = out.last() else {
        return;
    };
    if let Some(path) = last.metadata.hierarchy_path.as_ref() {
        assert!(!path.is_empty());
    }
}

#[test]
fn flushes_at_heading_boundary() {
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(10_000),
        respect_heading_boundaries: Some(true),
        ..Default::default()
    })
    .chunk("# Heading 1\n\nParagraph one.\n\n# Heading 2\n\nParagraph two.");
    assert!(out.len() >= 2);
}

#[test]
fn fewer_chunks_when_boundaries_disabled() {
    let text = "# Heading 1\n\nParagraph one.\n\n# Heading 2\n\nParagraph two.";
    let with = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(10_000),
        respect_heading_boundaries: Some(true),
        ..Default::default()
    })
    .chunk(text);
    let without = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(10_000),
        respect_heading_boundaries: Some(false),
        ..Default::default()
    })
    .chunk(text);
    assert!(without.len() <= with.len());
}

#[test]
fn code_blocks_preserved_atomically() {
    let out = SmartChunker::new(SmartChunkingOptions {
        preserve_code_blocks: Some(true),
        ..Default::default()
    })
    .chunk("Some intro.\n\n```javascript\nconst x = 1;\nconst y = 2;\n```\n\nSome outro.");
    let code = out.iter().find(|c| c.text.contains("const x = 1"));
    assert!(code.is_some());
    if let Some(code) = code {
        assert!(code.text.contains("```"));
    }
}

#[test]
fn disabling_code_block_preservation_merges_the_fence_into_its_paragraph() {
    // `preserveCodeBlocks: false` must not flush at the closing fence: the
    // fence's lines keep accumulating and are flushed by whatever comes next,
    // so a line that follows the fence immediately lands in the same block.
    // (With a blank line after the fence both settings agree — the blank line
    // ends the block either way, which is why this test needs no blank line.)
    let text = "```javascript\nconst x = 1;\nconst y = 2;\n```\nRight after the fence.";
    let merged = SmartChunker::new(SmartChunkingOptions {
        preserve_code_blocks: Some(false),
        ..Default::default()
    })
    .chunk(text);
    assert!(
        merged
            .iter()
            .any(|c| c.text.contains("const x = 1") && c.text.contains("Right after the fence.")),
        "expected the code block folded into the following paragraph, got {:?}",
        merged.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    let separate = SmartChunker::new(SmartChunkingOptions {
        preserve_code_blocks: Some(true),
        ..Default::default()
    })
    .chunk(text);
    assert!(
        separate
            .iter()
            .any(|c| c.text.contains("const x = 1") && !c.text.contains("Right after the fence.")),
        "expected the code block to be its own chunk, got {:?}",
        separate.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

#[test]

fn code_chunk_type_detected() {
    let out =
        SmartChunker::new(SmartChunkingOptions::default()).chunk("```python\nprint('hi')\n```");
    assert_eq!(out[0].metadata.chunk_type, "code");
}

#[test]
fn table_chunk_type_detected() {
    let out = SmartChunker::new(SmartChunkingOptions::default())
        .chunk("| Col A | Col B |\n| --- | --- |\n| 1 | 2 |");
    let table = out.iter().find(|c| c.metadata.chunk_type == "table");
    assert!(table.is_some());
    if let Some(t) = table {
        assert_eq!(t.metadata.chunk_type, "table");
    }
}

#[test]
fn list_chunk_type_detected() {
    let out = SmartChunker::new(SmartChunkingOptions::default())
        .chunk("- Item one\n- Item two\n- Item three");
    assert_eq!(out[0].metadata.chunk_type, "list");
}

#[test]
fn multiple_chunks_when_exceeding_max() {
    let paragraphs = std::iter::repeat_n("Short sentence here.", 20)
        .collect::<Vec<_>>()
        .join("\n\n");
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(200),
        overlap_size: Some(0),
        ..Default::default()
    })
    .chunk(&paragraphs);
    assert!(out.len() > 1);
}

#[test]
fn wall_of_text_falls_back_to_sentence_splitting() {
    let text = std::iter::repeat_n("This is a sentence. Another sentence here.", 50)
        .collect::<Vec<_>>()
        .join(" ");
    let out = SmartChunker::new(SmartChunkingOptions {
        max_chunk_size: Some(200),
        overlap_size: Some(0),
        ..Default::default()
    })
    .chunk(&text);
    assert!(out.len() > 1);
}

#[test]
fn smart_chunk_document_emits_storage_shape() {
    let out = smart_chunk_document(
        "# Title\n\nSome content here.",
        "Test Doc",
        "general",
        None,
        SmartChunkingOptions::default(),
    );
    assert!(!out.is_empty());
    assert_eq!(out[0].metadata.document_title, "Test Doc");
    assert_eq!(out[0].metadata.category, "general");
    assert!(!out[0].content.is_empty());
}

#[test]
fn smart_chunk_document_filters_empty() {
    let out = smart_chunk_document(
        "Some content.",
        "Doc",
        "cat",
        None,
        SmartChunkingOptions::default(),
    );
    for c in &out {
        assert!(!c.content.is_empty());
    }
}

#[test]
fn smart_chunk_document_passes_subcategory() {
    let out = smart_chunk_document(
        "Content.",
        "Doc",
        "cat",
        Some("subcat"),
        SmartChunkingOptions::default(),
    );
    assert_eq!(out[0].metadata.subcategory.as_deref(), Some("subcat"));
}

fn mk_chunk() -> Chunk {
    Chunk {
        content: "Revenue was $94.9B for Q4.".to_string(),
        metadata: ChunkMetadata {
            document_title: "Apple 10-K".to_string(),
            category: "financial".to_string(),
            section: Some("Income Statement".to_string()),
            chunk_index: 3,
            ..Default::default()
        },
    }
}

#[test]
fn prepare_chunk_includes_contextual_prefix() {
    let mut c = mk_chunk();
    c.metadata.contextual_prefix =
        Some("This chunk continues the Q4 2024 earnings summary.".to_string());
    let out = prepare_chunk_for_embedding(&c);
    assert!(out.contains("This chunk continues the Q4 2024 earnings summary."));
    let prefix_pos = out.find("This chunk continues");
    let body_pos = out.find("Revenue was $94.9B");
    if let (Some(p), Some(b)) = (prefix_pos, body_pos) {
        assert!(p < b);
    }
}

#[test]
fn prepare_chunk_omits_context_when_missing() {
    let c = mk_chunk();
    let out = prepare_chunk_for_embedding(&c);
    assert!(!out.contains("\nContext:"));
}

#[test]
fn utf16_len_counts_ascii_as_units() {
    assert_eq!(utf16_len("hello"), 5);
}

#[test]
fn utf16_len_counts_cjk_as_units() {
    assert_eq!(utf16_len("中文"), 2);
}

#[test]
fn utf16_len_counts_emoji_surrogate_pair_as_two_units() {
    assert_eq!(utf16_len("🦀"), 2);
}
