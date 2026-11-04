//! SQL for the graph community table (`KbCommunity`).
//!
//! Added by `prisma/migrations/20261104120000_community_tables`. Additive: no
//! existing table, column, index or constraint is touched.
//!
//! # Scope is a predicate on the row
//!
//! [`SELECT_SCOPED_COMMUNITIES`] filters `"documentIds" && $2`, a Postgres array
//! overlap against the scope's document ids. A community with no in-scope
//! document is not returned, so its summary — written from documents the caller
//! may not see — cannot steer the caller's ranking. The adapter then narrows the
//! `"documentIds"` and `"chunkIds"` arrays before handing the record on, because
//! overlap says "some member is visible", not "only visible members are
//! returned".
//!
//! Every value is bound through sqlx; there is no `format!` and no interpolated
//! value in any statement here.

/// The tenant's communities that have at least one document in the scope.
///
/// Ordered by id, which is deterministic (`(tenant, version, position)`), so the
/// arm's tie-break on community order is stable across reads.
pub const SELECT_SCOPED_COMMUNITIES: &str = r#"
    SELECT "id" AS id,
           "version" AS version,
           "summary" AS summary,
           "entityKeys" AS entity_keys,
           "documentIds" AS document_ids,
           "chunkIds" AS chunk_ids,
           "model" AS model
      FROM "KbCommunity"
     WHERE "tenantId" = $1 AND "documentIds" && $2
     ORDER BY "id"
"#;

/// The version a tenant's community set is currently at. Missing row means `0`.
pub const SELECT_COMMUNITY_VERSION: &str = r#"
    SELECT COALESCE(MAX("version"), 0) FROM "KbCommunity" WHERE "tenantId" = $1
"#;

/// Delete the tenant's previous set inside the replace transaction.
pub const DELETE_TENANT_COMMUNITIES: &str = r#"
    DELETE FROM "KbCommunity" WHERE "tenantId" = $1
"#;

/// One community row. The id is derived from `(tenant, version, position)`, so
/// a rebuild of the same graph writes the same ids and a re-run is idempotent.
pub const INSERT_COMMUNITY: &str = r#"
    INSERT INTO "KbCommunity"
        ("id", "tenantId", "version", "summary", "entityKeys", "documentIds", "chunkIds", "model", "createdAt")
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
"#;

/// Every entity mention of one tenant, joined to the entity's key and name.
///
/// The build command's read, not a retrieval path: it is tenant-wide on purpose
/// because an operator is building the tenant's community set, and there is no
/// caller whose scope could narrow it. `m."tenantId"` and `e."tenantId"` are both
/// predicates so a join can never pair rows across tenants.
pub const SELECT_TENANT_MENTIONS: &str = r#"
    SELECT e."key" AS entity_key,
           e."name" AS name,
           m."chunkId" AS chunk_id,
           m."documentId" AS document_id
      FROM "KbMention" m
      JOIN "KbEntity" e ON e."id" = m."entityId" AND e."tenantId" = m."tenantId"
     WHERE m."tenantId" = $1
     ORDER BY m."documentId", m."chunkId", e."key"
"#;
