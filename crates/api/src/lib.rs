//! Management API wire types, SSE event types and `openapi.json`.
//!
//! The resources are specified in `docs/spec/management-api.md`; this crate owns their types,
//! the error envelope, the route constants with their Deadlines, and the committed
//! `openapi.json`. No tokio, axum or IO (architecture §4).

pub mod adapter;
pub mod agents;
pub mod deadline;
pub mod discord;
pub mod error;
pub mod events;
pub mod export;
pub mod jobs;
pub mod log_event;
pub mod logs;
pub mod permission;
pub mod revision;
pub mod routes;
pub mod runs;
pub mod scope;
pub mod scopes;
pub mod sessions;
pub mod settings;
pub mod status;
pub mod supervisor;
pub mod turns;
pub mod usage;
mod wire;

pub use adapter::{AdapterState, Transport};
pub use agents::{
    AgentArchived, AgentCreate, AgentInfo, AgentList, AgentPersona, AgentPersonaPut, AgentPurged,
    ArchivedAgent, ArchivedAgentList,
};
pub use deadline::{Call, Deadline};
pub use discord::{
    AvailableChannelList, DiscordChannel, DiscordChannelPatch, DiscordChannelRemoved,
    DiscordOverview, DiscordThread, DiscordUser, FireResult, HireRequest, HireResult,
};
pub use error::{ApiError, ErrorBody, ErrorCode};
pub use events::{
    DaemonEvent, GapReason, JobChange, JobChangeSource, LogStreamHello, NoticeKind, StreamGap,
    StreamHello,
};
pub use jobs::{ImportFinding, JobDetail, JobSummary};
pub use log_event::UiLogEvent;
pub use permission::TokenPermission;
pub use revision::Revision;
pub use routes::Route;
pub use runs::{RunIndexState, RunRecord};
pub use scope::Scope;
pub use scopes::ScopeLauncher;
pub use sessions::{ScopeSession, SessionStatus};
pub use settings::SecretState;
pub use status::{DaemonStatus, Health, Problem, ProblemKind, RunLocale};
pub use supervisor::{DaemonReachability, RunRegistry, SupervisorStatus};
pub use turns::{
    AttachmentUpload, SessionCompacted, TurnAccepted, TurnAgentState, TurnEvent, TurnOutcome,
    TurnPurpose, TurnRecord, TurnRequest, TurnSession, TurnState,
};

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
    doc.merge(adapter::Doc::openapi());
    doc.merge(status::Doc::openapi());
    doc.merge(supervisor::Doc::openapi());
    doc.merge(events::Doc::openapi());
    doc.merge(turns::Doc::openapi());
    doc.merge(jobs::Doc::openapi());
    doc.merge(scopes::Doc::openapi());
    doc.merge(sessions::Doc::openapi());
    doc.merge(settings::Doc::openapi());
    doc.merge(logs::Doc::openapi());
    doc.merge(log_event::Doc::openapi());
    doc.merge(export::Doc::openapi());
    doc.merge(runs::Doc::openapi());
    doc.merge(usage::Doc::openapi());
    doc.merge(discord::Doc::openapi());
    doc.merge(agents::Doc::openapi());
    doc
}
