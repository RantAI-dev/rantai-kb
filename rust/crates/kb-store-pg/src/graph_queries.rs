//! SQL for the entity graph tables (`KbEntity`, `KbMention`, `KbRelation`,
//! `KbGraphVersion`).
//!
//! Kept beside the adapter that uses them rather than in `queries.rs`, because
//! these statements are a different shape from the document ones: they are batched,
//! they are scope-filtered, and every one of them is written so the *scope filter
//! is on the row*, not on the result.
//!
//! The two reads that build a graph are the ones worth reading closely. Both take
//! `"documentId" = ANY($2)` over the scope's document ids, which is not an
//! optimisation detail — it is the scope rule. A relation whose document is outside
//! the scope is not returned, so it cannot become a path through the graph
//! (`kb_ports::graph`), and a mention outside the scope cannot keep an entity alive
//! in it.

/// One entity, upserted by `(tenant, key)`.
///
/// Aliases merge rather than replace: a re-ingest that happens not to know an alias
/// must not delete it, so the new set is unioned with the stored one. `key` is the
/// identity, which is what makes a re-ingest idempotent.
///
/// The union is `DISTINCT` and drops empty strings, and that is not decoration: the
/// in-memory reference merges into a `BTreeSet`, so an adapter that kept duplicates
/// or an empty alias would store a different alias list from the reference on the
/// same input. The port does not expose aliases yet, so nothing observes the
/// difference today — which is precisely why it has to be right now rather than when
/// the alias surface lands.
///
/// `unnest` sits in a subquery rather than in the `SELECT DISTINCT` list itself
/// because that spelling fails at runtime, not at parse time: a set-returning
/// function in a `SELECT DISTINCT` list yields an anonymous composite, and Postgres
/// rejects it with *"input of anonymous composite types is not implemented"*. The
/// subquery names the unnested column, so `DISTINCT` applies to the text values.
pub const UPSERT_ENTITY: &str = r#"
    INSERT INTO "KbEntity" ("id", "tenantId", "key", "name", "type", "aliases", "updatedAt")
    VALUES ($1, $2, $3, $4, $5, $6, now())
    ON CONFLICT ("tenantId", "key") DO UPDATE SET
        "name" = EXCLUDED."name",
        "type" = EXCLUDED."type",
        "aliases" = COALESCE((
            SELECT array_agg(alias ORDER BY alias)
              FROM (
                    SELECT DISTINCT u AS alias
                      FROM unnest(EXCLUDED."aliases" || "KbEntity"."aliases") AS u
                   ) merged
             WHERE alias <> ''
        ), '{}'),
        "updatedAt" = now()
    RETURNING "id"
"#;

/// One mention. `ON CONFLICT DO NOTHING` because the unique key is
/// `(tenant, entity, chunk)` and a re-ingest writes the same rows again.
pub const INSERT_MENTION: &str = r#"
    INSERT INTO "KbMention" ("id", "tenantId", "entityId", "chunkId", "documentId")
    VALUES ($1, $2, $3, $4, $5)
    ON CONFLICT ("tenantId", "entityId", "chunkId") DO NOTHING
"#;

/// One relation. The id is derived from `(tenant, src, dst, type, document)`, so a
/// re-ingest of the same document replaces rather than duplicates; a *different*
/// document that asserts the same relation is a separate row, because it is
/// separate evidence and it is scoped to that document.
pub const INSERT_RELATION: &str = r#"
    INSERT INTO "KbRelation"
        ("id", "tenantId", "srcEntityId", "dstEntityId", "type", "weight", "chunkId", "documentId")
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
    ON CONFLICT ("id") DO UPDATE SET
        "weight" = EXCLUDED."weight",
        "chunkId" = EXCLUDED."chunkId"
"#;

/// Which of a batch's relation endpoints this tenant actually has. A batch with a
/// missing endpoint is rejected whole, before anything is written.
pub const COUNT_EXISTING_ENTITIES: &str = r#"
    SELECT "key" FROM "KbEntity" WHERE "tenantId" = $1 AND "key" = ANY($2)
"#;

/// Bump the tenant's version. `0` is written on first write, so a tenant that has
/// never been written reads back as a version that is greater than any cache entry
/// a reader could have taken before it.
pub const BUMP_VERSION: &str = r#"
    INSERT INTO "KbGraphVersion" ("tenantId", "version") VALUES ($1, 1)
    ON CONFLICT ("tenantId") DO UPDATE SET "version" = "KbGraphVersion"."version" + 1
"#;

/// Read the tenant's version. Missing row means "never written".
pub const SELECT_VERSION: &str = r#"
    SELECT "version" FROM "KbGraphVersion" WHERE "tenantId" = $1
"#;

/// Entity ids the document mentioned, captured before the mentions are deleted so
/// the orphan sweep knows which entities to reconsider.
pub const SELECT_DOCUMENT_MENTIONED_ENTITY_IDS: &str = r#"
    SELECT DISTINCT "entityId" FROM "KbMention"
     WHERE "tenantId" = $1 AND "documentId" = $2
"#;

/// Delete the document's relations before its mentions, so no query ever sees a
/// mention whose relation is already gone.
pub const DELETE_DOCUMENT_RELATIONS: &str = r#"
    DELETE FROM "KbRelation" WHERE "tenantId" = $1 AND "documentId" = $2
"#;

/// Delete the document's mentions.
pub const DELETE_DOCUMENT_MENTIONS: &str = r#"
    DELETE FROM "KbMention" WHERE "tenantId" = $1 AND "documentId" = $2
"#;

/// Delete entities this document mentioned that nothing mentions any more.
///
/// The `NOT EXISTS` is what makes it an orphan sweep rather than a mass delete: an
/// entity still mentioned by another document survives, and an entity that was
/// never mentioned at all was never a candidate in the first place — it is not in
/// the id list, which is why the ids were captured first.
pub const DELETE_ORPHANED_ENTITIES: &str = r#"
    DELETE FROM "KbEntity" e
     WHERE e."id" = ANY($2)
       AND NOT EXISTS (SELECT 1 FROM "KbMention" m WHERE m."entityId" = e."id")
"#;

/// The scoped graph's entities: id and key, for entities with at least one mention
/// inside the scope.
///
/// This is the *only* table the walk resolves ids against, so it is also the scope
/// rule: an entity with no in-scope mention has no row here, therefore no node id,
/// therefore cannot be an endpoint of anything in the scoped graph.
pub const SELECT_SCOPED_ENTITIES: &str = r#"
    SELECT e."id" AS id, e."key" AS entity_key
      FROM "KbEntity" e
     WHERE e."tenantId" = $1
       AND EXISTS (
           SELECT 1 FROM "KbMention" m
            WHERE m."entityId" = e."id" AND m."tenantId" = e."tenantId"
              AND m."documentId" = ANY($2)
       )
"#;

/// The scoped graph's mention rows: entity id plus chunk id. No join and no
/// `ORDER BY`.
///
/// Both are deliberate, and both were measured. Joining `KbEntity` here costs an
/// index lookup per row, and sorting 100k rows by text key costs half a second:
/// `load_tenant_graph` on a 100k-relation tenant went from 628 ms to the number
/// below by taking the join and the sort out of SQL. Determinism does not need
/// them — the CSR merges parallel edges with `w_max`, which is order-independent,
/// and node ids come from a sorted `NodeMap` — so the ordering is re-established
/// in Rust, where it is cheap and cannot disagree with the walk.
pub const SELECT_SCOPED_MENTION_ROWS: &str = r#"
    SELECT m."entityId" AS entity_id, m."chunkId" AS chunk_id
      FROM "KbMention" m
     WHERE m."tenantId" = $1 AND m."documentId" = ANY($2)
"#;

/// The scoped graph's relation rows: both endpoint ids and the weight. No join and
/// no `ORDER BY`, for the reasons on [`SELECT_SCOPED_MENTION_ROWS`].
pub const SELECT_SCOPED_RELATION_ROWS: &str = r#"
    SELECT r."srcEntityId" AS src_id, r."dstEntityId" AS dst_id, r."weight" AS weight
      FROM "KbRelation" r
     WHERE r."tenantId" = $1 AND r."documentId" = ANY($2)
"#;

/// Seed lookup: name and aliases of every entity with an in-scope mention.
///
/// The `EXISTS` clause is the scope filter. Matching itself is done in Rust so the
/// Postgres and in-memory adapters agree on what "matches" means, down to the
/// ordering — a semantic difference here would make the contract suite's
/// equivalence checks lie.
pub const SELECT_SCOPED_ENTITY_LABELS: &str = r#"
    SELECT e."key" AS entity_key, e."name" AS name, e."aliases" AS aliases
      FROM "KbEntity" e
     WHERE e."tenantId" = $1
       AND EXISTS (
           SELECT 1 FROM "KbMention" m
            WHERE m."entityId" = e."id" AND m."tenantId" = e."tenantId"
              AND m."documentId" = ANY($2)
       )
     ORDER BY e."key"
"#;

/// One entity's scoped mentions, ascending by chunk id.
pub const SELECT_SCOPED_ENTITY_MENTIONS: &str = r#"
    SELECT m."chunkId" AS chunk_id
      FROM "KbMention" m
      JOIN "KbEntity" e ON e."id" = m."entityId" AND e."tenantId" = m."tenantId"
     WHERE m."tenantId" = $1 AND e."key" = $2 AND m."documentId" = ANY($3)
     ORDER BY m."chunkId"
"#;
