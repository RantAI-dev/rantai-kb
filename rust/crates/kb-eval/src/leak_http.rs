//! The HTTP half of the leak instrument: seed, probe, clean up.
//!
//! Everything here is black-box, exactly like the eval harness: the only thing
//! it knows is the public API of a running `kb-server`. That is the point —
//! the same probe runs against any backend without a line changed.
//!
//! Nothing here decides what a leak is ([`crate::leak`] does), which is why a
//! fake server can be made to leak in a test and have the instrument say so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::leak::{
    Answer, Arm, Attribution, Canaries, LeakExample, LeakMatrix, Observation, ProbeKind, Scope,
    SeedDocument, SeedPlan, leaks_in, plan, positive_hit, tenant_allowed,
};

/// How many corpus rows a seed writes per knowledge base, before the two
/// categories are crossed in.
pub const DOCUMENTS_PER_KNOWLEDGE_BASE: usize = 2;

/// Knowledge bases and categories per tenant: 2 × 2 gives four documents, which
/// is the smallest matrix where every scope type has something outside it.
pub const KNOWLEDGE_BASES_PER_TENANT: usize = 2;
pub const CATEGORIES_PER_TENANT: usize = 2;

/// Everything `kb-server eval leak` needs.
#[derive(Debug, Clone)]
pub struct LeakConfig {
    pub base_url: String,
    /// Full-access key of the first tenant.
    pub api_key: String,
    /// Full-access key of a *different* tenant. Without it the tenant row of
    /// the matrix has no out-of-scope probe to make and is reported UNTESTED.
    pub peer_key: Option<String>,
    pub out_dir: PathBuf,
    pub max_chunks: u32,
    pub job_timeout: Duration,
    pub poll_interval: Duration,
    /// Seed for the canary tokens. Same seed, same canaries.
    pub canary_seed: u64,
    /// Leave the seeded corpus behind.
    pub keep: bool,
    /// Also exit non-zero when an arm is UNTESTED.
    pub strict: bool,
    /// Seed the corpus as PDFs instead of markdown. The page-image arm renders
    /// only PDFs, so its row has nothing to find without this.
    pub pdf_corpus: bool,
    /// Seed the corpus as `.docx` files, each holding a picture whose alt text
    /// is the figure canary (`ING-5`). The picture becomes a figure chunk with
    /// an asset key, so the figure arm and the asset route have something to
    /// find without a layout extractor or a vision model.
    pub docx_corpus: bool,
}

impl LeakConfig {
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            peer_key: None,
            out_dir: PathBuf::from("eval-out/leak"),
            max_chunks: 20,
            job_timeout: Duration::from_secs(900),
            poll_interval: Duration::from_millis(400),
            canary_seed: 0x1EA4_C0DE_2026_0001,
            keep: false,
            strict: false,
            pdf_corpus: false,
            docx_corpus: false,
        }
    }
}

/// Failures of the instrument itself (not of the server).
#[derive(Debug, thiserror::Error)]
pub enum LeakError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("seed: {0}")]
    Seed(String),
}

/// What a run produced.
pub struct LeakRun {
    pub matrix: LeakMatrix,
    pub plan: SeedPlan,
    /// True when the run must exit non-zero.
    pub failed: bool,
    pub out_dir: PathBuf,
}

// ─── deterministic canaries ──────────────────────────────────────────────────

/// A few syllables' worth of invented roots, so a canary reads like a word to
/// a tokenizer and a human but appears nowhere else in the corpus.
const ROOTS: [&str; 8] = [
    "vorla", "kesel", "anzhir", "brikal", "tumne", "zhelvi", "okrast", "pirlu",
];

/// SplitMix64: deterministic, dependency-free, and enough for tokens that only
/// have to be unique within one run.
fn mix(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The canaries planted in one seeded document.
///
/// Unique by construction: the token and the root both carry the document's
/// index, so no two documents in a run — or in two runs with the same seed —
/// can collide.
#[must_use]
pub fn canaries_for(seed: u64, index: usize) -> Canaries {
    let noise = mix(seed ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let root_a = ROOTS[(noise as usize) % ROOTS.len()];
    let root_b = ROOTS[((noise >> 8) as usize) % ROOTS.len()];
    let digits = noise % 10_000;
    Canaries {
        bm25: format!("{root_a}qjx{digits:04}"),
        vector: format!(
            "The {root_a}wright commission reconvened at dawn to appraise the tariff of \
             {root_b}qjx{digits:04}, a figure no ledger had recorded."
        ),
        entity: format!(
            "{}-{} {} of the {} Accord",
            root_a.to_uppercase(),
            digits,
            "Ivory",
            root_b
        ),
        figure: format!(
            "Figure {digits}: the {root_a}wright reconciliation curve against {root_b} volume"
        ),
        neighbour: format!(
            "In the annex to the same minute, the {root_b}ward comptroller noted that the \
             {root_a}qjx{digits:04} adjustment had been applied twice."
        ),
        // Letters only: the global arm's query must classify as aggregative,
        // and a letters-and-digits token would make it an exact query instead.
        community: format!("{root_a}marker{}", index_letters(index)),
    }
}

/// `0 -> a`, `1 -> b`, … so the community canary carries no digits.
fn index_letters(index: usize) -> String {
    index
        .to_string()
        .chars()
        .map(|digit| {
            let value = digit.to_digit(10).unwrap_or(0);
            char::from(b'a' + u8::try_from(value).unwrap_or(0))
        })
        .collect()
}

/// The markdown body planted in one seeded document.
///
/// Long enough to be cut into several chunks, so the neighbour arm has something
/// to weave, and every canary appears verbatim in it.
#[must_use]
pub fn document_body(index: usize, canaries: &Canaries, seed: u64) -> String {
    let noise = mix(seed ^ index as u64);
    let filler: Vec<&str> = ["ledger", "tariff", "harbour", "quarry", "warden", "sluice"]
        .iter()
        .map(|_| ROOTS[(noise as usize) % ROOTS.len()])
        .collect();
    format!(
        "# Minute {index}\n\n\
         ## 1. Scope\n\n\
         {vector}\n\n\
         The {f0} clerk recorded the same figure twice, once against the harbour account and \
         once against the quarry account. The warden of the {f1} district asked for the \
         original minute book, which was not in the {f2} room that day, and the sluice \
         keeper swore the ledger had been sealed since the autumn. Nobody could say who had \
         signed the reconciliation, and the {f3} copy carried no name at all.\n\n\
         ## 2. Figures\n\n\
         {figure}\n\n\
         The curve was reproduced in the annex and then withdrawn, because the {f4} series \
         behind it had been corrected twice. Reference token for retrieval: {bm25}.\n\n\
         ## 3. Annex\n\n\
         {neighbour}\n\n\
         Named after the {entity} accord, which the {f5} delegates signed without amendment. \
         The {f0} archive holds one further copy, filed under the old numbering, and its \
         marginal note refers to a {f1} inspection that no register mentions.\n",
        vector = canaries.vector,
        figure = canaries.figure,
        neighbour = canaries.neighbour,
        bm25 = canaries.bm25,
        entity = canaries.entity,
        f0 = filler[0],
        f1 = filler[1],
        f2 = filler[2],
        f3 = filler[3],
        f4 = filler[4],
        f5 = filler[5],
    )
}

/// The same document as [`document_body`], as a two-page PDF with a real text
/// layer: sections 1–2 on the first page, the annex on the second.
///
/// Two pages, so the page arm has more than one page per document to rank and
/// the canaries are spread over both: the vector and figure canaries on page 1,
/// the neighbour and entity canaries on page 2.
#[must_use]
pub fn document_pdf(index: usize, canaries: &Canaries, seed: u64) -> Vec<u8> {
    let body = document_body(index, canaries, seed);
    let (first, second) = body
        .split_once("## 3. Annex")
        .map_or((body.as_str(), ""), |(a, b)| (a, b));
    let lines = |text: &str, heading: Option<&str>| -> Vec<String> {
        heading
            .map(str::to_string)
            .into_iter()
            .chain(
                text.lines()
                    .map(|l| l.trim_start_matches('#').trim().to_string()),
            )
            .collect()
    };
    crate::pdf::text_pdf(&[lines(first, None), lines(second, Some("3. Annex"))])
}

/// A seeded `.docx`: the document's text as paragraphs, *without* the figure
/// canary, plus one picture whose alt text is the figure canary. A figure-arm
/// hit can then only come from the picture's own chunk.
#[must_use]
pub fn document_docx(index: usize, canaries: &Canaries, seed: u64) -> Vec<u8> {
    use std::io::Write as _;
    let body = document_body(index, canaries, seed).replace(&canaries.figure, "");
    let escape = |t: &str| {
        t.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let paragraphs: String = body
        .lines()
        .map(|l| l.trim_start_matches('#').trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                escape(l)
            )
        })
        .collect();
    let picture = format!(
        "<w:p><w:r><w:drawing><wp:inline><wp:extent cx=\"2743200\" cy=\"2743200\"/>\
         <wp:docPr id=\"1\" name=\"Picture 1\" descr=\"{}\"/><a:graphic><a:graphicData \
         uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr>\
         <pic:cNvPr id=\"0\" name=\"figure.png\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill>\
         <a:blip r:embed=\"rId5\"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData>\
         </a:graphic></wp:inline></w:drawing></w:r></w:p>",
        escape(&canaries.figure)
    );
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <w:body>{paragraphs}{picture}</w:body></w:document>"
    );
    let parts: [(&str, Vec<u8>); 5] = [
        (
            "[Content_Types].xml",
            br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_vec(),
        ),
        (
            "_rels/.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_vec(),
        ),
        (
            "word/_rels/document.xml.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/figure.png"/></Relationships>"#.to_vec(),
        ),
        ("word/document.xml", document.into_bytes()),
        ("word/media/figure.png", canary_png(index)),
    ];
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in parts {
            // Writing to memory cannot fail short of allocation failure.
            if zip.start_file(name, options).is_ok() {
                let _ = zip.write_all(&bytes);
            }
        }
        let _ = zip.finish();
    }
    out.into_inner()
}

/// A 256 × 256 picture, different per document, large enough that the visual
/// stage's size filter keeps it.
fn canary_png(index: usize) -> Vec<u8> {
    let shade = u8::try_from(index % 200).unwrap_or(0);
    let img = image::RgbImage::from_fn(256, 256, |x, y| {
        image::Rgb([
            shade,
            u8::try_from(x % 256).unwrap_or(0),
            u8::try_from(y % 256).unwrap_or(0),
        ])
    });
    let mut png = Vec::new();
    let _ = image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    png
}

// ─── HTTP plumbing ───────────────────────────────────────────────────────────

fn bearer(key: &str) -> String {
    format!("Bearer {key}")
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

#[derive(Debug, Deserialize)]
struct UploadAnswer {
    #[serde(default, rename = "id")]
    id: String,
    #[serde(default, rename = "jobId")]
    job_id: String,
}

#[derive(Debug, Deserialize)]
struct JobAnswer {
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreatedKb {
    id: String,
    #[serde(default, rename = "tenantId")]
    tenant_id: String,
}

#[derive(Debug, Default, Deserialize)]
struct CreatedCategory {
    #[serde(default)]
    category: CreatedCategoryInner,
}

#[derive(Debug, Default, Deserialize)]
struct CreatedCategoryInner {
    #[serde(default)]
    id: String,
}

async fn json_post(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: &Value,
) -> Result<(u16, Value), LeakError> {
    let response = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, bearer(key))
        .json(body)
        .send()
        .await?;
    let status = response.status().as_u16();
    let parsed: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, parsed))
}

async fn server_version(client: &reqwest::Client, base_url: &str) -> Option<String> {
    let response = client
        .get(endpoint(base_url, "/health"))
        .send()
        .await
        .ok()?;
    let body: Value = response.json().await.ok()?;
    body.get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ─── seeding ─────────────────────────────────────────────────────────────────

/// Create the matrix corpus: knowledge bases, categories, and one document per
/// (knowledge base, category) cell, each carrying its own canaries.
///
/// Returns the seeded documents; the tenant ids are discovered from the server
/// (the knowledge-base row carries `tenantId`) rather than assumed from the
/// key, because nothing in the API tells a caller which tenant it belongs to.
pub async fn seed(
    client: &reqwest::Client,
    config: &LeakConfig,
) -> Result<(Vec<SeedDocument>, BTreeMap<String, String>), LeakError> {
    let mut tenants: Vec<(&str, &str)> = vec![("A", config.api_key.as_str())];
    if let Some(peer) = config.peer_key.as_deref() {
        tenants.push(("B", peer));
    }

    let mut documents = Vec::new();
    // tenant id -> the key that authenticates it.
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    let mut index = 0usize;

    for (label, key) in tenants {
        let mut tenant_id = String::new();
        let mut knowledge_bases = Vec::new();
        for kb in 0..KNOWLEDGE_BASES_PER_TENANT {
            let (status, body) = json_post(
                client,
                &endpoint(&config.base_url, "/v1/knowledge-bases"),
                key,
                &json!({"name": format!("leak-{label}-kb{kb}")}),
            )
            .await?;
            if status != 201 {
                return Err(LeakError::Seed(format!(
                    "creating knowledge base for tenant {label} returned {status}: {body}"
                )));
            }
            let created: CreatedKb = serde_json::from_value(body).map_err(|e| {
                LeakError::Seed(format!(
                    "unreadable knowledge-base answer for tenant {label}: {e}"
                ))
            })?;
            if created.tenant_id.is_empty() {
                return Err(LeakError::Seed(format!(
                    "the server did not report a tenantId for the knowledge base of {label}"
                )));
            }
            tenant_id = created.tenant_id.clone();
            keys.insert(created.tenant_id.clone(), key.to_string());
            knowledge_bases.push(created.id);
        }

        let mut categories = Vec::new();
        for cat in 0..CATEGORIES_PER_TENANT {
            let (status, body) = json_post(
                client,
                &endpoint(&config.base_url, "/v1/categories"),
                key,
                &json!({
                    "name": format!("leak-{label}-cat{cat}"),
                    "label": format!("Leak {label} category {cat}"),
                }),
            )
            .await?;
            if status != 201 {
                return Err(LeakError::Seed(format!(
                    "creating category for tenant {label} returned {status}: {body}"
                )));
            }
            let created: CreatedCategory = serde_json::from_value(body).map_err(|e| {
                LeakError::Seed(format!(
                    "unreadable category answer for tenant {label}: {e}"
                ))
            })?;
            categories.push(created.category.id);
        }

        for kb in &knowledge_bases {
            for category in &categories {
                let canaries = canaries_for(config.canary_seed, index);
                let body = document_body(index, &canaries, config.canary_seed);
                let document = upload_and_wait(
                    client, config, key, &tenant_id, kb, category, index, &canaries, &body,
                )
                .await?;
                documents.push(document);
                index += 1;
            }
        }
    }

    Ok((documents, keys))
}

/// Upload one seeded document and wait for its ingest job.
#[allow(clippy::too_many_arguments)]
async fn upload_and_wait(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    tenant_id: &str,
    knowledge_base_id: &str,
    category_id: &str,
    index: usize,
    canaries: &Canaries,
    body: &str,
) -> Result<SeedDocument, LeakError> {
    let (file_name, part) = if config.docx_corpus {
        let file_name = format!("leak-{index:02}.docx");
        let part =
            reqwest::multipart::Part::bytes(document_docx(index, canaries, config.canary_seed))
                .file_name(file_name.clone())
                .mime_str(
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                )?;
        (file_name, part)
    } else if config.pdf_corpus {
        let file_name = format!("leak-{index:02}.pdf");
        let part =
            reqwest::multipart::Part::bytes(document_pdf(index, canaries, config.canary_seed))
                .file_name(file_name.clone())
                .mime_str("application/pdf")?;
        (file_name, part)
    } else {
        let file_name = format!("leak-{index:02}.md");
        let part = reqwest::multipart::Part::text(body.to_string()).file_name(file_name.clone());
        (file_name, part)
    };
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("title", format!("Leak canary {index:02}"))
        .text("categories", category_id.to_string())
        .text("externalRef", format!("leak-ref-{index:02}"))
        .text("knowledgeBaseIds", knowledge_base_id.to_string());

    let response = client
        .post(endpoint(&config.base_url, "/v1/documents"))
        .header(reqwest::header::AUTHORIZATION, bearer(key))
        .multipart(form)
        .send()
        .await?;
    let status = response.status().as_u16();
    if status != 202 {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        return Err(LeakError::Seed(format!(
            "upload of {file_name} returned {status}: {body}"
        )));
    }
    let uploaded: UploadAnswer = response.json().await?;
    if uploaded.job_id.is_empty() || uploaded.id.is_empty() {
        return Err(LeakError::Seed(format!(
            "upload of {file_name} answered without an id/jobId"
        )));
    }
    poll_job(client, config, key, &uploaded.job_id).await?;
    let detail = document_detail(client, config, key, &uploaded.id).await?;
    let figures = detail
        .get("figures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let figure_asset_key = figures
        .first()
        .and_then(|f| f.get("assetKey"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(SeedDocument {
        id: uploaded.id,
        tenant: tenant_id.to_string(),
        knowledge_base_id: knowledge_base_id.to_string(),
        category_id: category_id.to_string(),
        external_ref: format!("leak-ref-{index:02}"),
        canaries: canaries.clone(),
        n_chunks: detail
            .get("document")
            .and_then(|d| d.get("chunkCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        has_figures: !figures.is_empty(),
        figure_asset_key,
    })
}

async fn poll_job(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    job_id: &str,
) -> Result<(), LeakError> {
    let deadline = Instant::now() + config.job_timeout;
    loop {
        let response = client
            .get(endpoint(&config.base_url, &format!("/v1/jobs/{job_id}")))
            .header(reqwest::header::AUTHORIZATION, bearer(key))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(LeakError::Seed(format!(
                "job {job_id} returned HTTP {}",
                response.status()
            )));
        }
        let job: JobAnswer = response.json().await?;
        match job.status.as_str() {
            "success" => return Ok(()),
            "failed" => {
                return Err(LeakError::Seed(format!(
                    "job {job_id} failed: {}",
                    job.error.as_deref().unwrap_or("(no error)")
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(LeakError::Seed(format!("job {job_id} timed out")));
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

async fn document_detail(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    id: &str,
) -> Result<Value, LeakError> {
    let response = client
        .get(endpoint(&config.base_url, &format!("/v1/documents/{id}")))
        .header(reqwest::header::AUTHORIZATION, bearer(key))
        .send()
        .await?;
    Ok(response.json().await.unwrap_or(Value::Null))
}

// ─── probing ─────────────────────────────────────────────────────────────────

/// The search body a scope selects.
#[must_use]
pub fn search_body(scope: &Scope, query: &str, max_chunks: u32) -> Value {
    let mut body = json!({"query": query, "maxChunks": max_chunks});
    if !scope.knowledge_base_ids.is_empty() {
        body["knowledgeBaseIds"] = json!(scope.knowledge_base_ids);
    }
    if let Some(category) = &scope.category {
        body["category"] = json!(category);
    }
    if let Some(document_ids) = &scope.document_ids {
        body["documentIds"] = json!(document_ids);
    }
    if let Some(external_refs) = &scope.external_refs {
        body["externalRefs"] = json!(external_refs);
    }
    body
}

/// The arms a returned chunk says it came from, as the server reports them.
///
/// Step S.1c: the server names them in `retrieval.arms` (`dense`, `sparse`,
/// `neighbour`, `figure`). `Some(vec![…])` means the server answered, `Some` with
/// nothing in it means the key was there but empty, and `None` means there was no
/// `retrieval` block at all — an older server, or a route that answers with no
/// chunks. Only the last case falls back to the shape heuristic, and the caller
/// reports how often that happened.
#[must_use]
pub fn provenance_arms(chunk: &Value) -> Option<Vec<Arm>> {
    let arms = chunk.get("retrieval")?.get("arms")?.as_array()?;
    let mut out: Vec<Arm> = Vec::new();
    for label in arms.iter().filter_map(Value::as_str) {
        if let Some(arm) = Arm::from_wire(label) {
            if !out.contains(&arm) {
                out.push(arm);
            }
        }
    }
    Some(out)
}

/// Which arm a returned chunk came from, when the server did not say.
///
/// The pre-S.1c heuristic: `ChunkDto` marked a keyword hit by omitting
/// `chunkType`, `page` and `assetKey`, a vector hit always carried `chunkType`,
/// and a figure hit carried an `assetKey`. Kept because a Bun-era server — or a
/// mixed fleet mid-cutover — still answers this way, and a matrix that could not
/// read it at all would be worse than one that reads it honestly and says so.
#[must_use]
pub fn arm_of_chunk(chunk: &Value) -> Arm {
    let asset = chunk
        .get("assetKey")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty());
    if asset.is_some() {
        return Arm::Figure;
    }
    if chunk.get("chunkType").is_some_and(|t| !t.is_null()) {
        return Arm::Vector;
    }
    Arm::Bm25
}

/// Which arms one chunk is attributed to, and how that was decided.
///
/// The `retrieval` block is authoritative whenever it is there, *including* when
/// it names arms this harness does not know: the shape heuristic's premise is
/// that no server said which arm produced a chunk, and a server that said so has
/// outgrown that premise. Falling back anyway would file a future arm's hits
/// under whichever shape they happen to have.
#[must_use]
pub fn arms_of_chunk(chunk: &Value) -> (Vec<Arm>, Attribution) {
    match provenance_arms(chunk) {
        Some(arms) => {
            // A figure is identified by what it carries, not only by the phase
            // that retrieved it: a figure chunk the dense arm ranked is still a
            // figure, and the figure arm's question is whether a foreign
            // document's figure came back at all.
            let mut arms = arms;
            if chunk
                .get("assetKey")
                .and_then(Value::as_str)
                .is_some_and(|k| !k.is_empty())
                && !arms.contains(&Arm::Figure)
            {
                arms.push(Arm::Figure);
            }
            (arms, Attribution::Provenance)
        }
        _ => (vec![arm_of_chunk(chunk)], Attribution::Heuristic),
    }
}

/// Whether one chunk counts towards `arm`'s cell.
#[must_use]
pub fn chunk_belongs_to_arm(chunk: &Value, arm: Arm) -> bool {
    let (arms, _) = arms_of_chunk(chunk);
    arms.contains(&arm)
}

/// Whether an answer carried the `retrieval` block at all, i.e. whether its arms
/// could be attributed without the chunk-shape heuristic.
#[must_use]
pub fn answer_has_provenance(body: &Value) -> bool {
    body.get("chunks")
        .and_then(Value::as_array)
        .is_some_and(|chunks| chunks.iter().any(|c| c.get("retrieval").is_some()))
}

/// The documents named by a search answer that belong to `arm`.
#[must_use]
pub fn documents_for_arm(body: &Value, arm: Arm) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: Option<&str>| {
        if let Some(id) = id {
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    };
    if let Some(chunks) = body.get("chunks").and_then(Value::as_array) {
        for chunk in chunks {
            // The neighbour arm takes every chunk: a woven neighbour is
            // indistinguishable from any other chunk for containment purposes, so
            // this is the widest containment check the API allows.
            if matches!(arm, Arm::Neighbour) || chunk_belongs_to_arm(chunk, arm) {
                push(chunk.get("documentId").and_then(Value::as_str));
            }
        }
    }
    if arm == Arm::Figure {
        if let Some(sources) = body.get("sources").and_then(Value::as_array) {
            for source in sources {
                if source.get("assetKey").and_then(Value::as_str).is_some() {
                    push(source.get("documentId").and_then(Value::as_str));
                }
            }
        }
    }
    ids
}

/// Run every planned probe and return the finished matrix.
pub async fn probe(
    client: &reqwest::Client,
    config: &LeakConfig,
    seed_plan: &SeedPlan,
    keys: &BTreeMap<String, String>,
    mut matrix: LeakMatrix,
) -> LeakMatrix {
    for probe in &seed_plan.probes {
        let Some(scope) = seed_plan.scope(probe.scope) else {
            continue;
        };
        let Some(key) = keys.get(&scope.tenant) else {
            matrix.notes.push(format!(
                "no key for tenant {}: {} probes in `{}` were skipped",
                scope.tenant,
                probe.arm.as_str(),
                scope.label
            ));
            continue;
        };
        let outcome = run_probe(
            client,
            config,
            key,
            scope,
            probe,
            seed_plan,
            config.max_chunks,
        )
        .await;
        matrix.record(Observation {
            arm: probe.arm,
            scope: scope.kind,
            positive: probe.positive,
            hit: outcome.hit,
            leaked: outcome.leaked,
            errored: outcome.answer.is_error(),
            refused: outcome.answer.refused,
            degrades: outcome.answer.degrades,
            attribution: outcome.answer.attribution,
            example: outcome.example,
        });
    }
    matrix.finish()
}

struct ProbeOutcome {
    answer: Answer,
    hit: bool,
    leaked: Vec<String>,
    example: Option<LeakExample>,
}

async fn run_probe(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    scope: &Scope,
    probe: &crate::leak::PlannedProbe,
    seed_plan: &SeedPlan,
    max_chunks: u32,
) -> ProbeOutcome {
    // A document-addressed route is authorised by tenant visibility, not by the
    // search filters, so it is judged against a different allowed set.
    let by_tenant = tenant_allowed(seed_plan, &scope.tenant);
    match probe.kind {
        ProbeKind::Search | ProbeKind::SearchOrAsset => {
            let mut outcome = search_probe(client, config, key, scope, probe, max_chunks).await;
            if probe.kind == ProbeKind::SearchOrAsset {
                let asset = asset_probe(client, config, key, probe).await;
                // A figure the scope may not see, served anyway, is a leak even
                // when the search arm answered cleanly.
                for id in leaks_in(&asset.answer, &by_tenant) {
                    outcome.leaked.push(id.clone());
                    outcome.example.get_or_insert_with(|| LeakExample {
                        arm: probe.arm,
                        scope: scope.label.clone(),
                        query: probe.query.clone(),
                        canary_owner: probe.canary_owner.clone(),
                        request: json!({"method": "GET", "path": asset.path}),
                        foreign_document_ids: vec![id.clone()],
                        response: asset
                            .answer
                            .error
                            .clone()
                            .map_or(Value::Null, |e| json!({ "error": e })),
                        degrades: Vec::new(),
                    });
                }
                if !outcome.answer.degrades.is_empty() {
                    outcome.answer.degrades.extend(asset.answer.degrades);
                }
            }
            outcome
        }
        ProbeKind::DocumentIntelligence => {
            let path = format!("/v1/documents/{}/intelligence", probe.query);
            let response = client
                .get(endpoint(&config.base_url, &path))
                .header(reqwest::header::AUTHORIZATION, bearer(key))
                .send()
                .await;
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body: Value = response.json().await.unwrap_or(Value::Null);
                    // The entity graph of a document outside the scope: naming
                    // the document at all is the leak.
                    let served = status < 400;
                    let documents = served.then(|| probe.query.clone()).into_iter().collect();
                    let answer = Answer {
                        documents,
                        http_status: status,
                        error: (status >= 500).then(|| body.to_string()),
                        // Asked for a document by name and refused: the guard
                        // fired, which is the evidence this arm needs.
                        refused: (400..500).contains(&status),
                        ..Answer::default()
                    };
                    finish(probe, scope, answer, max_chunks, &by_tenant)
                }
                Err(err) => finish(
                    probe,
                    scope,
                    Answer::failed(0, err.to_string()),
                    max_chunks,
                    &by_tenant,
                ),
            }
        }
    }
}

async fn search_probe(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    scope: &Scope,
    probe: &crate::leak::PlannedProbe,
    max_chunks: u32,
) -> ProbeOutcome {
    let body = search_body(scope, &probe.query, max_chunks);
    let response = client
        .post(endpoint(&config.base_url, "/v1/search"))
        .header(reqwest::header::AUTHORIZATION, bearer(key))
        .json(&body)
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            let parsed: Value = response.json().await.unwrap_or(Value::Null);
            let attribution = if answer_has_provenance(&parsed) {
                Attribution::Provenance
            } else {
                Attribution::Heuristic
            };
            let answer = Answer {
                attribution,
                documents: documents_for_arm(&parsed, probe.arm),
                degrades: parsed
                    .get("degrades")
                    .and_then(Value::as_array)
                    .map(|d| {
                        d.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                note: parsed
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                http_status: status,
                // A search that 4xx/5xx is a broken search, not a refusal.
                error: (status >= 400).then(|| parsed.to_string()),
                refused: false,
            };
            finish(probe, scope, answer, max_chunks, &scope.allowed)
        }
        Err(err) => finish(
            probe,
            scope,
            Answer::failed(0, err.to_string()),
            max_chunks,
            &scope.allowed,
        ),
    }
}

struct AssetOutcome {
    answer: Answer,
    path: String,
}

/// Fetch one figure asset by key, the way a caller guessing `assetKey`s would.
async fn asset_probe(
    client: &reqwest::Client,
    config: &LeakConfig,
    key: &str,
    probe: &crate::leak::PlannedProbe,
) -> AssetOutcome {
    let path = format!("/v1/documents/{}/assets", probe.query);
    let response = client
        .get(endpoint(&config.base_url, &path))
        .header(reqwest::header::AUTHORIZATION, bearer(key))
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            // Any body at all means the asset was served: a 404 is the answer a
            // correct implementation gives.
            let served = response.bytes().await.map(|b| b.len()).unwrap_or(0);
            AssetOutcome {
                answer: Answer {
                    documents: (status < 400 && served > 0)
                        .then(|| probe.query.clone())
                        .into_iter()
                        .collect(),
                    http_status: status,
                    error: (status >= 500).then(|| format!("HTTP {status}")),
                    refused: (400..500).contains(&status),
                    ..Answer::default()
                },
                path: format!("{path}?key=<assetKey>"),
            }
        }
        Err(err) => AssetOutcome {
            answer: Answer::failed(0, err.to_string()),
            path: format!("{path}?key=<assetKey>"),
        },
    }
}

fn finish(
    probe: &crate::leak::PlannedProbe,
    scope: &Scope,
    answer: Answer,
    max_chunks: u32,
    by_tenant: &[String],
) -> ProbeOutcome {
    let allowed: &[String] = match probe.kind {
        // Search arms answer inside the search scope; a document-addressed
        // route answers for the whole tenant.
        ProbeKind::Search => &scope.allowed,
        ProbeKind::SearchOrAsset | ProbeKind::DocumentIntelligence => by_tenant,
    };
    let leaked = leaks_in(&answer, allowed);
    let hit = positive_hit(&answer, &probe.canary_owner);
    let example = if leaked.is_empty() {
        None
    } else {
        Some(LeakExample {
            arm: probe.arm,
            scope: scope.label.clone(),
            query: probe.query.clone(),
            canary_owner: probe.canary_owner.clone(),
            request: match probe.kind {
                ProbeKind::DocumentIntelligence => {
                    json!({"method": "GET", "path": format!("/v1/documents/{}/intelligence", probe.query)})
                }
                _ => search_body(scope, &probe.query, max_chunks),
            },
            foreign_document_ids: leaked.clone(),
            response: json!({"status": answer.http_status, "degrades": answer.degrades, "documents": answer.documents}),
            degrades: answer.degrades.clone(),
        })
    };
    ProbeOutcome {
        answer,
        hit,
        leaked,
        example,
    }
}

// ─── the run ─────────────────────────────────────────────────────────────────

/// Seed, probe, clean up, write `leak-report.md` and `leak.json`.
pub async fn run(config: LeakConfig) -> Result<LeakRun, LeakError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut matrix = LeakMatrix::new(&config.base_url);
    matrix.server_version = server_version(&client, &config.base_url).await;

    if config.peer_key.is_none() {
        matrix.notes.push(
            "only one tenant was seeded: the cross-tenant row of the matrix has no second key \
             to probe with, so it is UNTESTED rather than passing. Pass --peer-key with a \
             key of a different tenant to close it."
                .to_string(),
        );
    }

    eprintln!("leak: seeding the matrix corpus…");
    let (documents, keys) = seed(&client, &config).await?;
    let plan = plan(documents);
    if plan.documents.is_empty() {
        return Err(LeakError::Seed("nothing was seeded".to_string()));
    }
    eprintln!(
        "leak: {} documents, {} scopes, {} probes",
        plan.documents.len(),
        plan.scopes.len(),
        plan.probes.len()
    );

    if !plan.documents.iter().any(|d| d.has_figures) {
        matrix.notes.push(
            "no seeded document produced a figure. Pass --docx: each seeded .docx holds a \
             picture whose alt text is the figure canary, which the visual stage (ING-5, \
             KB_IMAGES) indexes as a figure chunk with no model. Without it the figure arm \
             has nothing to retrieve and is reported UNTESTED rather than passing; the asset \
             route is probed only once a figure exists."
                .to_string(),
        );
    }
    if !config.pdf_corpus {
        matrix.notes.push(
            "the corpus was seeded as markdown, and the page-image arm renders only PDFs: \
             its row has no page to find and is UNTESTED whether or not the server runs the \
             arm. Pass --pdf to seed PDFs."
                .to_string(),
        );
    }
    if !plan.documents.iter().any(|d| d.n_chunks > 1) {
        matrix.notes.push(
            "every seeded document produced a single chunk, so the neighbour arm had nothing \
             to weave."
                .to_string(),
        );
    }

    let matrix = probe(&client, &config, &plan, &keys, matrix).await;

    if !config.keep {
        eprintln!("leak: removing the seeded corpus…");
        cleanup(&client, &config, &plan, &keys).await;
    }

    write_outputs(&config.out_dir, &matrix, &plan)?;
    let failed = matrix.any_leak() || (config.strict && matrix.any_untested());
    Ok(LeakRun {
        matrix,
        plan,
        failed,
        out_dir: config.out_dir.clone(),
    })
}

/// Delete what the seeder created. Best effort: a leaked row left behind must
/// not be reported as a clean teardown.
pub async fn cleanup(
    client: &reqwest::Client,
    config: &LeakConfig,
    seed_plan: &SeedPlan,
    keys: &BTreeMap<String, String>,
) {
    for document in &seed_plan.documents {
        let Some(key) = keys.get(&document.tenant) else {
            continue;
        };
        let _ = client
            .delete(endpoint(
                &config.base_url,
                &format!("/v1/documents/{}?hard=true", document.id),
            ))
            .header(reqwest::header::AUTHORIZATION, bearer(key))
            .send()
            .await;
    }
    for scope in &seed_plan.scopes {
        let Some(key) = keys.get(&scope.tenant) else {
            continue;
        };
        for kb in &scope.knowledge_base_ids {
            let _ = client
                .delete(endpoint(
                    &config.base_url,
                    &format!("/v1/knowledge-bases/{kb}"),
                ))
                .header(reqwest::header::AUTHORIZATION, bearer(key))
                .send()
                .await;
        }
        if let Some(category) = &scope.category {
            let _ = client
                .delete(endpoint(
                    &config.base_url,
                    &format!("/v1/categories/{category}"),
                ))
                .header(reqwest::header::AUTHORIZATION, bearer(key))
                .send()
                .await;
        }
    }
}

/// Write `leak-report.md` and `leak.json` into `dir`.
pub fn write_outputs(
    dir: &Path,
    matrix: &LeakMatrix,
    seed_plan: &SeedPlan,
) -> Result<(), LeakError> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join("leak-report.md"),
        crate::leak::render_markdown(matrix, seed_plan),
    )?;
    let payload = json!({
        "matrix": matrix,
        "seed": seed_plan,
    });
    std::fs::write(
        dir.join("leak.json"),
        serde_json::to_string_pretty(&payload).map_err(|e| LeakError::Seed(e.to_string()))?,
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::leak::{Scope, ScopeKind};

    fn scope(kind: ScopeKind) -> Scope {
        Scope {
            kind,
            label: "tenant=A".to_string(),
            tenant: "A".to_string(),
            allowed: vec!["a".to_string()],
            knowledge_base_ids: match kind {
                ScopeKind::KnowledgeBase => vec!["kb0".to_string()],
                _ => Vec::new(),
            },
            category: match kind {
                ScopeKind::Category => Some("cat0".to_string()),
                _ => None,
            },
            document_ids: match kind {
                ScopeKind::DocumentIds => Some(vec!["a".to_string()]),
                _ => None,
            },
            external_refs: match kind {
                ScopeKind::ExternalRef => Some(vec!["ref-a".to_string()]),
                _ => None,
            },
        }
    }

    #[test]
    fn the_search_body_carries_exactly_the_scope_the_caller_asked_for() {
        assert_eq!(
            search_body(&scope(ScopeKind::Tenant), "q", 7),
            json!({"query": "q", "maxChunks": 7})
        );
        assert_eq!(
            search_body(&scope(ScopeKind::KnowledgeBase), "q", 7),
            json!({"query": "q", "maxChunks": 7, "knowledgeBaseIds": ["kb0"]})
        );
        assert_eq!(
            search_body(&scope(ScopeKind::Category), "q", 7),
            json!({"query": "q", "maxChunks": 7, "category": "cat0"})
        );
        assert_eq!(
            search_body(&scope(ScopeKind::DocumentIds), "q", 7),
            json!({"query": "q", "maxChunks": 7, "documentIds": ["a"]})
        );
        assert_eq!(
            search_body(&scope(ScopeKind::ExternalRef), "q", 7),
            json!({"query": "q", "maxChunks": 7, "externalRefs": ["ref-a"]})
        );
    }

    #[test]
    fn the_arms_the_server_names_win_over_the_chunk_shape() {
        // Step S.1c: `retrieval.arms` is what the server says, so it decides —
        // including when it disagrees with the shape heuristic that used to.
        let hybrid = json!({
            "documentId": "d1",
            // A chunk that carries chunkType looks like a vector hit by shape,
            // but the server says the keyword arm produced it alone.
            "chunkType": "text",
            "retrieval": {"arms": ["sparse"], "sparseRank": 0, "fusedScore": 0.0164}
        });
        assert_eq!(provenance_arms(&hybrid), Some(vec![Arm::Bm25]));
        assert_eq!(arms_of_chunk(&hybrid).1, Attribution::Provenance);
        assert!(chunk_belongs_to_arm(&hybrid, Arm::Bm25));
        assert!(!chunk_belongs_to_arm(&hybrid, Arm::Vector));
    }

    #[test]
    fn a_chunk_both_arms_found_counts_for_both_cells() {
        let both = json!({
            "documentId": "d1",
            "chunkType": "text",
            "retrieval": {"arms": ["dense", "sparse"], "denseRank": 2, "sparseRank": 1}
        });
        assert!(chunk_belongs_to_arm(&both, Arm::Vector));
        assert!(chunk_belongs_to_arm(&both, Arm::Bm25));
    }

    #[test]
    fn a_woven_neighbour_is_named_rather_than_guessed() {
        let neighbour = json!({
            "documentId": "d1",
            "chunkType": "text",
            "retrieval": {"arms": ["neighbour"]}
        });
        assert!(chunk_belongs_to_arm(&neighbour, Arm::Neighbour));
        assert!(!chunk_belongs_to_arm(&neighbour, Arm::Vector));
    }

    #[test]
    fn a_figure_chunk_is_a_figure_whatever_retrieved_it() {
        // A figure the dense arm ranked is a figure: the figure arm asks whether a
        // foreign document's figure came back, not which phase fetched it.
        let dense_figure = json!({
            "documentId": "d1",
            "chunkType": "figure",
            "assetKey": "documents/A/d1/assets/1.png",
            "retrieval": {"arms": ["dense"], "denseRank": 0}
        });
        assert!(chunk_belongs_to_arm(&dense_figure, Arm::Figure));
        assert!(chunk_belongs_to_arm(&dense_figure, Arm::Vector));
        let co_retrieved = json!({
            "documentId": "d2",
            "chunkType": "figure",
            "assetKey": "documents/A/d2/assets/2.png",
            "retrieval": {"arms": ["figure"]}
        });
        assert!(chunk_belongs_to_arm(&co_retrieved, Arm::Figure));
    }

    #[test]
    fn an_answer_with_no_provenance_falls_back_to_the_shape_heuristic() {
        // A Bun-era server, or a mixed fleet mid-cutover, answers without the
        // block. The matrix must still work — and must count that it guessed.
        let old = json!({"chunks": [
            {"documentId": "vec", "chunkType": "text", "assetKey": null},
            {"documentId": "bm", "contextualPrefix": null, "similarity": 0.0},
        ]});
        assert!(!answer_has_provenance(&old));
        assert_eq!(arms_of_chunk(&old["chunks"][0]).1, Attribution::Heuristic);
        assert_eq!(arms_of_chunk(&old["chunks"][1]).1, Attribution::Heuristic);
        assert_eq!(
            documents_for_arm(&old, Arm::Vector),
            vec!["vec".to_string()]
        );
        assert_eq!(documents_for_arm(&old, Arm::Bm25), vec!["bm".to_string()]);
    }

    #[test]
    fn an_empty_answer_has_no_provenance_and_falls_back_rather_than_failing() {
        let empty = json!({"chunks": [], "sources": []});
        assert!(!answer_has_provenance(&empty));
        assert!(documents_for_arm(&empty, Arm::Vector).is_empty());
        assert!(documents_for_arm(&empty, Arm::Bm25).is_empty());
    }

    #[test]
    fn an_unknown_arm_label_is_not_silently_mapped_onto_a_known_one() {
        // A newer server may add an arm. Pretending it is the dense arm would
        // file a hit in the wrong cell, so it stays unmapped.
        let future = json!({"documentId": "d1", "retrieval": {"arms": ["late_interaction"]}});
        assert_eq!(provenance_arms(&future), Some(Vec::new()));
        assert!(!chunk_belongs_to_arm(&future, Arm::Vector));
        assert!(!chunk_belongs_to_arm(&future, Arm::Bm25));
    }

    #[test]
    fn the_arm_of_a_chunk_is_read_from_the_wire() {
        // A vector hit: chunkType is always serialised.
        let vector = json!({"documentId": "d1", "chunkType": "text", "assetKey": null});
        assert_eq!(arm_of_chunk(&vector), Arm::Vector);
        // A BM25 hit: chunkType, page and assetKey are all omitted.
        let bm25 = json!({"documentId": "d2", "contextualPrefix": null, "similarity": 0.0});
        assert_eq!(arm_of_chunk(&bm25), Arm::Bm25);
        // A figure hit carries an asset key.
        let figure = json!({"documentId": "d3", "chunkType": "figure", "assetKey": "documents/A/d3/assets/1.png"});
        assert_eq!(arm_of_chunk(&figure), Arm::Figure);
    }

    #[test]
    fn each_arm_reads_only_its_own_documents_from_one_answer() {
        let answer = json!({"chunks": [
            {"documentId": "vec", "chunkType": "text", "assetKey": null},
            {"documentId": "bm", "contextualPrefix": null},
            {"documentId": "fig", "chunkType": "figure", "assetKey": "k.png"},
        ], "sources": [
            {"documentId": "fig", "assetKey": "k.png"},
            {"documentId": "vec", "assetKey": null},
        ]});
        assert_eq!(
            documents_for_arm(&answer, Arm::Vector),
            vec!["vec".to_string()]
        );
        assert_eq!(
            documents_for_arm(&answer, Arm::Bm25),
            vec!["bm".to_string()]
        );
        assert_eq!(
            documents_for_arm(&answer, Arm::Neighbour),
            vec!["vec".to_string(), "bm".to_string(), "fig".to_string()]
        );
        // Figure reads both the chunk and the source that names an asset.
        assert_eq!(
            documents_for_arm(&answer, Arm::Figure),
            vec!["fig".to_string()]
        );
    }

    #[test]
    fn canaries_are_unique_per_document_and_stable_for_a_seed() {
        let one: Vec<String> = (0..6).map(|i| canaries_for(42, i).bm25).collect();
        let two: Vec<String> = (0..6).map(|i| canaries_for(42, i).bm25).collect();
        assert_eq!(one, two, "the same seed must give the same canaries");
        for (i, token) in one.iter().enumerate() {
            assert!(
                !one.iter()
                    .enumerate()
                    .any(|(j, other)| j != i && other == token),
                "canary {token} is not unique"
            );
        }
        // A different seed gives different tokens, so two runs do not share
        // canaries and a stale index cannot masquerade as a fresh hit.
        let other: Vec<String> = (0..6).map(|i| canaries_for(43, i).bm25).collect();
        assert_ne!(one, other);
    }

    #[test]
    fn every_canary_appears_verbatim_in_the_document_body() {
        for index in 0..4 {
            let canaries = canaries_for(7, index);
            let body = document_body(index, &canaries, 7);
            for (arm, text) in [
                (Arm::Bm25, &canaries.bm25),
                (Arm::Vector, &canaries.vector),
                (Arm::Figure, &canaries.figure),
                (Arm::Neighbour, &canaries.neighbour),
                (Arm::Entity, &canaries.entity),
            ] {
                assert!(
                    body.contains(text.as_str()),
                    "{index}: the {arm:?} canary is missing from the body"
                );
            }
            // Long enough to be cut into more than one chunk, so the neighbour
            // arm has something to weave.
            assert!(body.len() > 900, "{index}: body was {} bytes", body.len());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod docx_corpus_tests {
    use super::*;
    use kb_ports::VisualCollector as _;

    #[tokio::test]
    async fn the_docx_canary_is_a_picture_with_the_figure_canary_as_alt_text() {
        let canaries = canaries_for(7, 3);
        let docx = document_docx(3, &canaries, 7);
        let text = kb_extract::extract_from_buffer(
            &docx,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "leak-03.docx",
        )
        .unwrap();
        assert!(
            text.contains(&canaries.bm25),
            "the text canaries are in the body"
        );
        assert!(
            !text.contains(&canaries.figure),
            "the figure canary is only on the picture"
        );
        let scan = kb_extract::LocalVisualCollector
            .collect(
                "leak-03.docx",
                "",
                &docx,
                kb_ports::VisualOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(scan.visuals.len(), 1);
        assert_eq!(
            scan.visuals[0].alt_text.as_deref(),
            Some(canaries.figure.as_str())
        );
        assert_eq!((scan.visuals[0].width, scan.visuals[0].height), (256, 256));
    }
}
