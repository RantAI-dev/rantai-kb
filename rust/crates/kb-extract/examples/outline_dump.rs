//! Print the heading outline of a document from its page texts.
//!
//! `cargo run -p kb-extract --example outline_dump -- pages.json`, where the
//! file is a JSON array of page strings, zero-based.
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: outline_dump <pages.json>");
    let pages: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    let contents = kb_extract::outline::contents_nodes(&pages);
    let method = if contents.is_empty() {
        "headings"
    } else {
        "contents"
    };
    let nodes = if contents.is_empty() {
        kb_extract::outline::heading_nodes(&pages)
    } else {
        contents
    };
    println!("method {method}");
    let level1 = nodes.iter().filter(|n| n.level == 1).count();
    println!(
        "nodes {} level1 {} level2+ {}",
        nodes.len(),
        level1,
        nodes.len() - level1
    );
    for node in &nodes {
        println!(
            "{}{} (p{})",
            "  ".repeat(node.level as usize - 1),
            node.title,
            node.start_page
        );
    }
}
