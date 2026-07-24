//! # kedge-server
//!
//! A lightweight embedded HTTP control plane (`kedge serve`) so external
//! dashboards, web UIs, and the WASM terminal can inspect runs and — crucially —
//! **resolve pending human-in-the-loop approvals** remotely, instead of a terminal
//! `y/N`. It's the callback target for [`kedge_hitl::WebhookApprover`].
//!
//! Endpoints:
//! - `GET  /health`                — liveness
//! - `GET  /runs`                  — every recorded run (from the ledger)
//! - `GET  /runs/{id}`             — a run's trajectory + events
//! - `GET  /approvals`             — approvals awaiting a human
//! - `POST /approvals/{id}`        — `{"approved": true|false}` resolves one

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kedge_core::TaskId;
use kedge_hitl::{PendingApproval, PendingApprovals};
use kedge_ledger::Ledger;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    ledger: Ledger,
    approvals: Arc<PendingApprovals>,
    /// When set, every endpoint except `/health` requires
    /// `Authorization: Bearer <token>`. Without it, anyone who can reach the port
    /// could read full trajectories (prompts/args/outputs) or — worse — resolve a
    /// pending human-in-the-loop approval.
    token: Option<Arc<String>>,
}

/// Constant-time string comparison so token checking doesn't leak length-prefix
/// timing. (Length itself can differ; that's an acceptable, minor leak.)
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Reject any request without a valid bearer token. Applied only to protected
/// routes; `/health` stays open for liveness probes.
async fn require_auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = &s.token else {
        return next.run(req).await; // auth disabled (loopback-only, no token set)
    };
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(t) if ct_eq(t, expected) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            "unauthorized: set Authorization: Bearer <KEDGE_SERVE_TOKEN>\n",
        )
            .into_response(),
    }
}

/// A minimal API error that renders as an HTTP status + message.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<kedge_ledger::LedgerError> for ApiError {
    fn from(e: kedge_ledger::LedgerError) -> Self {
        ApiError(StatusCode::NOT_FOUND, e.to_string())
    }
}

/// Build the router over a ledger + a shared approvals registry. When `token` is
/// `Some`, all endpoints except `/health` require a matching bearer token.
/// Exposed for tests.
pub fn router(ledger: Ledger, approvals: Arc<PendingApprovals>, token: Option<String>) -> Router {
    let state = AppState {
        ledger,
        approvals,
        token: token.map(Arc::new),
    };
    // Everything that exposes run data or mutates approval state sits behind auth.
    let protected = Router::new()
        .route("/runs", get(list_runs))
        .route("/runs/:id", get(get_run))
        .route("/approvals", get(list_approvals))
        .route("/approvals/:id", post(resolve_approval))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));
    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}

/// Serve the control API on `addr` until the process exits.
///
/// **Fail-safe:** refuses to bind a non-loopback address without a token, so you
/// can't accidentally expose unauthenticated approval-resolution to the network.
pub async fn serve(
    ledger: Ledger,
    approvals: Arc<PendingApprovals>,
    addr: SocketAddr,
    token: Option<String>,
) -> std::io::Result<()> {
    if !addr.ip().is_loopback() && token.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to bind {addr}: a non-loopback address with no auth would expose \
                 unauthenticated approval-resolution and full ledger reads to the network. \
                 Set KEDGE_SERVE_TOKEN, or bind a loopback address (127.0.0.1)."
            ),
        ));
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, authed = token.is_some(), "kedge control API listening");
    axum::serve(listener, router(ledger, approvals, token)).await
}

async fn health() -> &'static str {
    "ok"
}

async fn list_runs(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let runs = s.ledger.list_runs()?;
    Ok(Json(json!(runs)))
}

async fn get_run(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tid = TaskId(
        Uuid::parse_str(&id)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid run id".into()))?,
    );
    let trajectory = s.ledger.replay(tid)?; // NotFound → 404
    let events = s.ledger.events(tid).unwrap_or_default();
    Ok(Json(json!({ "trajectory": trajectory, "events": events })))
}

async fn list_approvals(State(s): State<AppState>) -> Json<Vec<PendingApproval>> {
    Json(s.approvals.list())
}

#[derive(Deserialize)]
struct ApproveBody {
    approved: bool,
}

async fn resolve_approval(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ApproveBody>,
) -> StatusCode {
    if s.approvals.resolve(&id, body.approved) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND // unknown or already-resolved/expired id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spawn() -> (String, Ledger, Arc<PendingApprovals>) {
        spawn_with(None).await
    }

    async fn spawn_with(token: Option<String>) -> (String, Ledger, Arc<PendingApprovals>) {
        let ledger = Ledger::in_memory().unwrap();
        let approvals = PendingApprovals::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(ledger.clone(), approvals.clone(), token);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), ledger, approvals)
    }

    #[tokio::test]
    async fn protected_routes_require_a_token_when_one_is_set() {
        let (base, _ledger, approvals) = spawn_with(Some("s3cret".into())).await;
        let c = reqwest::Client::new();

        // /health stays open.
        assert!(c
            .get(format!("{base}/health"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());

        // No token → 401 on protected routes.
        assert_eq!(
            c.get(format!("{base}/runs")).send().await.unwrap().status(),
            401
        );
        // Wrong token → 401.
        assert_eq!(
            c.get(format!("{base}/runs"))
                .bearer_auth("wrong")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        // Critically: resolving an approval must not be possible unauthenticated.
        let (id, _rx) = approvals.register("delete_file", "high");
        assert_eq!(
            c.post(format!("{base}/approvals/{id}"))
                .json(&json!({ "approved": true }))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );

        // Correct token → allowed.
        assert!(c
            .get(format!("{base}/runs"))
            .bearer_auth("s3cret")
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind_without_a_token() {
        let ledger = Ledger::in_memory().unwrap();
        let approvals = PendingApprovals::new();
        let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let err = serve(ledger, approvals, addr, None).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn health_and_runs() {
        let (base, _ledger, _approvals) = spawn().await;
        let c = reqwest::Client::new();
        assert_eq!(
            c.get(format!("{base}/health"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        let runs: serde_json::Value = c
            .get(format!("{base}/runs"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(runs.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn approvals_can_be_resolved_over_http() {
        let (base, _ledger, approvals) = spawn().await;
        let c = reqwest::Client::new();

        // The agent side registers a pending approval and awaits the decision.
        let (id, rx) = approvals.register("delete_file", "high");

        // It shows up on the API…
        let listed: Vec<serde_json::Value> = c
            .get(format!("{base}/approvals"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);

        // …a human resolves it over HTTP, and the awaiter unblocks.
        let resp = c
            .post(format!("{base}/approvals/{id}"))
            .json(&json!({ "approved": true }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert!(rx.await.unwrap());

        // Unknown id → 404.
        let resp = c
            .post(format!("{base}/approvals/does-not-exist"))
            .json(&json!({ "approved": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);
    }
}
