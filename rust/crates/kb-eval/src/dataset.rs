//! BEIR/TREC dataset loading.
//!
//! A dataset is a directory:
//!
//! ```text
//! <dataset>/
//!   corpus/            the files to ingest; the file stem is the document key
//!   queries.jsonl      {"_id": "q1", "text": "..."} per line
//!   qrels/test.tsv     query-id<TAB>corpus-id<TAB>score
//!   answers.jsonl      optional {"_id": "...", "answer": "..."} for later
//! ```
//!
//! `corpus-id` is either `<doc-key>` (document-level) or `<doc-key>#p<N>`
//! (page-level, `N` 1-based). The loader is deliberately lenient about
//! whitespace but strict about structure, because a public benchmark's format
//! is a contract.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub id: String,
    pub text: String,
}

/// One graded relevance judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qrel {
    pub query_id: String,
    pub corpus_id: String,
    pub score: i32,
}

/// One reference answer (parsed for later answer-level evals; not scored yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub id: String,
    pub answer: String,
}

/// One corpus file, keyed by its file stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusFile {
    pub key: String,
    pub path: PathBuf,
    /// The document's own title, when the file names itself in its first
    /// line (a `# Heading`). `None` when it does not.
    ///
    /// MIRACL's corpus files are Wikipedia passages that start with
    /// `# Title`. The title belongs in the upload: a document called
    /// `1000580_1` is not findable by anything a reader would type, and
    /// without a title the engine embeds the file stem in front of every one
    /// of its chunks.
    pub title: Option<String>,
}

/// A loaded dataset.
#[derive(Debug, Clone)]
pub struct Dataset {
    /// The dataset directory's name.
    pub name: String,
    pub dir: PathBuf,
    pub queries: Vec<Query>,
    pub qrels: Vec<Qrel>,
    pub answers: Vec<Answer>,
    pub corpus: Vec<CorpusFile>,
}

impl Dataset {
    /// Load a dataset directory into memory.
    ///
    /// # Errors
    ///
    /// [`DatasetError`] when a required file is missing, unreadable, or does not
    /// parse. A dataset that loads is a dataset that is safe to run.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, DatasetError> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Err(DatasetError::NotFound(dir.to_path_buf()));
        }
        let name = dir.file_name().map_or_else(
            || dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        let corpus = load_corpus(&dir.join("corpus"))?;
        let queries_path = dir.join("queries.jsonl");
        let queries =
            parse_queries(&read(&queries_path)?).map_err(|message| DatasetError::Parse {
                path: queries_path.clone(),
                message,
            })?;
        let qrels_path = dir.join("qrels/test.tsv");
        let qrels = parse_qrels(&read(&qrels_path)?).map_err(|message| DatasetError::Parse {
            path: qrels_path.clone(),
            message,
        })?;
        let answers_path = dir.join("answers.jsonl");
        let answers = if answers_path.is_file() {
            parse_answers(&read(&answers_path)?).map_err(|message| DatasetError::Parse {
                path: answers_path.clone(),
                message,
            })?
        } else {
            Vec::new()
        };

        Ok(Self {
            name,
            dir: dir.to_path_buf(),
            queries,
            qrels,
            answers,
            corpus,
        })
    }

    /// The qrels for one query id, in file order.
    #[must_use]
    pub fn qrels_for(&self, query_id: &str) -> Vec<Qrel> {
        self.qrels
            .iter()
            .filter(|q| q.query_id == query_id)
            .cloned()
            .collect()
    }
}

fn read(path: &Path) -> Result<String, DatasetError> {
    fs::read_to_string(path).map_err(|source| DatasetError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn load_corpus(dir: &Path) -> Result<Vec<CorpusFile>, DatasetError> {
    if !dir.is_dir() {
        return Err(DatasetError::NotFound(dir.to_path_buf()));
    }
    let entries = fs::read_dir(dir).map_err(|source| DatasetError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DatasetError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem() else {
            continue;
        };
        let key = stem.to_string_lossy().into_owned();
        if key.starts_with('.') {
            continue;
        }
        files.push(CorpusFile {
            title: title_of(&path),
            key,
            path,
        });
    }
    if files.is_empty() {
        return Err(DatasetError::EmptyCorpus(dir.to_path_buf()));
    }
    // Deterministic order, so an ingest is reproducible.
    files.sort_by(|a, b| a.key.cmp(&b.key));
    // The file stem *is* the document key, so two files that share one would
    // silently overwrite each other in the ingest map and one document's chunks
    // would stop being attributable to the corpus.
    for pair in files.windows(2) {
        if pair[0].key == pair[1].key {
            return Err(DatasetError::DuplicateKey {
                key: pair[0].key.clone(),
                first: pair[0].path.clone(),
                second: pair[1].path.clone(),
            });
        }
    }
    Ok(files)
}

/// The document's title, when the corpus file names itself in its first
/// non-empty line.
///
/// `# Judul` → `Judul`. A file that does not start with a heading has no
/// title, and the upload then says nothing about one — the engine falls back
/// to the file stem, exactly as it did before.
///
/// This is deliberately a local five-line read rather than a call into
/// `kb-core`: this crate is a black-box client and may not link the engine's
/// own code (see `kb_server::boundary`).
fn title_of(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let title = trimmed.strip_prefix("# ")?.trim();
        return (!title.is_empty()).then(|| title.to_string());
    }
    None
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    _id: String,
    #[serde(default)]
    text: String,
}
#[derive(Debug, Deserialize)]
struct RawAnswer {
    _id: String,
    #[serde(default)]
    answer: String,
}

/// Parse `queries.jsonl`. A blank line is skipped; a bad line names its number.
///
/// # Errors
///
/// A message with the offending line number.
pub fn parse_queries(text: &str) -> Result<Vec<Query>, String> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawQuery = serde_json::from_str(line)
            .map_err(|e| format!("line {}: invalid query JSON: {e}", idx + 1))?;
        if raw._id.trim().is_empty() {
            return Err(format!("line {}: query has an empty _id", idx + 1));
        }
        out.push(Query {
            id: raw._id,
            text: raw.text,
        });
    }
    Ok(out)
}

/// Parse `answers.jsonl`.
///
/// # Errors
///
/// A message with the offending line number.
pub fn parse_answers(text: &str) -> Result<Vec<Answer>, String> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawAnswer = serde_json::from_str(line)
            .map_err(|e| format!("line {}: invalid answer JSON: {e}", idx + 1))?;
        out.push(Answer {
            id: raw._id,
            answer: raw.answer,
        });
    }
    Ok(out)
}

/// Which column of a qrels line holds what.
///
/// The documented shape is `query-id corpus-id score`. A header, when present,
/// is obeyed: TREC and MS MARCO files put an `iteration` column in the middle,
/// and a public benchmark that imports with the wrong column silently produces
/// numbers about nothing.
#[derive(Debug, Clone, Copy)]
struct QrelColumns {
    query: usize,
    corpus: usize,
    score: usize,
}

const DEFAULT_COLUMNS: QrelColumns = QrelColumns {
    query: 0,
    corpus: 1,
    score: 2,
};

/// Recognise a qrels header row and read the column positions off it.
fn columns_from_header(fields: &[&str]) -> Option<QrelColumns> {
    if fields.len() < 2 {
        return None;
    }
    let normalized: Vec<String> = fields
        .iter()
        .map(|f| f.to_ascii_lowercase().replace(['-', '_', ' '], ""))
        .collect();
    let position = |names: &[&str]| normalized.iter().position(|f| names.contains(&f.as_str()));
    let query = position(&["queryid", "qid", "query", "topicid"]);
    let corpus = position(&["corpusid", "docid", "documentid"]);
    let score = position(&["score", "relevance", "rel", "label", "grade"]);
    match (query, corpus, score) {
        (Some(query), Some(corpus), Some(score)) => Some(QrelColumns {
            query,
            corpus,
            score,
        }),
        // A partial header is not a header; fall back to the documented shape.
        _ => None,
    }
}

/// Parse `qrels/test.tsv`, with or without the `query-id corpus-id score` header.
///
/// Fields may be separated by tabs or spaces; blank lines and `#` comments are
/// ignored. A score may be written as an integer or as an integer-valued float
/// (`2.0`); anything fractional is rejected rather than rounded.
///
/// # Errors
///
/// A message naming the offending line number.
pub fn parse_qrels(text: &str) -> Result<Vec<Qrel>, String> {
    let mut out = Vec::new();
    let mut columns = DEFAULT_COLUMNS;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if out.is_empty() {
            if let Some(header) = columns_from_header(&fields) {
                columns = header;
                continue;
            }
        }
        let needed = columns.query.max(columns.corpus).max(columns.score);
        if fields.len() <= needed {
            return Err(format!(
                "line {}: expected {} columns (query-id, corpus-id, score), got {}",
                idx + 1,
                needed + 1,
                fields.len()
            ));
        }
        let raw = fields[columns.score];
        let score = parse_grade(raw).ok_or_else(|| {
            format!(
                "line {}: score {raw:?} is not a whole relevance grade",
                idx + 1
            )
        })?;
        out.push(Qrel {
            query_id: fields[columns.query].to_string(),
            corpus_id: fields[columns.corpus].to_string(),
            score,
        });
    }
    Ok(out)
}

/// A relevance grade, tolerating the `2.0` spelling some exports use.
fn parse_grade(raw: &str) -> Option<i32> {
    if let Ok(value) = raw.parse::<i32>() {
        return Some(value);
    }
    let value = raw.parse::<f64>().ok()?;
    (value.fract() == 0.0 && value >= i32::MIN as f64 && value <= i32::MAX as f64)
        .then_some(value as i32)
}

/// The document key of a corpus id, stripping a `#p<N>` page suffix.
#[must_use]
pub fn base_doc_key(corpus_id: &str) -> &str {
    match corpus_id.rsplit_once("#p") {
        Some((doc, page)) if page.parse::<u32>().is_ok() => doc,
        _ => corpus_id,
    }
}

/// The 1-based page number of a `#p<N>` corpus id, if it has one.
#[must_use]
pub fn page_of(corpus_id: &str) -> Option<u32> {
    let (_, page) = corpus_id.rsplit_once("#p")?;
    page.parse::<u32>().ok().filter(|&p| p > 0)
}

/// The page-level corpus id for a document key and 1-based page.
#[must_use]
pub fn page_key(doc_key: &str, page_1based: u32) -> String {
    format!("{doc_key}#p{page_1based}")
}

/// Document-level gold for one query: page judgements collapse onto their
/// document, and the highest grade wins.
#[must_use]
pub fn gold_for_documents(qrels: &[Qrel]) -> HashMap<String, i32> {
    let mut gold: HashMap<String, i32> = HashMap::new();
    for qrel in qrels {
        let entry = gold.entry(base_doc_key(&qrel.corpus_id).to_string());
        let current = entry.or_insert(qrel.score);
        *current = (*current).max(qrel.score);
    }
    gold
}

/// Page-level gold for one query: only explicit `#p<N>` judgements count.
#[must_use]
pub fn gold_for_pages(qrels: &[Qrel]) -> HashMap<String, i32> {
    qrels
        .iter()
        .filter(|q| page_of(&q.corpus_id).is_some())
        .map(|q| (q.corpus_id.clone(), q.score))
        .collect()
}

/// Dataset loading errors.
#[derive(Debug, thiserror::Error)]
pub enum DatasetError {
    #[error("dataset path is not a directory: {0}")]
    NotFound(PathBuf),
    #[error("corpus directory has no files: {0}")]
    EmptyCorpus(PathBuf),
    #[error("two corpus files share the document key {key}: {first} and {second}")]
    DuplicateKey {
        key: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not parse {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_queries_and_skips_blanks() {
        let text = "{\"_id\":\"q1\",\"text\":\"alpha\"}\n\n{\"_id\":\"q2\",\"text\":\"beta\"}\n";
        let queries = parse_queries(text).unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(
            queries[0],
            Query {
                id: "q1".into(),
                text: "alpha".into()
            }
        );
        assert_eq!(queries[1].id, "q2");
    }

    #[test]
    fn a_bad_query_line_names_its_number() {
        let err = parse_queries("{\"_id\":\"q1\",\"text\":\"a\"}\nnot json\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn parses_qrels_with_header() {
        let text = "query-id\tcorpus-id\tscore\nq1\tdoc-a#p2\t3\nq1\tdoc-b\t1\n";
        let qrels = parse_qrels(text).unwrap();
        assert_eq!(qrels.len(), 2);
        assert_eq!(qrels[0].query_id, "q1");
        assert_eq!(qrels[0].corpus_id, "doc-a#p2");
        assert_eq!(qrels[0].score, 3);
        assert_eq!(qrels[1].corpus_id, "doc-b");
    }

    #[test]
    fn parses_qrels_without_header_and_with_spaces() {
        let qrels = parse_qrels("q1 doc-a 2\nq1 doc-b 0\n").unwrap();
        assert_eq!(qrels.len(), 2);
        assert_eq!(qrels[0].score, 2);
        assert_eq!(qrels[1].score, 0);
    }

    #[test]
    fn a_trec_header_with_an_iteration_column_is_read_in_the_right_places() {
        // MS MARCO / TREC shape: query-id, iteration, corpus-id, relevance.
        let text = "query-id\titeration\tcorpus-id\trelevance\nq1\t0\tdoc-a\t3\nq2\t0\tdoc-b\t1\n";
        let qrels = parse_qrels(text).unwrap();
        assert_eq!(qrels.len(), 2);
        assert_eq!(qrels[0].query_id, "q1");
        assert_eq!(qrels[0].corpus_id, "doc-a");
        assert_eq!(qrels[0].score, 3);
        assert_eq!(qrels[1].corpus_id, "doc-b");
    }

    #[test]
    fn a_data_line_is_never_mistaken_for_a_header() {
        // "qid docid rel" is the column-name shape, not a query named "qid".
        let qrels = parse_qrels("q1 doc-a 2\n").unwrap();
        assert_eq!(qrels.len(), 1);
        assert_eq!(qrels[0].query_id, "q1");
    }

    #[test]
    fn an_integer_valued_float_score_is_accepted_and_a_real_one_is_not() {
        let qrels = parse_qrels("q1\tdoc-a\t2.0\n").unwrap();
        assert_eq!(qrels[0].score, 2);
        let err = parse_qrels("q1\tdoc-a\t2.5\n").unwrap_err();
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn a_short_data_line_names_its_line() {
        let err = parse_qrels("query-id\tcorpus-id\tscore\nq1\tdoc-a\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn qrels_rejects_a_non_integer_score() {
        let err = parse_qrels("q1\tdoc-a\thigh\n").unwrap_err();
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn corpus_ids_split_into_doc_and_page() {
        assert_eq!(base_doc_key("doc-a#p2"), "doc-a");
        assert_eq!(page_of("doc-a#p2"), Some(2));
        assert_eq!(base_doc_key("doc-a"), "doc-a");
        assert_eq!(page_of("doc-a"), None);
        // A `#p` that is not a page number stays part of the key.
        assert_eq!(base_doc_key("doc#pnotanumber"), "doc#pnotanumber");
        assert_eq!(page_of("doc#p0"), None);
        assert_eq!(page_key("doc-a", 3), "doc-a#p3");
    }

    #[test]
    fn document_gold_collapses_pages_and_keeps_the_max() {
        let qrels = vec![
            Qrel {
                query_id: "q1".into(),
                corpus_id: "doc-a#p2".into(),
                score: 3,
            },
            Qrel {
                query_id: "q1".into(),
                corpus_id: "doc-a#p5".into(),
                score: 1,
            },
            Qrel {
                query_id: "q1".into(),
                corpus_id: "doc-b".into(),
                score: 2,
            },
        ];
        let gold = gold_for_documents(&qrels);
        assert_eq!(gold.get("doc-a"), Some(&3));
        assert_eq!(gold.get("doc-b"), Some(&2));
    }

    #[test]
    fn page_gold_keeps_only_explicit_pages() {
        let qrels = vec![
            Qrel {
                query_id: "q1".into(),
                corpus_id: "doc-a#p2".into(),
                score: 3,
            },
            Qrel {
                query_id: "q1".into(),
                corpus_id: "doc-b".into(),
                score: 2,
            },
        ];
        let gold = gold_for_pages(&qrels);
        assert_eq!(gold.len(), 1);
        assert_eq!(gold.get("doc-a#p2"), Some(&3));
    }

    #[test]
    fn a_missing_dataset_is_an_error() {
        let err = Dataset::load("/nonexistent/kb-eval-dataset").unwrap_err();
        assert!(matches!(err, DatasetError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn a_corpus_file_that_names_itself_in_the_first_line_has_a_title() {
        let dir = std::env::temp_dir().join(format!("kb-eval-title-{}", std::process::id()));
        let corpus = dir.join("corpus");
        fs::create_dir_all(&corpus).unwrap();
        fs::write(corpus.join("1000580_1.md"), "# DPTools\n\nbody").unwrap();
        fs::write(corpus.join("plain.txt"), "no heading here").unwrap();
        fs::write(corpus.join("sub.md"), "\n\n## Sub\n\nbody").unwrap();
        let files = load_corpus(&corpus).unwrap();
        let title = |key: &str| {
            files
                .iter()
                .find(|f| f.key == key)
                .and_then(|f| f.title.clone())
        };
        assert_eq!(title("1000580_1").as_deref(), Some("DPTools"));
        assert_eq!(title("plain"), None);
        assert_eq!(title("sub"), None, "an H2 does not name the document");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_corpus_files_with_one_stem_are_refused() {
        let dir = std::env::temp_dir().join(format!("kb-eval-dup-{}", std::process::id()));
        let corpus = dir.join("corpus");
        fs::create_dir_all(&corpus).unwrap();
        fs::write(corpus.join("report.md"), "a").unwrap();
        fs::write(corpus.join("report.txt"), "b").unwrap();
        let err = load_corpus(&corpus).unwrap_err();
        assert!(matches!(err, DatasetError::DuplicateKey { .. }), "{err:?}");
        assert!(err.to_string().contains("report"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
