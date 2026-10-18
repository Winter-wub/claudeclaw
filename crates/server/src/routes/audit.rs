//! The audit-log handler (management-api §2.9): `audit.list`.

use axum::Json;
use axum::extract::{Query, State};

use claudeclaw_api::logs::AuditPage;

use crate::backend::audit::AuditQuery;
use crate::error::ApiFailure;
use crate::routes::Params;
use crate::state::AppState;

/// `GET /v1/audit`: one page of entries, newest first (§2.9). `target` is an identifier, never a
/// value, and the page holds no secret, prompt or value (#13, #44).
pub async fn list(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<AuditPage>, ApiFailure> {
    let params = Params::new(&pairs);
    let query = AuditQuery {
        before: params.get("before").map(str::to_owned),
        limit: params.page_limit()?,
        principal: params.get("principal").map(str::to_owned),
        action: params.get("action").map(str::to_owned),
    };
    state.audit.list(query).await.map(Json).map_err(Into::into)
}
