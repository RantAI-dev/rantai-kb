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
        let chunks = kb_core::chunk::smart_chunk_document(&text, &stem, "", None, kb_engine::ingest::INGEST_CHUNKING);
        for (i, c) in chunks.iter().enumerate() {
            let sv = kb_core::sparse::encode_document(&c.content);
            let full = kb_core::sparse::encode_document(&text);
            let first = text.trim_start().lines().next().unwrap_or("");
            let title = if first.starts_with('#') { first.trim_start_matches('#').trim() } else { "" };
            let with_title = if i == 0 || title.is_empty() { c.content.clone() } else { format!("{title}\n\n{}", c.content) };
            let tv = kb_core::sparse::encode_document(&with_title);
            writeln!(f, "{}", serde_json::json!({"doc": stem, "i": i, "content": c.content,
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
