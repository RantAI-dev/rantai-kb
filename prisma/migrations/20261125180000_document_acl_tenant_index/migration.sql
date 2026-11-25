-- SEC-2d: the scope builder asks "does this tenant hold any access-listed
-- document?" before it may use predicate mode. With no index that is a
-- sequential scan of the tenant (26 ms at 100k rows, EXPLAIN ANALYZE).
--
-- A partial index on exactly that predicate is not enough by itself: the
-- planner guesses a third of all rows satisfy `cardinality(...) > 0` and keeps
-- the sequential scan. Expression statistics give it the real selectivity, and
-- the check becomes an index-only scan (0.12 ms). Both are additive. The index
-- predicate must match `SELECT_TENANT_HAS_ACL` in kb-store-pg. Needs
-- PostgreSQL 14+ (the compose stack ships 16).
CREATE INDEX "Document_tenantId_has_acl_idx"
    ON "Document"("tenantId")
    WHERE cardinality("aclPrincipals") > 0 AND "deletedAt" IS NULL;

CREATE STATISTICS "Document_acl_cardinality_stat"
    ON (cardinality("aclPrincipals")) FROM "Document";

ANALYZE "Document";
