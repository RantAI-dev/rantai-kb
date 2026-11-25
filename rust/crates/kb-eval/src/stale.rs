//! Pure planning and classification for the stale-payload leak matrix (SEC-2c).
//!
//! The live runner in [`crate::leak_http`] connects to Postgres and injects the
//! drift; everything here is a string builder or a data transform that can be
//! tested without a database or a server. IDs are always bound parameters in
//! the SQL, never interpolated into the statement.

use serde_json::Value;

use crate::leak::{Arm, PlannedProbe, SeedDocument, SeedPlan, canary_query, has_the_right_corpus};

/// Move D from KB-1 to KB-2 in Postgres only. No outbox row is written, so the
/// Qdrant payload still claims KB-1.
pub fn move_kb_sql() -> &'static str {
    r#"UPDATE "DocumentGroup" SET "knowledgeBaseId" = $2
       WHERE "documentId" = $1 AND "knowledgeBaseId" = $3"#
}

/// Soft-delete E in Postgres only. Qdrant still holds its chunk.
pub fn soft_delete_sql() -> &'static str {
    r#"UPDATE "Document" SET "deletedAt" = now(), "updatedAt" = now() WHERE "id" = $1"#
}

/// Put an ACL on F in Postgres only. A tenant with an ACL'd document forces
/// Ids mode in `predicate_spec_for`, so this case proves the Ids path.
pub fn set_acl_sql() -> &'static str {
    r#"UPDATE "Document" SET "aclPrincipals" = $2::text[] WHERE "id" = $1"#
}

/// Drop any pending repair so the reconciler cannot heal the injected drift
/// before the probes run.
pub fn delete_outbox_sql() -> &'static str {
    r#"DELETE FROM "ScopeOutbox" WHERE "documentId" = $1"#
}

/// Mint an additional key for the stale run. The hash is what the server
/// looks up; scopes and KB restrictions are bound arrays.
pub fn insert_key_sql() -> &'static str {
    r#"INSERT INTO "ApiKey" ("id", "tenantId", "name", "hash", "scopes",
        "knowledgeBaseIds", "createdAt")
       VALUES ($1, $2, $3, $4, $5, $6, now())"#
}

/// All SQL the live runner executes, with what each statement is for.
pub fn stale_statements() -> [(&'static str, &'static str); 5] {
    [
        (move_kb_sql(), "move D from KB-1 to KB-2 without a repair"),
        (soft_delete_sql(), "soft-delete E without a repair"),
        (set_acl_sql(), "ACL F without a repair"),
        (delete_outbox_sql(), "suppress the outbox repair"),
        (
            insert_key_sql(),
            "mint the KB-limited / principal-less keys",
        ),
    ]
}

/// SHA-256 hex of a plaintext key, matching `kb-store-pg::hash_api_key`.
pub fn hash_key(plaintext: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(plaintext.as_bytes()))
}

/// A key plaintext. The server only stores the hash, so the exact suffix does
/// not matter — it only has to be unique per run.
pub fn mint_key(tenant: &str, nonce: &str) -> String {
    format!("rkb_stale_{tenant}_{nonce}")
}

/// When `/mcp` cannot be probed, say why rather than marking it a pass.
pub fn mcp_unavailable(status: u16, body: &Value) -> Option<String> {
    if status == 404 || status == 405 {
        return Some("/mcp answered 404/405: the endpoint is not mounted (KB_MCP=off)".to_string());
    }
    if let Some(error) = body.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        if code == -32601 || message.contains("Method not found") {
            return Some(format!("/mcp rejected the JSON-RPC method: {message}"));
        }
    }
    let result_is_error = body
        .get("result")
        .and_then(|r| r.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if result_is_error {
        let text = body
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("unknown tool");
        if text.contains("unknown tool") {
            return Some(format!("/mcp does not serve this tool: {text}"));
        }
    }
    None
}

/// Stale targets that must never appear in an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTargets {
    /// Document moved from `moved_from_kb` to another KB in Postgres only (D).
    pub moved: String,
    pub moved_from_kb: String,
    /// KB D is moved into; it must already exist in D's tenant (foreign key).
    pub moved_to_kb: String,
    /// Soft-deleted document (E).
    pub deleted: String,
    /// ACL'd document, invisible without a principal (F).
    pub acl: String,
    pub principal: String,
}

/// Choose D, E and F from seeded documents, deterministically. D and E come
/// from the first tenant, F from the second; F is table-bearing when possible
/// so the table row can actually see it.
pub fn pick_targets(documents: &[SeedDocument]) -> Option<StaleTargets> {
    let mut tenants: Vec<&str> = documents.iter().map(|d| d.tenant.as_str()).collect();
    tenants.sort_unstable();
    tenants.dedup();
    let first = *tenants.first()?;
    let second = *tenants.get(1)?;
    let prose = |tenant: &str| -> Vec<&SeedDocument> {
        documents
            .iter()
            .filter(|d| d.tenant == tenant && d.table_ids.is_empty())
            .collect()
    };
    let first_prose = prose(first);
    let moved = *first_prose.first()?;
    let deleted = first_prose.iter().find(|d| d.id != moved.id).copied()?;
    let moved_to_kb = documents
        .iter()
        .find(|d| d.tenant == first && d.knowledge_base_id != moved.knowledge_base_id)?
        .knowledge_base_id
        .clone();
    let acl = documents
        .iter()
        .find(|d| d.tenant == second && !d.table_ids.is_empty())
        .or_else(|| prose(second).first().copied())?;
    Some(StaleTargets {
        moved: moved.id.clone(),
        moved_from_kb: moved.knowledge_base_id.clone(),
        moved_to_kb,
        deleted: deleted.id.clone(),
        acl: acl.id.clone(),
        principal: "user:42".to_string(),
    })
}

/// Forbidden ids present in an answer, deduplicated.
pub fn forbidden_in(documents: &[String], forbidden: &[String]) -> Vec<String> {
    let mut seen = Vec::new();
    for id in forbidden {
        if documents.iter().any(|found| found == id) && !seen.contains(id) {
            seen.push(id.clone());
        }
    }
    seen
}

/// Keys used by the stale probes, per target.
#[derive(Debug, Clone)]
pub struct StaleKeys {
    /// Restricted to the KB D was moved out of.
    pub kb_limited: String,
    /// No principals asserted, tenant A.
    pub plain_first: String,
    /// No principals asserted, tenant B.
    pub plain_second: String,
}

/// Build the targeted matrix: every arm × (D, E, F), each with a positive
/// control from a sibling the caller may still see.
pub fn build_stale_plan(base: &SeedPlan, targets: &StaleTargets, keys: &StaleKeys) -> SeedPlan {
    let mut scopes = base.scopes.clone();
    for scope in &mut scopes {
        scope.allowed.retain(|id| {
            if id == &targets.deleted || id == &targets.acl {
                return false;
            }
            !(id == &targets.moved && scope.knowledge_base_ids.contains(&targets.moved_from_kb))
        });
    }

    let tenant_of = |id: &str| {
        base.documents
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.tenant.clone())
            .unwrap_or_default()
    };
    let kb_of = |id: &str| {
        base.documents
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.knowledge_base_id.clone())
            .unwrap_or_default()
    };

    let cases: Vec<(String, String, String)> = vec![
        (
            targets.moved.clone(),
            keys.kb_limited.clone(),
            targets.moved_from_kb.clone(),
        ),
        (
            targets.deleted.clone(),
            keys.plain_first.clone(),
            kb_of(&targets.deleted),
        ),
        (
            targets.acl.clone(),
            keys.plain_second.clone(),
            kb_of(&targets.acl),
        ),
    ];

    let mut probes = Vec::new();
    for (target_id, key, kb) in &cases {
        let tenant = tenant_of(target_id);
        let Some(document) = base.document(target_id) else {
            continue;
        };
        let scope_index = scopes
            .iter()
            .position(|scope| {
                scope.tenant == tenant
                    && scope.kind == crate::leak::ScopeKind::KnowledgeBase
                    && scope.knowledge_base_ids.iter().any(|k| k == kb)
            })
            .or_else(|| scopes.iter().position(|scope| scope.tenant == tenant));
        let Some(scope_index) = scope_index else {
            continue;
        };
        let Some(scope) = scopes.get(scope_index) else {
            continue;
        };
        for arm in Arm::ALL {
            probes.push(PlannedProbe {
                arm,
                kind: arm.probe_kind(),
                scope: scope_index,
                query: canary_query(document, arm),
                canary_owner: target_id.clone(),
                positive: false,
                key_override: Some(key.clone()),
                forbidden: Some(vec![target_id.clone()]),
            });
            if let Some(sibling) = scope
                .allowed
                .iter()
                .filter_map(|id| base.document(id))
                .find(|d| d.id != *target_id && has_the_right_corpus(d, arm))
            {
                probes.push(PlannedProbe {
                    arm,
                    kind: arm.probe_kind(),
                    scope: scope_index,
                    query: canary_query(sibling, arm),
                    canary_owner: sibling.id.clone(),
                    positive: true,
                    key_override: Some(key.clone()),
                    forbidden: None,
                });
            }
        }
    }

    SeedPlan {
        documents: base.documents.clone(),
        scopes,
        probes,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::leak::{Canaries, ScopeKind};

    fn doc(id: &str, tenant: &str, kb: &str, table: bool) -> SeedDocument {
        SeedDocument {
            id: id.to_string(),
            tenant: tenant.to_string(),
            knowledge_base_id: kb.to_string(),
            category_id: "cat".to_string(),
            external_ref: format!("ref-{id}"),
            canaries: Canaries {
                bm25: format!("bm-{id}"),
                vector: format!("vector sentence for {id}"),
                entity: format!("Entity-{id}"),
                figure: format!("Figure: {id}"),
                neighbour: format!("neighbour of {id}"),
                community: format!("marker{id}"),
            },
            n_chunks: 2,
            has_figures: false,
            figure_asset_key: None,
            table_ids: if table {
                vec![format!("{id}_t0")]
            } else {
                Vec::new()
            },
        }
    }

    fn plan_with_four() -> SeedPlan {
        let documents = vec![
            doc("a-kb0", "A", "kb0", false),
            doc("a-kb1", "A", "kb1", false),
            doc("a-kb1b", "A", "kb1", false),
            doc("a-kb1-table", "A", "kb1", true),
            doc("a-kb0b", "A", "kb0", false),
            doc("a-kb0-table", "A", "kb0", true),
            doc("b-kb0", "B", "kb0", false),
            doc("b-kb0-table", "B", "kb0", true),
            doc("b-kb0-table2", "B", "kb0", true),
            doc("b-kb1", "B", "kb1", false),
        ];
        crate::leak::plan(documents)
    }

    #[test]
    fn the_stale_sql_binds_ids_and_names_the_authoritative_tables() {
        assert!(move_kb_sql().contains("\"DocumentGroup\""));
        assert!(move_kb_sql().contains("\"knowledgeBaseId\" = $2"));
        assert!(soft_delete_sql().contains("\"deletedAt\" = now()"));
        assert!(set_acl_sql().contains("\"aclPrincipals\" = $2::text[]"));
        assert!(delete_outbox_sql().contains("\"ScopeOutbox\""));
        assert!(insert_key_sql().contains("\"knowledgeBaseIds\""));
        for (sql, why) in stale_statements() {
            assert!(!why.is_empty());
            assert!(sql.contains('$'), "every id must be a bind: {sql}");
        }
    }

    #[test]
    fn the_key_hash_is_deterministic_and_hex() {
        let a = hash_key("rkb_test_key");
        let b = hash_key("rkb_test_key");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(a, hash_key("rkb_other_key"));
        assert!(mint_key("tenant-a", "42").starts_with("rkb_stale_tenant-a_"));
    }

    #[test]
    fn an_unmounted_mcp_is_reported_not_counted_as_a_pass() {
        assert!(
            mcp_unavailable(404, &serde_json::Value::Null)
                .unwrap()
                .contains("KB_MCP")
        );
        assert!(mcp_unavailable(405, &serde_json::Value::Null).is_some());
        assert!(mcp_unavailable(
            200,
            &serde_json::json!({"error": {"code": -32601, "message": "Method not found: tools/call"}})
        )
        .is_some());
        assert!(
            mcp_unavailable(
                200,
                &serde_json::json!({"result": {"structuredContent": {"documentId": "x"}}})
            )
            .is_none()
        );
    }

    #[test]
    fn forbidden_ids_are_classified_once_each() {
        let answer = vec![
            "a-kb0".to_string(),
            "a-kb1".to_string(),
            "a-kb0".to_string(),
        ];
        assert_eq!(
            forbidden_in(&answer, &["a-kb0".to_string(), "a-kb1".to_string()]),
            vec!["a-kb0".to_string(), "a-kb1".to_string()]
        );
        assert!(forbidden_in(&answer, &["nobody".to_string()]).is_empty());
    }

    #[test]
    fn targets_are_picked_deterministically_with_f_on_a_table_document() {
        let plan = plan_with_four();
        let targets = pick_targets(&plan.documents).unwrap();
        assert_eq!(targets.moved, "a-kb0");
        assert_eq!(targets.moved_from_kb, "kb0");
        assert_eq!(targets.moved_to_kb, "kb1");
        assert_eq!(targets.deleted, "a-kb1");
        assert_eq!(targets.acl, "b-kb0-table");
        assert_eq!(targets.principal, "user:42");
    }

    #[test]
    fn the_stale_matrix_marks_all_three_targets_forbidden_on_every_arm() {
        let base = plan_with_four();
        let targets = pick_targets(&base.documents).unwrap();
        let keys = StaleKeys {
            kb_limited: "kb-key".to_string(),
            plain_first: "plain-a".to_string(),
            plain_second: "plain-b".to_string(),
        };
        let stale = build_stale_plan(&base, &targets, &keys);
        for target in [&targets.moved, &targets.deleted, &targets.acl] {
            for arm in Arm::ALL {
                let probe = stale
                    .probes
                    .iter()
                    .find(|p| !p.positive && p.arm == arm && p.canary_owner == *target)
                    .unwrap_or_else(|| panic!("no {arm:?} leak probe for {target}"));
                assert_eq!(probe.forbidden.as_deref(), Some(&[target.to_string()][..]));
                assert!(
                    stale
                        .probes
                        .iter()
                        .any(|p| p.positive && p.arm == arm && p.scope == probe.scope),
                    "{arm:?} has no positive control for {target}"
                );
            }
        }
        // The ACL case runs under the principal-less key of the second tenant.
        let acl_probe = stale
            .probes
            .iter()
            .find(|p| !p.positive && p.canary_owner == targets.acl)
            .unwrap();
        assert_eq!(acl_probe.key_override.as_deref(), Some("plain-b"));
        assert!(
            stale
                .scopes
                .iter()
                .all(|scope| !scope.allowed.contains(&targets.deleted))
        );
        assert!(stale.scopes.iter().any(|scope| {
            scope.kind == ScopeKind::KnowledgeBase
                && !scope.allowed.contains(&targets.moved)
                && scope.knowledge_base_ids.contains(&targets.moved_from_kb)
        }));
    }
}
