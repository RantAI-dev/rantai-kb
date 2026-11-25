# Security model

In plain words: **permissions belong to API keys, not people.** A key is the tenant and,
optionally, the knowledge bases and read/write scope it may touch. A calling app decides
who may see what and hands out keys or knowledge-base filters to match.

- **Tenants.** Every API key belongs to exactly one tenant. Every search arm, document
  route and asset route is filtered to that tenant. A CI gate probes the retrieval arms for
  cross-tenant leaks (`rust/scripts/ci-eval-gate.sh`), and there is no route that reads
  across tenants.
- **Key scopes.** A key can be limited to `kb:read` and/or `kb:write`; an empty scope list
  means full access (the CLI prints `(all)`). `rantai-kb key <tenant>` mints an unrestricted
  key; `kb-server create-api-key --scopes kb:read,kb:write` mints a narrower one.
- **Knowledge-base limit.** A key can be limited to a set of knowledge bases. The limit
  applies to search, the document list, upload and metadata edits — and to every by-ID read,
  download, edit and delete, which answers `404` for a document outside the limit rather
  than confirming it exists (`SEC-1`, decided and built in v0.2.0). A document in no
  knowledge base is not visible to a limited key at all.
- **Uploads are narrowed, never trusted.** A `knowledgeBaseIds` value for another tenant's
  knowledge base is dropped instead of creating a cross-tenant link.
- **The stores are not the API.** Postgres, Qdrant and the blob store hold no per-request
  identity: they are addressed by the server, on the caller's behalf, with the tenant filter
  already applied. Exposing them to a caller is a deployment mistake, not a design option.
- **Per-document access lists (`SEC-3`, v0.9.0).** A document can carry `aclPrincipals`
  (for example `["group:hr"]`). rantai-kb does not know your users; the calling app asserts
  the current user's principals per request with `X-KB-Principals`, and only a key with the
  privileged scope `kb:assert-principals` may do that (any other key gets `403`), so a
  leaked ordinary key cannot claim `group:admin`. An ACL'd document is visible only when the
  asserted principals overlap its list, and is hidden when none are asserted
  (**default-deny**). The rule lives in the one Postgres visibility statement that scopes
  every retrieval arm, the document list, every by-ID route, tables and `/mcp`.
- **Postgres is the authority, Qdrant is a cache.** With predicate scope on
  (`KB_SCOPE_ID_LIMIT`, opt-in, `SEC-2a`) Qdrant pre-filters on payload, but every candidate
  is re-verified against Postgres before it can be returned, and a failed verify fails the
  search. A stale payload can cost recall, never visibility.
- **What it does not do.** There are no users, groups, roles or folders managed *inside*
  rantai-kb: principals are opaque strings asserted by the calling app, which owns identity.
  See [`docs/core/PRODUCT.md` §3](core/PRODUCT.md#3-what-it-does-not-do).

Tenant isolation is covered by tests rather than by review: the retrieval scope gate fails
CI on a cross-tenant hit, and the by-ID limit has its own regression test
([spec](core/specs/sec-1.md)).

Report a vulnerability privately; see [`SECURITY.md`](../SECURITY.md).
