use std::io::Write;
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let out = std::env::args().nth(2).unwrap();
    let mut f = std::fs::File::create(&out).unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(format!("{dir}/corpus")).unwrap().map(|e| e.unwrap().path()).collect();
    paths.sort();
    for p in paths {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&p).unwrap();
        // The title the engine gives this document: the H1 of the file (the
        // eval now uploads it as the title too), else the file stem.
        let title = kb_core::title::document_title(None, &stem, &text, true);
        let chunks = kb_core::chunk::smart_chunk_document(&text, &title, "", None, kb_engine::ingest::INGEST_CHUNKING);
        for (i, c) in chunks.iter().enumerate() {
            let sv = kb_core::sparse::encode_document(&c.content);
            let full = kb_core::sparse::encode_document(&text);
            let first = text.trim_start().lines().next().unwrap_or("");
            let h1 = if first.starts_with('#') { first.trim_start_matches('#').trim() } else { "" };
            let with_title = if i == 0 || h1.is_empty() { c.content.clone() } else { format!("{h1}\n\n{}", c.content) };
            let tv = kb_core::sparse::encode_document(&with_title);
            // The exact text the engine embeds for this chunk: the title, the
            // chunk's heading path, and the content. The caller adds the
            // model's passage prefix.
            let emb = kb_core::title::embedding_text(&title, c.metadata.heading_path.as_deref(), &c.content);
            writeln!(f, "{}", serde_json::json!({"doc": stem, "i": i, "content": c.content,
              "title": title, "path": c.metadata.heading_path, "emb": emb,
              "si": sv.indices, "sv": sv.values, "fsi": full.indices, "fsv": full.values, "tsi": tv.indices, "tsv": tv.values})).unwrap();
        }
    }
    let q = format!("{dir}/queries.jsonl");
    let mut fq = std::fs::File::create(format!("{out}.queries")).unwrap();
    for line in std::fs::read_to_string(q).unwrap().lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let t = v["text"].as_str().unwrap();
        let sv = kb_core::sparse::encode_query(t);
        writeln!(fq, "{}", serde_json::json!({"id": v["_id"], "text": t, "si": sv.indices, "sv": sv.values})).unwrap();
    }
}
