# rantai-kb — Product

The one product document: what the product is, what it has, what it lacks, and what comes next.

Last updated 2026-11-04. The product owner confirmed the purpose and the release decisions
on 2026-11-04.

Statuses come from reading the repository and from runs whose results are committed under
`docs/eval-ab/`. Anything not run says *Not verified*. **Have** means we match or beat the
best competitor on that row; anything less is **Partial**.

---

## 1. What it is and who it is for

**rantai-kb finds the right page of an organisation's documents, with a citation, for any AI
product that asks.** It runs on whatever hardware the organisation has. One tenant never
sees another's documents, and on the local tiers nothing leaves the building.

It is the retrieval half of an assistant. The chat, the agents, the users and the UI belong
to the app that calls it.

### Who uses it

1. **RantAI's own products first.** RantAI Agents and other RantAI apps call it as a shared
   engine over HTTP (`/v1/search`, `/v1/documents`). `externalRef` lets an app map hits back
   to its own records.
2. **The open-source community.** Developers and teams who want document search as a
   service they can self-host, instead of building ingest, OCR, chunking, embedding and
   ranking themselves.

It is **not sold on its own** today. Selling it alone would need users, roles, folders and a
UI (section 3, `ACC-*` in the backlog).

### Why someone picks it

1. **It reads the page, not just the text.** Scans, charts, tables and slides are also
   searched as page images. Measured gain over text-only search: +0.035 page nDCG@10 on
   ViDoRe v3 HR and +0.044 on CS, both with 95% CIs above zero
   (`docs/eval-ab/rk-cloud-vs-kf/`).
2. **One engine from a laptop to the cloud.** Six tiers, chosen with one setting
   (`KB_TIER`), share the same API and data. `kb-dev.sh setup` reads the hardware and
   suggests one (`docs/kb-tiers.md`).
3. **Measured quality.** On MIRACL-id-200 (Indonesian), the cloud tier scores nDCG@10 0.845
   against KF main's 0.813 (+0.032 [+0.006, +0.057]). The CPU-only lite tier scores 0.802 on
   MIRACL-id-50 with a 42 ms search on 2 cores (`docs/eval-ab/lite-2core/`).
4. **Near-zero running cost.** Open-weights models run locally. Paid APIs are optional.
   Every embedding, OCR and caption result is cached on disk by model and content hash.
5. **Tenant scope is a hard rule.** Every search path is filtered to the caller's tenant,
   and a CI gate probes for cross-tenant leaks (`docs/eval-baseline.md`).

### What we measure ourselves against

| Kind | Competitors |
| --- | --- |
| Managed retrieval services | Azure AI Search, Vertex AI Search / RAG Engine, AWS Bedrock Knowledge Bases, Vectara, Ragie |
| Open source | RAGFlow, Onyx, R2R, Dify (knowledge base) |
| Our direct baseline | KF main (unmodified), run on the same labelled sets |

### Business

Open source under Apache-2.0. There is no paid edition and no hosted version.

### Stage

- **Releases:** `v0.1.0` is the last Bun release. `v0.2.0` is the Rust service: merge PR #3,
  then tag (`DEC-1`, decided 2026-11-04).
- **The Rust port:** it replaces Bun and sits on `feat/kb-sota` (PR #3, a draft, more than 280 commits
  ahead of `main`). Bun was removed from the branch in `DEC-3`, so Rust
  is the only runtime in the repo from `v0.2.0`.
- **Team:** one product owner working with AI agents (`AGENTS.md`).

---

## 2. What is in it

| Module | What a calling app does with it | State |
| --- | --- | --- |
| **Ingest** | Uploads a file (`POST /v1/documents`, 202 and a job). It is extracted, OCR'd if scanned, chunked, embedded and indexed, with progress on `/v1/jobs/:id` and `/v1/events` | Built. About 30 file types (PDF, images, Office Open XML, OpenDocument, text, Markdown, code, CSV/TSV/JSON, HTML/XML, RTF, EPUB; `kb-extract/src/mime.rs`). Legacy `.doc/.xls/.ppt` are accepted but fail to extract (`ING-1`). A 943-page PDF ingests in 71 s with simulated provider latency (`docs/eval-ab/ingest-big-pdf/`) |
| **Retrieval** | `POST /v1/search` returns chunks with document, page, score, figures, the arms that found each hit, and `degrades` when an arm failed | Built. Dense + BM25 fused, reranker, page-image arm, neighbour chunks and an entity graph. Measured numbers are in section 1 |
| **Access & Security** | Tenants, API keys (scopes `kb:read` / `kb:write`, optional knowledge-base limit), knowledge bases and categories | Partial. The tenant wall is tested in CI. The knowledge-base limit is not applied to by-ID document routes (`SEC-1`). There are no users, roles, folders or per-document permissions |
| **Deployment** | `kb-dev.sh setup` → `up <tier>` → `key`. Tiers: lite, lite-en (CPU), desk, studio, station (NVIDIA), cloud (OpenRouter key) | Partial. lite-en ran end to end on 2026-10-25. The GPU tiers are checked by `docker compose config` only. Cloud is measured through the eval harness, not through compose (`DEP-1` to `DEP-3`) |
| **Platform** | Postgres (documents, jobs, keys, graph), Qdrant (chunks, vectors, BM25, page images) and S3 for originals. `kb-server reindex` rebuilds the index from S3 | Built in Rust. Bun was removed in `v0.2.0` (`DEC-3`); `v0.1.0` is the last Bun release |

### Coverage against competitors

The capabilities someone comparing us would check, and where we stand.

- **Ours:** Have, Partial, Missing or *Not verified*.
- **Competitors:** desk research from their documentation on 2026-11-04 (sources at the end
  of this file), not hands-on testing.

| Capability | Ours | Competitors | Gap |
| --- | --- | --- | --- |
| Self-host, open licence | **Have.** Apache-2.0; CPU-only to GPU to cloud APIs | Open source: RAGFlow (Apache-2.0), Onyx (MIT community edition), R2R (MIT), Dify (Apache-2.0 with extra terms) [F1, O1, S1, D1]. Managed: Azure, Vertex and Bedrock are cloud-only; Vectara and Ragie offer private deployments, closed source [V1, R1] | — |
| Retrieval over page images (scans, charts, slides) | **Have.** Whole pages embedded as images; +0.035 / +0.044 page nDCG@10 over text-only on ViDoRe v3 HR / CS | Azure (image embeddings) and Bedrock (Nova multimodal, 2025-11) yes [A4, B3]. The open-source four embed extracted images or captions, not whole pages [F2, O5, S1, D4] | Not measured head to head against Azure or Bedrock |
| OCR of scanned documents | **Partial.** PaddleOCR-VL locally (desk and up), Mistral OCR on cloud, PP-OCRv6 on lite (sidecar never built, `DEP-1`) | RAGFlow: DeepDoc plus MinerU / Docling / PaddleOCR-VL [F1]; Azure and Vertex: layout and OCR parsers [A4, G4] | Prove lite OCR (`DEP-1`) |
| Hybrid search and reranking | **Have.** Dense + BM25 fused, local or hosted reranker | Every competitor has both [A5, G5, B5, V6, F4, O6, S1, D6] | — |
| Published retrieval numbers | **Have.** MIRACL-id-200, ViDoRe v3 HR / CS and HotpotQA runs committed with scripts (`docs/eval-ab/`) | Azure publishes nDCG@10 79.3 on MIRACL and 56.3 on BEIR [A6], on different sets from ours, so not comparable. RAGFlow ships a harness without scores [F2]. Others: none found | — |
| Tenant isolation | **Have.** One tenant per key; a CI gate probes every arm for cross-tenant leaks | Azure: index or service per tenant [A8]; Bedrock: silo, pool or bridge [B7]; Vectara: corpus per tenant [V3]; Onyx cloud: schema per tenant [O8] | — |
| Per-document permissions at search time | **Missing.** Only a key's knowledge-base limit, which is not yet applied to by-ID routes (`SEC-1`) | Vertex AI Search: per-document user / group ACLs [G2]; Azure: ACL trimming, in preview [A2]; Bedrock: SharePoint / OneDrive ACLs [B2]; Onyx: permission sync, enterprise edition only [O2] | `SEC-1`, `ACC-3` |
| Users, groups and roles | **Missing.** Key scopes `kb:read` / `kb:write` only | Vertex: IAM viewer / editor / admin [G2]; Dify: owner / admin / editor / normal [D3]; RAGFlow: owner / member [F2]; R2R: owner / member [S2] | `ACC-2` |
| Folders | **Missing.** Flat knowledge bases | RAGFlow: nested folders [F3]; Onyx: document sets, not nested [O4]; R2R: flat collections [S3] | `ACC-1` |
| File types | **Partial.** About 30 (`kb-extract/src/mime.rs`); legacy `.doc/.xls/.ppt` accepted but not extracted | R2R about 34 extensions [S1]; Azure about 30 [A3]; Onyx 16 [O7]; Dify 14 built in [D7] | `ING-1` |
| Connectors (Drive, SharePoint, …) | **Missing.** The calling app uploads files | Onyx 58 [O2]; RAGFlow about 40 [F5]; Vertex about 28 [G7]; Azure 11 [A7] | `ING-2`, Later |
| A UI for managing documents | **Missing.** HTTP API and CLI only | The open-source four all ship one | Not a goal while it is a shared engine |

---

## 3. What it does not do

Stated plainly, for anyone trying it.

**Access**
- Permissions belong to API keys, not people. There are no users, groups or roles. A
  calling app decides who may see what and hands out keys or knowledge-base filters to match.
- There are no folders. Knowledge bases are flat, and a document may be in several.
- There are no per-document permissions inside a knowledge base.
- A key limited to some knowledge bases can still open a document outside them if it has the
  ID (`SEC-1`, being fixed before the release).

**Ingest**
- It does not sync from Drive, SharePoint or other sources. The calling app uploads every
  file.
- It does not extract legacy binary Office files (`.doc`, `.xls`, `.ppt`).

**Deployment**
- The GPU tiers (desk, studio, station) have never been started on an NVIDIA machine. Their
  GPU memory splits are estimates.
- There is no UI. Everything is HTTP and the `kb-dev.sh` / `kb-server` commands.

---

## 4. What blocks the release

"Released" means: an outside user can follow the README, bring up a tier, upload a scanned
PDF and search it, without help from us.

| # | Blocker | What clears it |
| --- | --- | --- |
| 1 | By-ID document routes ignore a key's knowledge-base limit, and the repo is public | `SEC-1` |
| 2 | PR #3's retrieval gate is red (hit@1 −0.083 on the 12-query CI set) | `REL-1`: fix, or refresh the baseline with the reason |
| 3 | The lite tiers, the ones most outsiders will try first, are unproven on a fresh build with the new OCR sidecar | `DEP-1` |
| 4 | The Rust port is not on `main` | `DEC-1`, then merge PR #3 |

**Not blockers, but outside users will hit them:**

- The GPU tiers are unproven. They ship marked *Not verified* (`DEP-2`).
- There are no users or roles (`ACC-2`).

### Risks worth knowing

The Likelihood and Impact ratings are the planner's estimate.

| Risk | Why it matters | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- | --- |
| A tier we document fails for the first outside user | Open-source first impressions stick | High for GPU tiers | Medium | Mark each tier's verified status in the README; `DEP-1` to `DEP-3` |
| A permission hole in a public repo | Users trust key limits that do not hold | Medium | High | `SEC-1` before release; `SECURITY.md` for reports |
| A retrieval claim that does not reproduce | Our main selling point is measured quality | Low | High | Every number links to a committed run folder with its script |
| One owner plus AI agents | Decisions must be written down | High | Medium | This folder |

---

## 5. What is next

This is the proposed order. The product owner decides it.

### Now (the release)

| Item | Why |
| --- | --- |
| `SEC-1`, `REL-1`, `DEP-1` | The remaining release blockers in section 4 |
| `REL-3`, `REL-4` | Small, and they keep CI honest |

### Next

| Item | Why |
| --- | --- |
| `DEP-2`, `DEP-3`, `RET-1` | Prove and measure every tier, not just cloud and lite |
| `ACC-1`, `ACC-2`, `ACC-3` | What a team trying it on its own expects first; needed if it is ever sold alone |
| `EVAL-1` | Completes the KF comparison on the multimodal sets |

### Later

Connectors (`ING-2`) and parse/caption residency (`DEP-5`).

---

## 6. Decisions taken by the product owner

All three were decided on 2026-11-04.

| # | Decision | Outcome |
| --- | --- | --- |
| `DEC-1` | What the release is | Release `v0.2.0` so RantAI products can use it; merge PR #3, then tag |
| `DEC-2` | Where people (Owner, Acceptor) and the board live | GitHub Projects, so community contributors can see it |
| `DEC-3` | Retire Bun from `main` at the release | Removed. `v0.2.0` is Rust-only; `v0.1.0` stays as the last Bun tag, and `docs/rust-cutover.md` covers migration |

---

## Sources for the competitor columns

Desk research 2026-11-04, from vendor docs and GitHub. Caveats:

- The Onyx licence split comes from a third-party blog [O1]; check the LICENSE file before quoting it.
- Azure document ACLs are in preview.
- The Ragie cells come from its site, because its docs failed to load.

- [A2] https://learn.microsoft.com/azure/search/search-document-level-access-overview
- [A3] https://learn.microsoft.com/azure/search/search-howto-indexing-azure-blob-storage
- [A4] https://learn.microsoft.com/en-ca/azure/search/search-get-started-portal-image-search
- [A5] https://learn.microsoft.com/azure/search/search-relevance-overview
- [A6] https://cdn-dynmedia-1.microsoft.com/is/content/microsoftcorp/microsoft/final/en-us/microsoft-brand/documents/ai-search-query-engine-performance-whitepaper.pdf
- [A7] https://learn.microsoft.com/en-us/azure/search/search-data-sources-gallery
- [A8] https://learn.microsoft.com/azure/search/search-modeling-multitenant-saas-applications
- [G2] https://cloud.google.com/generative-ai-app-builder/docs/data-source-access-control
- [G4] https://docs.cloud.google.com/generative-ai-app-builder/docs/parse-chunk-documents
- [G5] https://cloud.google.com/vertex-ai/generative-ai/docs/retrieval-and-ranking
- [G7] https://docs.cloud.google.com/generative-ai-app-builder/docs/connect-third-party-data-source
- [B2] https://docs.aws.amazon.com/bedrock/latest/userguide/kb-managed-acl.html
- [B3] https://aws.amazon.com/about-aws/whats-new/2025/11/multimodal-retrieval-bedrock-knowledge-bases
- [B5] https://docs.aws.amazon.com/bedrock/latest/userguide/rerank-use.html
- [B7] https://aws.amazon.com/blogs/architecture/secure-multi-tenant-rag-with-amazon-bedrock-and-verified-permissions/
- [V1] https://docs.vectara.com/docs/deployments/private-deployment-overview
- [V3] https://docs.vectara.com/docs/security/authorization/personas-and-access-patterns
- [V6] https://docs.langchain.com/oss/python/integrations/providers/vectara
- [R1] https://www.ragie.ai/
- [F1] https://github.com/infiniflow/ragflow
- [F2] https://ragflow.io/docs/release_notes
- [F3] https://ragflow.io/docs/manage_files
- [F4] https://ragflow.io/docs/dev/retrieval_component
- [F5] https://ragflow.io/docs/add_data_source/data_source_categories_and_selection
- [O1] https://wz-it.com/en/blog/onyx-self-hosted-installation/
- [O2] https://docs.onyx.app/overview/core_features/connectors
- [O4] https://docs.onyx.app/developers/api_reference/projects/upload_user_files
- [O5] https://docs.onyx.app/security/architecture/data_flows.md
- [O6] https://docs.onyx.app/admins/advanced_configs/search_configs
- [O7] https://docs.onyx.app/admins/connectors/official/file
- [O8] https://docs.onyx.app/security/onyx_cloud/multi_tenant
- [S1] https://github.com/SciPhi-AI/R2R
- [S2] https://raw.githubusercontent.com/SciPhi-AI/R2R/main/llms.txt
- [S3] https://r2r-docs.sciphi.ai/cookbooks/collections
- [D1] https://github.com/langgenius/dify/blob/main/LICENSE
- [D3] https://docs.dify.ai/en/guides/management/team-members-management
- [D4] https://www.dify.ai/blog/multimodal-retrieval-is-now-available-in-the-knowledge-base
- [D6] https://docs.dify.ai/en/self-host/use-dify/knowledge/create-knowledge/setting-indexing-methods
- [D7] https://github.com/langgenius/dify (`api/constants/__init__.py`)
