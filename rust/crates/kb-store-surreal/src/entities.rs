//! Entity and relation persistence in SurrealDB.
//!
//! Mirrors entity upserts and graph relation writes in `src/lib/ingest/index-document.ts`.

use kb_ports::{
    DocumentId, DocumentIntelligenceData, EntityStore, EntityView, PortError, RelationView,
};
use serde_json::Value;

use crate::client::SurrealChunkStore;
use crate::queries::{surql_json, surql_string, validate_document_id};

/// Sanitize relation type to valid SurrealDB table name matching TypeScript:
/// `.toUpperCase().replace(/[^A-Z0-9_]/g, "_").replace(/^_+|_+$/g, "") || "RELATED_TO"`
#[must_use]
pub fn sanitize_relation_type(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "RELATED_TO".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Escape record id for SurrealDB (e.g. `entity:uuid-with-hyphens` -> `entity:`uuid-with-hyphens``)
pub fn escape_record_id(record_id: &str) -> Result<String, PortError> {
    let trimmed = record_id.trim();
    if trimmed.is_empty() {
        return Err(PortError::Malformed("empty record id".to_string()));
    }

    if let Some((table, id)) = trimmed.split_once(':') {
        if table.is_empty() || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(PortError::Malformed(format!(
                "invalid record table name: {table}"
            )));
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(PortError::Malformed(format!(
                "invalid record id characters: {id}"
            )));
        }
        if id.contains('-') || id.contains(':') {
            Ok(format!("{table}:`{id}`"))
        } else {
            Ok(format!("{table}:{id}"))
        }
    } else {
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(PortError::Malformed(format!(
                "invalid record id characters: {trimmed}"
            )));
        }
        if trimmed.contains('-') {
            Ok(format!("`{trimmed}`"))
        } else {
            Ok(trimmed.to_string())
        }
    }
}

/// Execute UPSERT query for an extracted entity in SurrealDB.
pub async fn execute_upsert_entity(
    store: &SurrealChunkStore,
    document_id: &DocumentId,
    sanitized_name: &str,
    name: &str,
    entity_type: &str,
    confidence: f32,
    metadata: Option<&Value>,
) -> Result<(), PortError> {
    validate_document_id(&document_id.0)?;
    if sanitized_name.is_empty()
        || !sanitized_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(PortError::Malformed(format!(
            "invalid sanitized entity name: {sanitized_name}"
        )));
    }

    let meta_json = metadata.unwrap_or(&Value::Null);
    let confidence_val = if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.8
    };

    let sql = format!(
        "UPSERT entity:`{}_{}` CONTENT {{\n  name: {},\n  type: {},\n  confidence: {},\n  document_id: {},\n  file_id: {},\n  metadata: {},\n  updated_at: time::now()\n}};",
        document_id.0,
        sanitized_name,
        surql_string(name),
        surql_string(entity_type),
        confidence_val,
        surql_string(&document_id.0),
        surql_string(&document_id.0),
        surql_json(meta_json),
    );

    store.query(&sql).await?;
    Ok(())
}

/// Execute RELATE query between two entity record ids in SurrealDB.
pub async fn execute_relate_entities(
    store: &SurrealChunkStore,
    source_id: &str,
    rel_type: &str,
    target_id: &str,
    confidence: f32,
    document_id: &DocumentId,
    context: Option<&str>,
) -> Result<(), PortError> {
    validate_document_id(&document_id.0)?;
    let escaped_source = escape_record_id(source_id)?;
    let escaped_target = escape_record_id(target_id)?;
    let safe_rel_type = sanitize_relation_type(rel_type);

    let confidence_val = if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.8
    };

    let mut content_fields = vec![
        format!("confidence: {confidence_val}"),
        format!("document_id: {}", surql_string(&document_id.0)),
        "created_at: time::now()".to_string(),
    ];
    if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
        content_fields.push(format!("context: {}", surql_string(ctx)));
    }

    let sql = format!(
        "RELATE {}->{}->{} CONTENT {{\n  {}\n}};",
        escaped_source,
        safe_rel_type,
        escaped_target,
        content_fields.join(",\n  ")
    );

    store.query(&sql).await?;
    Ok(())
}

#[async_trait::async_trait]
impl EntityStore for SurrealChunkStore {
    async fn upsert_entity(
        &self,
        document_id: &DocumentId,
        sanitized_name: &str,
        name: &str,
        entity_type: &str,
        confidence: f32,
        metadata: Option<&Value>,
    ) -> Result<(), PortError> {
        execute_upsert_entity(
            self,
            document_id,
            sanitized_name,
            name,
            entity_type,
            confidence,
            metadata,
        )
        .await
    }

    async fn relate_entities(
        &self,
        source_id: &str,
        rel_type: &str,
        target_id: &str,
        confidence: f32,
        document_id: &DocumentId,
        context: Option<&str>,
    ) -> Result<(), PortError> {
        execute_relate_entities(
            self,
            source_id,
            rel_type,
            target_id,
            confidence,
            document_id,
            context,
        )
        .await
    }

    async fn get_document_intelligence(
        &self,
        document_id: &DocumentId,
    ) -> Result<DocumentIntelligenceData, PortError> {
        validate_document_id(&document_id.0)?;

        let entity_sql = format!(
            "SELECT * FROM entity WHERE document_id = {};",
            surql_string(&document_id.0)
        );
        let entity_res = self.query(&entity_sql).await?;
        let entity_rows = SurrealChunkStore::extract_rows(&entity_res);

        let mut entities = Vec::with_capacity(entity_rows.len());
        for row in entity_rows {
            let id = row
                .get("id")
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let entity_type = row
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let confidence = row.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let metadata = row
                .get("metadata")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            entities.push(EntityView {
                id,
                name,
                r#type: entity_type,
                confidence,
                metadata,
            });
        }

        let mut relations = Vec::new();
        if let Ok(db_info) = self.query("INFO FOR DB;").await {
            let tables = db_info
                .first()
                .and_then(|stmt| stmt.get("result"))
                .and_then(|r| r.get("tables"))
                .or_else(|| {
                    db_info
                        .first()
                        .and_then(|stmt| stmt.get("result"))
                        .and_then(Value::as_array)
                        .and_then(|arr| arr.first())
                        .and_then(|obj| obj.get("tables"))
                })
                .and_then(Value::as_object);

            if let Some(tables_map) = tables {
                let relation_tables: Vec<String> = tables_map
                    .keys()
                    .filter(|t| t.as_str() != "entity" && t.as_str() != "document_chunk")
                    .cloned()
                    .collect();

                for table in relation_tables {
                    let rel_sql = format!(
                        "SELECT * FROM {} WHERE document_id = {};",
                        sanitize_relation_type(&table),
                        surql_string(&document_id.0)
                    );
                    match self.query(&rel_sql).await {
                        Ok(rel_res) => {
                            let rows = SurrealChunkStore::extract_rows(&rel_res);
                            for row in rows {
                                let id = row
                                    .get("id")
                                    .map(|v| match v {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                let in_id = row
                                    .get("in")
                                    .map(|v| match v {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                let out_id = row
                                    .get("out")
                                    .map(|v| match v {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                let rel_type = row
                                    .get("relation_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or(&table)
                                    .to_string();
                                let confidence =
                                    row.get("confidence").and_then(Value::as_f64).unwrap_or(0.0)
                                        as f32;
                                let mut metadata = match row.get("metadata") {
                                    Some(Value::Object(m)) => Value::Object(m.clone()),
                                    _ => serde_json::json!({}),
                                };
                                if let Some(ctx) = row.get("context").and_then(Value::as_str) {
                                    if let Value::Object(ref mut map) = metadata {
                                        map.entry("context".to_string())
                                            .or_insert_with(|| Value::String(ctx.to_string()));
                                    }
                                }

                                relations.push(RelationView {
                                    id,
                                    from_id: in_id,
                                    out: out_id,
                                    relation_type: rel_type,
                                    confidence,
                                    metadata,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "intelligence: relation table \"{table}\" query failed: {e}"
                            );
                        }
                    }
                }
            }
        }

        Ok(DocumentIntelligenceData {
            entities,
            relations,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_relation_types() {
        assert_eq!(sanitize_relation_type("works_for"), "WORKS_FOR");
        assert_eq!(sanitize_relation_type("RELATED-TO"), "RELATED_TO");
        assert_eq!(sanitize_relation_type("___part_of___"), "PART_OF");
        assert_eq!(sanitize_relation_type("   "), "RELATED_TO");
        assert_eq!(sanitize_relation_type("calls()"), "CALLS");
    }

    #[test]
    fn escapes_record_ids() {
        assert_eq!(
            escape_record_id("entity:doc_123_alice").unwrap(),
            "entity:doc_123_alice"
        );
        assert_eq!(
            escape_record_id("entity:doc-uuid-456_bob").unwrap(),
            "entity:`doc-uuid-456_bob`"
        );
    }
}
