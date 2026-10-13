//! Management API wire types, SSE event types and `openapi.json`.
//!
//! The resources are specified in `docs/spec/management-api.md`; this crate owns their types,
//! the error envelope, the route constants with their Deadlines, and the committed
//! `openapi.json`. No tokio, axum or IO (architecture §4).

pub mod deadline;
pub mod error;
pub mod permission;
pub mod revision;
pub mod routes;
pub mod scope;
pub mod scopes;
pub mod sessions;
pub mod settings;
mod wire;

pub use deadline::{Call, Deadline};
pub use error::{ApiError, ErrorBody, ErrorCode};
pub use permission::TokenPermission;
pub use revision::Revision;
pub use routes::Route;
pub use scope::Scope;
pub use scopes::ScopeLauncher;
pub use sessions::{ScopeSession, SessionStatus};
pub use settings::SecretState;

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "ClaudeClaw Management API",
        version = "1",
        description = "The Daemon's HTTP/JSON + SSE API, served on the local socket or pipe and, \
                       when enabled, on the pinned-TLS remote listener.",
        license(name = "MIT", identifier = "MIT"),
    ),
    components(schemas(ApiError, ErrorBody, ErrorCode, TokenPermission, Scope, Revision))
)]
struct ApiDoc;

/// The document `crates/api/openapi.json` holds: the API's info and every wire type's schema.
/// The server adds its routes' paths to it (architecture §4, `claudeclaw-server`).
///
/// Each resource module's `Doc` is merged here with `doc.merge(module::Doc::openapi())`, one line
/// per module, in management-api §2 order.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    doc.merge(scopes::Doc::openapi());
    doc.merge(sessions::Doc::openapi());
    doc.merge(settings::Doc::openapi());
    doc
}
