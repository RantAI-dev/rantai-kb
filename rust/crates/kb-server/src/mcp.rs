//! Thin Streamable-HTTP MCP endpoint (`API-4`).
//!
//! One path, `/mcp`, mounted only when `KB_MCP=on`. It accepts JSON-RPC 2.0
//! POSTs and answers with `application/json` (no SSE, no sessions). Every tool
//! delegates to the *same* shared handler function the `/v1` route uses — the
//! visibility, knowledge-base limit and version logic is never repeated here.
//!
//! Authentication is the ordinary `Authed` extractor, so a missing key is the
//! same 401 as on any other route and a tool that needs `kb:read` gets exactly
//! the scope check the HTTP route performs.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use kb_ports::AuthContext;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::routes::content::ContentQuery;
use crate::state::AppState;

/// The protocol version advertised when the client asks for one we do not support.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const SERVER_NAME: &str = "rantai-kb";

/// The `/mcp` router. Mounted by `build_router` only when `KB_MCP=on`.
pub fn router() -> Router<Arc<AppState>> {
    // A GET (or any other method) on this path is answered 405 by axum's
    // method router, which is allowed for this thin, stateless transport.
    Router::new().route("/mcp", post(handle_post))
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

// ─── JSON-RPC framing ───────────────────────────────────────────────────────

fn ok(id: Value, result: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})),
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message.into()},
        })),
    )
        .into_response()
}

fn parse_error(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {"code": -32700, "message": message.into()},
        })),
    )
        .into_response()
}

/// The JSON-RPC entry point.
pub async fn handle_post(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    body: Bytes,
) -> Response {
    let request: RpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(err) => return parse_error(err.to_string()),
    };
    if request.jsonrpc != "2.0" {
        return rpc_error(
            request.id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request: jsonrpc must be \"2.0\"",
        );
    }
    // Notifications have no id and get no response. There is no session state
    // to initialise, so `notifications/initialized` is simply acknowledged.
    let Some(id) = request.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    match request.method.as_str() {
        "initialize" => ok(id, initialize(&state, request.params.as_ref())),
        "ping" => ok(id, json!({})),
        "tools/list" => {
            let tools = serde_json::to_value(tool_definitions()).unwrap_or_else(|_| json!([]));
            ok(id, json!({"tools": tools}))
        }
        "tools/call" => call_tool(&state, &auth, id, request.params.as_ref()).await,
        other => rpc_error(id, -32601, format!("Method not found: {other}")),
    }
}

fn initialize(state: &AppState, params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = requested
        .filter(|v| SUPPORTED_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": SERVER_NAME, "version": state.version},
    })
}

// ─── Tool catalogue ─────────────────────────────────────────────────────────

/// A tool as advertised by `tools/list`.
#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[must_use]
fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "kb_search",
            description: "Search the caller's documents for a question or a keyword and get the \
                best chunks back, each with documentId, page, section, content, sources and \
                context. Use this FIRST when you do not yet know which document holds the answer. \
                Then follow the reading loop: kb_outline(documentId) to get section ids, then \
                kb_read(documentId, section) to read the whole passage, following nextCursor \
                while truncated is true. Do not re-search for the next paragraph.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The question or keywords to look for."},
                    "maxChunks": {"type": "integer", "minimum": 1, "maximum": 50,
                        "description": "How many chunks to return (default 8, max 50)."},
                    "documentIds": {"type": "array", "items": {"type": "string"},
                        "description": "Restrict the search to these document ids; only ids the key can see are searched."},
                    "pageRange": {"type": "object",
                        "properties": {"start": {"type": "integer"}, "end": {"type": "integer"}},
                        "required": ["start", "end"],
                        "description": "Inclusive 0-based page window inside exactly one documentIds entry."},
                    "section": {"type": "string",
                        "description": "An outline section id (from kb_outline) inside exactly one documentIds entry."}
                },
                "required": ["query"],
            }),
        },
        ToolDefinition {
            name: "kb_list_documents",
            description: "List the documents the key can see: id, title, pages, status, \
                knowledgeBaseIds, externalRef and sha256. Use it to discover a document id when \
                no search hit gave you one. Pass nextCursor back to get the next page; stop when \
                it is null. Once you have an id, continue with kb_outline then kb_read.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "knowledgeBaseId": {"type": "string", "description": "Only documents in this knowledge base."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200,
                        "description": "Page size (default 100, clamped to 200)."},
                    "cursor": {"type": "string", "description": "The nextCursor from a previous call."}
                },
                "required": [],
            }),
        },
        ToolDefinition {
            name: "kb_outline",
            description: "Get the table of contents of one document: ordered sections with ids, \
                titles, levels, paths and page/chunk ranges. Use it after kb_search (or \
                kb_list_documents) to turn a documentId into the section id kb_read needs. \
                This is step 2 of the reading loop search -> outline -> read.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string", "description": "The document id from a search hit or kb_list_documents."}
                },
                "required": ["documentId"],
            }),
        },
        ToolDefinition {
            name: "kb_read",
            description: "Read the stitched text of one document without searching again. Pick \
                exactly one selector: section (an id from kb_outline), pageStart + pageEnd, or \
                cursor (the nextCursor from a previous kb_read). Keep maxChars bounded (default \
                8000) and follow nextCursor while truncated is true; the slices concatenate into \
                one byte-exact read. This is step 3 of the reading loop search -> outline -> read.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {"type": "string", "description": "The document id to read."},
                    "section": {"type": "string", "description": "A section id from kb_outline."},
                    "pageStart": {"type": "integer", "description": "First 0-based page, with pageEnd."},
                    "pageEnd": {"type": "integer", "description": "Last 0-based page, with pageStart."},
                    "cursor": {"type": "string", "description": "The nextCursor of a previous kb_read."},
                    "maxChars": {"type": "integer", "minimum": 500, "maximum": 50000,
                        "description": "Character budget per slice (default 8000)."}
                },
                "required": ["documentId"],
            }),
        },
        ToolDefinition {
            name: "kb_query_table",
            description: "Run a typed query over one stored table and get exact rows plus a \
                receipt. Use it when the answer is a number in a table (from a kb_search hit's \
                tableId, or from GET /v1/tables discovery) instead of adding figures up in prose. \
                The query is never SQL: select columns and aggregations, filter, group_by, \
                order_by and limit are checked against the table's manifest. For example \
                {\"select\":[{\"column\":\"Region\"},{\"agg\":\"sum\",\"column\":\"Revenue\",\"as\":\"total\"}], \
                 \"group_by\":[\"Region\"]}.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tableId": {"type": "string", "description": "The table id from a search hit or table discovery."},
                    "query": {"type": "object",
                        "description": "The typed query: select, filter, group_by, order_by, limit."}
                },
                "required": ["tableId", "query"],
            }),
        },
    ]
}

// ─── Dispatch ───────────────────────────────────────────────────────────────

/// Why a tool call did not produce a result.
enum CallError {
    /// Bad tool params: a JSON-RPC `-32602` protocol error.
    Params(String),
    /// The shared handler failed: an `isError: true` tool result.
    Api(ApiError),
    /// A non-2xx answer from the shared handler, carrying its message.
    Failed(String),
}

impl From<ApiError> for CallError {
    fn from(err: ApiError) -> Self {
        Self::Api(err)
    }
}

fn params(message: impl Into<String>) -> CallError {
    CallError::Params(message.into())
}

async fn call_tool(
    state: &Arc<AppState>,
    auth: &AuthContext,
    id: Value,
    params: Option<&Value>,
) -> Response {
    let Some(params) = params else {
        return rpc_error(id, -32602, "tools/call needs params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "tools/call needs params.name");
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return rpc_error(id, -32602, "tools/call params.arguments must be an object");
    }
    let outcome = match name {
        "kb_search" => search(state, auth, &args).await,
        "kb_list_documents" => list_documents(state, auth, &args).await,
        "kb_outline" => outline(state, auth, &args).await,
        "kb_read" => read(state, auth, &args).await,
        "kb_query_table" => query_table(state, auth, &args).await,
        other => return rpc_error(id, -32602, format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(result) => ok(id, tool_success(result)),
        Err(CallError::Params(message)) => rpc_error(id, -32602, message),
        Err(CallError::Api(err)) => ok(id, tool_error(&api_error_message(err).await)),
        Err(CallError::Failed(message)) => ok(id, tool_error(&message)),
    }
}

/// A successful tool result: the JSON echoed as text *and* as structured content.
fn tool_success(value: Value) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": value,
    })
}

/// An API error is a tool error, not a JSON-RPC protocol error. No content is
/// included, so a 404 cannot be used as an oracle for another tenant's ids.
fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

/// Reuse the HTTP error messages rather than restating them here.
async fn api_error_message(err: ApiError) -> String {
    let response = err.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|body| {
            body.get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("HTTP {status}"))
}

fn to_value<T: Serialize>(value: T) -> Result<Value, CallError> {
    serde_json::to_value(value).map_err(|err| CallError::Api(ApiError::Internal(err.to_string())))
}

/// Convert the shared handler's HTTP answer into tool JSON. The handlers own
/// the status and body; this only decodes them.
async fn response_json(response: Response) -> Result<Value, CallError> {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|err| CallError::Api(ApiError::Internal(err.to_string())))?;
    if !(200..300).contains(&status.as_u16()) {
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|body| {
                body.get("error")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(CallError::Failed(message));
    }
    serde_json::from_slice(&bytes)
        .map_err(|err| CallError::Api(ApiError::Internal(err.to_string())))
}

// ─── Params validation (shape only, never visibility) ────────────────────────

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, CallError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| params(format!("params.arguments.{key} must be a non-empty string")))
}

fn optional_string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, CallError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| params(format!("params.arguments.{key} must be a string"))),
    }
}

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, CallError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                params(format!(
                    "params.arguments.{key} must be a non-negative integer"
                ))
            }),
    }
}

// ─── The five tools ─────────────────────────────────────────────────────────

async fn search(
    state: &Arc<AppState>,
    auth: &AuthContext,
    args: &Value,
) -> Result<Value, CallError> {
    required_string(args, "query")?;
    optional_u32(args, "maxChunks")?;
    optional_string(args, "section")?;
    if let Some(ids) = args.get("documentIds") {
        let is_strings = ids
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string));
        if !is_strings {
            return Err(params(
                "params.arguments.documentIds must be an array of strings",
            ));
        }
    }
    if let Some(range) = args.get("pageRange") {
        let object = range
            .as_object()
            .ok_or_else(|| params("params.arguments.pageRange must be an object"))?;
        let start = object
            .get("start")
            .and_then(Value::as_u64)
            .ok_or_else(|| params("params.arguments.pageRange.start must be an integer"))?;
        let end = object
            .get("end")
            .and_then(Value::as_u64)
            .ok_or_else(|| params("params.arguments.pageRange.end must be an integer"))?;
        if start > end {
            return Err(params("params.arguments.pageRange.start must be <= end"));
        }
    }
    // The argument names match `SearchRequestDto` exactly, so the shared
    // handler sees the very same JSON the HTTP route would parse.
    let response = crate::routes::search_inner(state, auth, Bytes::from(args.to_string())).await?;
    response_json(response).await
}

async fn list_documents(
    state: &Arc<AppState>,
    auth: &AuthContext,
    args: &Value,
) -> Result<Value, CallError> {
    let kb = optional_string(args, "knowledgeBaseId")?;
    let limit = optional_u32(args, "limit")?;
    let cursor = optional_string(args, "cursor")?;
    let q = crate::routes::ListDocumentsQuery {
        limit: limit.map(|n| n.to_string()),
        // The HTTP query takes a JSON array or a comma-separated list.
        knowledge_base_ids: kb.map(|id| json!([id]).to_string()),
        external_ref: None,
        cursor: cursor.map(str::to_string),
    };
    to_value(crate::routes::list_documents_inner(state, auth, q).await?)
}

async fn outline(
    state: &Arc<AppState>,
    auth: &AuthContext,
    args: &Value,
) -> Result<Value, CallError> {
    let id = required_string(args, "documentId")?;
    to_value(crate::routes::structure::get_document_structure_inner(state, auth, id).await?)
}

async fn read(state: &Arc<AppState>, auth: &AuthContext, args: &Value) -> Result<Value, CallError> {
    let id = required_string(args, "documentId")?;
    let section = optional_string(args, "section")?.map(str::to_string);
    let cursor = optional_string(args, "cursor")?.map(str::to_string);
    let page_start = optional_u32(args, "pageStart")?;
    let page_end = optional_u32(args, "pageEnd")?;
    let max_chars = optional_u32(args, "maxChars")?.map(|n| n as usize);
    let selectors = usize::from(section.is_some())
        + usize::from(cursor.is_some())
        + usize::from(page_start.is_some() || page_end.is_some());
    if selectors > 1 {
        return Err(params(
            "use exactly one of section, pageStart/pageEnd or cursor",
        ));
    }
    if page_start.is_some() != page_end.is_some() {
        return Err(params("pageStart and pageEnd must be given together"));
    }
    let query = ContentQuery {
        section,
        page_start,
        page_end,
        chunk_start: None,
        chunk_end: None,
        cursor,
        max_chars,
    };
    to_value(crate::routes::content::get_document_content_inner(state, auth, id, query).await?)
}

async fn query_table(
    state: &Arc<AppState>,
    auth: &AuthContext,
    args: &Value,
) -> Result<Value, CallError> {
    let id = required_string(args, "tableId")?;
    let query = args
        .get("query")
        .filter(|value| value.is_object())
        .ok_or_else(|| params("params.arguments.query must be an object"))?;
    let response =
        crate::routes::tables::query_table_inner(state, auth, id, Bytes::from(query.to_string()))
            .await?;
    response_json(response).await
}
