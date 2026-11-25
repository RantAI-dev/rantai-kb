//! Offline instrument for TBL-2: compare the column names gold queries use
//! against the names the *server's* extraction + manifest code produces.
//!
//!     cargo run -p kb-extract --example table_column_drift -- [dataset-dir]
//!
//! No server, no database and no network: for every source in tables-v1 it
//! calls `extract_from_buffer_full` (the parser registry `kb-server` uses) and
//! `kb_core::tables::build_manifest`, then walks every gold `TableQuery` in
//! `queries.jsonl`. A gold column that is not a manifest name would be a
//! `query_rejected: unknown_column` through `POST /v1/tables/{id}/query`.
//!
//! The report prints one line per distinct (file, gold column) mismatch, with
//! the closest manifest name and a cause, then counts by cause. `--headers`
//! prints each file's gold and extracted header for diagnosis.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kb_core::tables::{TableGrid, TableManifest, build_manifest, collapse_whitespace};
use kb_extract::{ext_to_mime, extension_of, extract};

#[derive(Default)]
struct Counts {
    queries: usize,
    columns: usize,
    queries_with_drift: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dataset = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("kbdata/tables-v1")
    });
    let show_headers = std::env::args().any(|arg| arg == "--headers");
    let show_grids = std::env::args().any(|arg| arg == "--grids");

    let raw = std::fs::read_to_string(dataset.join("queries.jsonl"))?;
    let questions: Vec<serde_json::Value> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;

    // One extraction per file, exactly as ingest would do it.
    let files: BTreeSet<String> = questions
        .iter()
        .filter_map(|q| q["file"].as_str().map(ToString::to_string))
        .collect();
    let mut extracted: BTreeMap<String, Vec<(TableGrid, TableManifest)>> = BTreeMap::new();
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    for file in &files {
        let path = dataset.join("sources").join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            failures.insert(file.clone(), "source file missing".to_string());
            continue;
        };
        let mime = ext_to_mime(&extension_of(file)).unwrap_or("application/octet-stream");
        match extract(file, mime, &bytes) {
            Ok(result) => {
                extracted.insert(
                    file.clone(),
                    result
                        .tables
                        .into_iter()
                        .map(|g| {
                            let m = build_manifest(&g);
                            (g, m)
                        })
                        .collect(),
                );
            }
            Err(err) => {
                failures.insert(file.clone(), err.to_string());
            }
        }
    }

    // Gold names from the dataset manifest, used only to explain a difference.
    let gold_headers: BTreeMap<String, Vec<String>> = {
        let manifest = std::fs::read_to_string(dataset.join("manifest.json"))?;
        let parsed: serde_json::Value = serde_json::from_str(&manifest)?;
        parsed["tables"]
            .as_array()
            .map(|tables| {
                tables
                    .iter()
                    .filter_map(|t| {
                        let name = t["file"].as_str()?;
                        let header = t["header"]
                            .as_array()?
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(ToString::to_string)
                            .collect();
                        Some((name.to_string(), header))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    if show_grids {
        for (file, grids) in &extracted {
            println!("GRIDS {file}: {}", grids.len());
            for (i, (grid, manifest)) in grids.iter().enumerate() {
                println!(
                    "  [{i}] sheet={:?} title={:?} rows={} header_rows={} header={:?} roles={:?}",
                    grid.sheet,
                    grid.title,
                    grid.rows.len(),
                    grid.header_rows,
                    grid.header,
                    manifest.columns.iter().map(|c| c.role).collect::<Vec<_>>()
                );
            }
        }
    }

    if show_headers {
        for (file, header) in &gold_headers {
            let got = extracted
                .get(file)
                .and_then(|grids| grids.first())
                .map(|(_, m)| {
                    m.columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            println!("HEADER {file}\n  gold: {:?}\n  rust: {:?}", header, got);
        }
    }

    let mut mismatches: BTreeMap<(String, String), Mismatch> = BTreeMap::new();
    let mut totals: BTreeMap<String, Counts> = BTreeMap::new();
    let mut overall = Counts::default();
    let mut no_grid: BTreeSet<String> = BTreeSet::new();

    for question in &questions {
        let file = question["file"].as_str().unwrap_or("");
        let qid = question["id"].as_str().unwrap_or("");
        let mut names: Vec<String> = Vec::new();
        if let Some(items) = question["gold_query"]["select"].as_array() {
            for item in items {
                if let Some(name) = item["column"].as_str() {
                    names.push(name.to_string());
                }
            }
        }
        if let Some(items) = question["gold_query"]["filter"].as_array() {
            for item in items {
                if let Some(name) = item["column"].as_str() {
                    names.push(name.to_string());
                }
            }
        }
        for item in question["gold_query"]["group_by"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(name) = item.as_str() {
                names.push(name.to_string());
            }
        }
        // `order_by.key` is an output name (an alias such as `max_value`), not
        // a manifest column: the compiler validates it against select aliases.

        let grids = extracted.get(file).map(Vec::as_slice).unwrap_or(&[]);
        let chosen = pick_grid(grids, question, gold_headers.get(file));
        if chosen.is_none() {
            no_grid.insert(format!("{qid} {file}"));
        }
        let manifest_names: Vec<String> = chosen
            .map(|(_, m)| m.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        let mut drifted = false;
        for gold in names {
            overall.columns += 1;
            if manifest_names.iter().any(|name| name == &gold) {
                continue;
            }
            drifted = true;
            let (closest, cause) = classify(&gold, &manifest_names, chosen, gold_headers.get(file));
            let entry = mismatches
                .entry((file.to_string(), gold.clone()))
                .or_insert_with(|| Mismatch {
                    gold: gold.clone(),
                    closest,
                    cause,
                    questions: Vec::new(),
                });
            if !entry.questions.iter().any(|id| id == qid) {
                entry.questions.push(qid.to_string());
            }
            let cause_counts = totals.entry(entry.cause.clone()).or_default();
            cause_counts.columns += 1;
        }
        overall.queries += 1;
        if drifted {
            overall.queries_with_drift += 1;
        }
    }

    println!("=== tables-v1 column drift (offline, before/after identical method) ===");
    println!(
        "files={} extraction_failures={} questions={} gold_columns={} queries_with_unknown_column={}",
        files.len(),
        failures.len(),
        overall.queries,
        overall.columns,
        overall.queries_with_drift
    );
    for (file, err) in &failures {
        println!("EXTRACT_FAIL {file}: {err}");
    }
    for id in &no_grid {
        println!("NO_GRID {id}");
    }
    println!("\n=== mismatches: file | gold name -> closest manifest name | cause | questions ===");
    for ((file, _), mismatch) in &mismatches {
        println!(
            "{} | {:?} -> {:?} | {} | {}",
            file,
            mismatch.gold,
            mismatch.closest,
            mismatch.cause,
            mismatch.questions.join(",")
        );
    }
    println!("\n=== counts by cause (gold column occurrences) ===");
    for (cause, counts) in &totals {
        println!("{cause}: {}", counts.columns);
    }
    let sum: usize = totals.values().map(|c| c.columns).sum();
    println!("TOTAL_MISMATCH_COLUMNS: {sum}");
    Ok(())
}

struct Mismatch {
    gold: String,
    closest: String,
    cause: String,
    questions: Vec<String>,
}

/// Mirror `score_tables.map_tables`: the same sheet, the same index, then the
/// largest overlap with the dataset's gold header decides which stored grid a
/// question queries. A harness that always took grid 0 would grade junk title
/// regions the live scorer never selects.
fn pick_grid<'a>(
    grids: &'a [(TableGrid, TableManifest)],
    question: &serde_json::Value,
    gold_header: Option<&Vec<String>>,
) -> Option<&'a (TableGrid, TableManifest)> {
    let wanted = question["sheet_or_index"].as_str();
    let index = question["grid_index"].as_u64().unwrap_or(0) as usize;
    let gold: BTreeSet<String> = gold_header
        .map(|header| header.iter().map(|name| fold_whitespace(name)).collect())
        .unwrap_or_default();
    let mut best: Option<(f64, &'a (TableGrid, TableManifest))> = None;
    for (i, grid) in grids.iter().enumerate() {
        let mut score = 0.0;
        if wanted.is_some_and(|sheet| grid.0.sheet.as_deref() == Some(sheet)) {
            score += 1.0;
        }
        if i == index {
            score += 0.5;
        }
        if !gold.is_empty() {
            let names: BTreeSet<String> = grid
                .1
                .columns
                .iter()
                .map(|column| fold_whitespace(&column.name))
                .collect();
            score += names.intersection(&gold).count() as f64 / gold.len() as f64;
        }
        if best.is_none_or(|(previous, _)| score > previous) {
            best = Some((score, grid));
        }
    }
    best.map(|(_, grid)| grid)
}

fn fold_whitespace(text: &str) -> String {
    collapse_whitespace(&text.to_lowercase())
}

/// The closest name by token overlap, with a case-folded edit-distance
/// tie-break. Good enough to say *which* manifest column a gold name meant.
fn closest_name<'a>(gold: &str, names: &'a [String]) -> &'a str {
    let tokens = |text: &str| -> BTreeSet<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(ToString::to_string)
            .collect()
    };
    let gold_tokens = tokens(gold);
    let mut best: Option<(f64, usize, &'a str)> = None;
    for (index, name) in names.iter().enumerate() {
        let other = tokens(name);
        let intersection = gold_tokens.intersection(&other).count();
        let union = gold_tokens.union(&other).count().max(1);
        let dice = 2.0 * intersection as f64 / union as f64;
        let distance = edit_distance(&gold.to_lowercase(), &name.to_lowercase());
        let score = (dice * 1000.0) - distance.min(500) as f64;
        if best.is_none_or(|(previous, _, _)| score > previous) {
            best = Some((score, index, name));
        }
    }
    best.map(|(_, _, name)| name).unwrap_or("(no columns)")
}

fn fold(text: &str) -> String {
    collapse_whitespace(&text.to_lowercase())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn strip_parenthetical(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    collapse_whitespace(&out)
}

fn strip_suffix(text: &str) -> &str {
    text.rsplit_once(" (")
        .filter(|(_, suffix)| {
            suffix.ends_with(')')
                && suffix[..suffix.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit())
        })
        .map_or(text, |(base, _)| base)
}

fn classify(
    gold: &str,
    names: &[String],
    grid: Option<&(TableGrid, TableManifest)>,
    gold_header: Option<&Vec<String>>,
) -> (String, String) {
    let closest = closest_name(gold, names);
    let a = collapse_whitespace(gold);
    let b = collapse_whitespace(closest);
    let first = names.first().map_or("", String::as_str);
    // A whole header read from the wrong row explains most columns at once;
    // name it as such instead of calling each one "no_similar_name".
    let title_row =
        first.starts_with("Tabel") && gold_header.is_some_and(|h| h.len() > names.len() + 2);
    let data_row = first.chars().next().is_some_and(|ch| ch.is_ascii_digit());
    let preamble = closest.starts_with("Column ")
        || first.starts_with('\u{feff}')
        || first.contains("Data Source");
    let cause = if names.is_empty() {
        "no_columns_in_manifest"
    } else if a.eq_ignore_ascii_case(&b) {
        "case_only"
    } else if a == b {
        "whitespace_or_line_break"
    } else if fold(gold) == fold(closest) {
        "separator_or_punctuation"
    } else if title_row {
        "title_row_read_as_header"
    } else if data_row {
        "data_row_read_as_header"
    } else if preamble && gold_header.is_some_and(|h| h.iter().any(|n| n == gold)) {
        "csv_preamble_read_as_header"
    } else if gold.contains(" / ") != closest.contains(" / ") {
        "multi_level_flattening"
    } else if strip_suffix(&a).eq_ignore_ascii_case(strip_suffix(&b)) {
        "duplicate_suffix"
    } else if strip_parenthetical(&a).eq_ignore_ascii_case(&strip_parenthetical(&b)) {
        "unit_or_parentheses"
    } else if gold.chars().all(|c| c.is_ascii_digit()) && gold.len() == 4 {
        "year_header_parsed_as_number"
    } else if gold.starts_with("Column ") || closest.starts_with("Column ") {
        "unnamed_column"
    } else if grid.is_some_and(|(_, m)| m.columns.iter().any(|c| c.name == gold)) {
        "wrong_grid_selected"
    } else {
        "no_similar_name"
    };
    (closest.to_string(), cause.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let temp = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous + usize::from(ca != cb));
            previous = temp;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_sees_a_case_only_drift() {
        let (closest, cause) = classify("Kode Code", &["kode code".to_string()], None, None);
        assert_eq!(closest, "kode code");
        assert_eq!(cause, "case_only");
    }

    #[test]
    fn classifier_sees_a_multi_level_drift() {
        let (_, cause) = classify(
            "Country Code / ABW",
            &["Country Code".to_string()],
            None,
            None,
        );
        assert_eq!(cause, "multi_level_flattening");
    }
}
