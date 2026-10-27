//! HTTP layer: `POST /trace_attributes/v1` (Stadia Maps path), `POST /trace_attributes`
//! (Valhalla path), `GET /health`.

use crate::trace::{ApiError, Service, TraceRequest};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.body())).into_response()
    }
}

pub fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route("/trace_attributes/v1", post(trace_attributes))
        .route("/trace_attributes", post(trace_attributes))
        .route("/health", get(health))
        .layer(RequestBodyLimitLayer::new(16 * 1024 * 1024))
        .layer(RequestDecompressionLayer::new())
        .layer(CompressionLayer::new())
        .with_state(service)
}

/// Liveness, and the dates of the data the answers come from.
async fn health(State(service): State<Arc<Service>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "sources": service.sources(),
    }))
}

/// The `Authorization: Stadia-Auth <key>` header (or an `api_key` parameter) is accepted
/// and ignored. The body is parsed by hand so that a malformed one gets a Valhalla error.
async fn trace_attributes(
    State(service): State<Arc<Service>>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req: TraceRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return ApiError::bad_request(100, format!("Failed to parse json request: {e}"))
                .into_response()
        }
    };
    match service.trace_attributes(&req).await {
        Ok(out) => {
            let s = &out.stats;
            tracing::info!(
                points = s.points,
                tiles = s.tiles,
                segments = s.segments,
                edges = out.response.edges.len(),
                tiles_ms = s.tiles_ms,
                match_ms = s.match_ms,
                total_ms = s.total_ms,
                "trace_attributes"
            );
            Json(out.response).into_response()
        }
        Err(e) => {
            if e.status >= 500 {
                tracing::warn!(status = e.status, code = e.code, error = %e.message, "trace_attributes failed");
            } else {
                tracing::info!(status = e.status, code = e.code, error = %e.message, "trace_attributes refused");
            }
            e.into_response()
        }
    }
}
