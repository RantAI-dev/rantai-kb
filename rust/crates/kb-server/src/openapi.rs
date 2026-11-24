//! The OpenAPI 3.1 document of the HTTP API, and the Swagger UI that renders
//! it.
//!
//! The document is *derived from the routes*, never written twice: every
//! handler carries a `#[utoipa::path]` attribute, and the route table in
//! [`crate::routes`] lists every route the router serves. The two are checked
//! against each other by `tests/openapi_tests.rs`, so a route that exists
//! without a spec entry — or a spec entry without a route — fails the build.
//!
//! # What is public
//!
//! `GET /openapi.json` and `GET /docs` answer without a key, like `/health`
//! does: a caller has to be able to read the contract before it has a key. The
//! `/v1` routes keep exactly the authentication they have always had.
//!
//! # Offline by construction
//!
//! The UI is served from assets embedded at build time (`utoipa-swagger-ui`'s
//! `vendored` feature), so the service never reaches a CDN at runtime. That
//! matters because the service runs on-prem and inside air-gapped networks.

pub mod mirrors;

use axum::Router;
use axum::response::IntoResponse;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// The name of the security scheme for `Authorization: Bearer <key>`.
pub const BEARER_SCHEME: &str = "bearer_auth";
/// The name of the security scheme for `X-API-Key: <key>`.
pub const API_KEY_SCHEME: &str = "api_key_header";

/// The published OpenAPI document.
///
/// Every path in here is a handler's `__path_*` marker: the same handlers the
/// router serves, so the document and the router cannot drift apart without
/// the route-table test noticing.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "rantai-kb API",
        // Set for real by `VersionAddon`: a value written here is baked in by the
        // derive macro, whose output a cached build keeps after a version bump.
        version = "0.0.0",
        description = "Upload documents, search them, and read back what was found.\n\n\
                       Every route under `/v1` needs an API key: send it as `Authorization: Bearer <key>` \
                       or as `X-API-Key: <key>`. The key decides the tenant, the scopes \
                       (`kb:read`, `kb:write`) and, when it is limited, the knowledge bases the caller may see."
    ),
    paths(
        crate::routes::health,
        crate::routes::health_v1,
        crate::routes::formats,
        crate::routes::search,
        crate::routes::list_documents,
        crate::routes::upload::ingest_document,
        crate::routes::get_document,
        crate::routes::patch_document,
        crate::routes::delete_document,
        crate::routes::media::get_document_raw,
        crate::routes::structure::get_document_structure,
        crate::routes::content::get_document_content,
        crate::routes::media::get_document_asset,
        crate::routes::media::get_document_intelligence,
        crate::routes::jobs::get_job,
        crate::routes::jobs::retry_job,
        crate::routes::list_knowledge_bases,
        crate::routes::taxonomy::create_knowledge_base,
        crate::routes::taxonomy::patch_knowledge_base,
        crate::routes::taxonomy::delete_knowledge_base,
        crate::routes::taxonomy::list_categories,
        crate::routes::taxonomy::create_category,
        crate::routes::taxonomy::patch_category,
        crate::routes::taxonomy::delete_category,
        crate::events::events_stream,
        crate::routes::tables::list_tables,
        crate::routes::tables::get_table,
        crate::routes::tables::query_table,
    ),
    tags(
        (name = "documents", description = "Upload documents, list them, read one, and change or delete it."),
        (name = "search", description = "Ask a question and get the chunks, the numbered sources and the assembled context."),
        (name = "jobs", description = "Follow an ingest job's progress and requeue a failed one."),
        (name = "tables", description = "List the tables a document's extractor found and query them with a typed query."),
        (name = "taxonomy", description = "Knowledge bases and categories: the labels documents are filed under."),
        (name = "structure", description = "The pages, figures and chunk-to-page map of one document."),
        (name = "media", description = "The stored original, the figure crops and the entities extracted for a document."),
        (name = "events", description = "The server-sent ingest progress stream."),
        (name = "health", description = "Liveness, readiness and the accepted upload formats. No key needed."),
    ),
    security(
        ("bearer_auth" = []),
        ("api_key_header" = []),
    ),
    modifiers(&SecurityAddon, &VersionAddon),
)]
pub struct ApiDoc;

/// Stamps the crate version on the document.
///
/// Not in the `#[openapi(info(version = ...))]` attribute: that value is
/// expanded by the derive macro, and Cargo does not rebuild a macro's output
/// when only the package version changes, so a CI build cache kept answering
/// `0.3.0` after the bump to `0.4.0`. `env!` in ordinary code is a tracked
/// dependency of this crate, so it follows every bump.
pub struct VersionAddon;

impl Modify for VersionAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.info.version = env!("CARGO_PKG_VERSION").to_string();
    }
}

/// Adds the two security schemes the API accepts.
///
/// The document declares them globally; the routes that need no key override
/// that with an empty `security`, which is why `/health` and `/docs` are open.
pub struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            BEARER_SCHEME,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "The API key, as `Authorization: Bearer <key>`. The scheme is matched \
                         case-insensitively.",
                    ))
                    .build(),
            ),
        );
        // `ApiKeyValue` is `#[non_exhaustive]`, so the description is set on
        // the value instead of through a builder method.
        let mut api_key = ApiKeyValue::new("X-API-Key");
        api_key.description = Some("The API key, as `X-API-Key: <key>`.".to_string());
        components.add_security_scheme(
            API_KEY_SCHEME,
            SecurityScheme::ApiKey(ApiKey::Header(api_key)),
        );
    }
}

/// The document as pretty JSON.
///
/// Pretty, because the file is committed to the repository and read by people
/// as well as by tools: `docs/openapi.json` is this string, byte for byte.
///
/// The document is fixed once the binary is built, so it is built once and
/// then handed out; a request for `/openapi.json` does not rebuild it.
///
/// # Errors
///
/// Fails when the document cannot be serialised, which cannot happen for a
/// document built from Rust values — but the CLI has to answer with something
/// anyway.
pub fn spec_json() -> Result<String, serde_json::Error> {
    static DOCUMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(document) = DOCUMENT.get() {
        return Ok(document.clone());
    }
    let document = serde_json::to_string_pretty(&ApiDoc::openapi())?;
    // A second caller may have won the race; its document is the same one.
    let _ = DOCUMENT.set(document.clone());
    Ok(document)
}

/// The routes that serve the document and the UI.
///
/// Public by design: `GET /openapi.json` answers the document, `GET /docs`
/// redirects to `GET /docs/`, which renders it.
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/openapi.json",
            axum::routing::get(|| async {
                match spec_json() {
                    Ok(body) => (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "application/json; charset=utf-8",
                        )],
                        body,
                    )
                        .into_response(),
                    Err(err) => crate::error::ApiError::Internal(format!(
                        "could not serialise the OpenAPI document: {err}"
                    ))
                    .into_response(),
                }
            }),
        )
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/docs")
                .config(utoipa_swagger_ui::Config::new(["/openapi.json"])),
        )
}

/// The route table the document is checked against.
///
/// Re-exported so the test reads the same list the router is built from.
pub use crate::routes::ALL_ROUTES;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use utoipa::openapi::path::HttpMethod;

    #[test]
    fn the_document_names_itself_and_its_version() {
        let document = ApiDoc::openapi();
        assert_eq!(document.info.title, "rantai-kb API");
        assert_eq!(document.info.version, env!("CARGO_PKG_VERSION"));
        let version = serde_json::to_value(&document.openapi).expect("version");
        assert_eq!(version, serde_json::json!("3.1.0"));
    }

    #[test]
    fn the_document_declares_both_key_schemes() {
        let document = ApiDoc::openapi();
        let schemes = document
            .components
            .expect("components")
            .security_schemes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert!(schemes.contains(&BEARER_SCHEME.to_string()), "{schemes:?}");
        assert!(schemes.contains(&API_KEY_SCHEME.to_string()), "{schemes:?}");
        // A key is sent one way or the other, never both: the document must
        // offer the choice, which is two requirement objects and not one
        // object with two schemes (that would mean "both, please").
        let requirements = document.security.expect("global security");
        assert_eq!(requirements.len(), 2, "{}", serde_json::json!(requirements));
        let names = requirements
            .iter()
            .map(|requirement| serde_json::to_value(requirement).expect("requirement"))
            .collect::<Vec<_>>();
        assert!(
            names.contains(&serde_json::json!({ BEARER_SCHEME: [] })),
            "{names:?}"
        );
        assert!(
            names.contains(&serde_json::json!({ API_KEY_SCHEME: [] })),
            "{names:?}"
        );
    }

    #[test]
    fn the_public_routes_do_not_ask_for_a_key() {
        let document = ApiDoc::openapi();
        for path in ["/health", "/v1/health", "/v1/formats"] {
            let operation = document
                .paths
                .get_path_operation(path, HttpMethod::Get)
                .expect("operation");
            // `SecurityRequirement` has no `Debug`, so the comparison goes
            // through the JSON the caller will actually read. One empty
            // requirement object is how OpenAPI says "this operation asks
            // for no key", and it is what cancels the global requirement.
            let security = serde_json::to_value(&operation.security).expect("security");
            assert_eq!(
                security,
                serde_json::json!([{}]),
                "{path} must ask for no key"
            );
        }
    }

    #[test]
    fn every_route_of_the_table_is_in_the_document() {
        let document = ApiDoc::openapi();
        for route in ALL_ROUTES {
            let path = openapi_path(route.path);
            let operation = document
                .paths
                .get_path_operation(&path, http_method(route.method.clone()))
                .expect("operation in the document");
            let tags = operation.tags.as_ref().expect("tags");
            assert!(
                tags.contains(&route.tag.to_string()),
                "{path} is filed under {tags:?}, not under {}",
                route.tag
            );
        }
    }

    /// The OpenAPI spelling of an axum method.
    fn http_method(method: axum::http::Method) -> HttpMethod {
        match method {
            axum::http::Method::GET => HttpMethod::Get,
            axum::http::Method::POST => HttpMethod::Post,
            axum::http::Method::PATCH => HttpMethod::Patch,
            axum::http::Method::DELETE => HttpMethod::Delete,
            other => panic!("unexpected method {other}"),
        }
    }

    /// The OpenAPI spelling of an axum path: `:id` becomes `{id}`.
    fn openapi_path(path: &str) -> String {
        let mut out = String::with_capacity(path.len());
        let mut rest = path;
        while let Some(index) = rest.find(':') {
            out.push_str(&rest[..index]);
            out.push('{');
            rest = &rest[index + 1..];
            let end = rest.find('/').unwrap_or(rest.len());
            out.push_str(&rest[..end]);
            out.push('}');
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }
}
