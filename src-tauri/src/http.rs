//! Embedded Axum HTTP server.
//!
//! Hosts `/healthz` and the MCP streamable-HTTP service at `/mcp`.
//! This endpoint exists so:
//!   * the front-end can verify the backend booted
//!   * agents (Claude Code, Cursor) can reach Atlas tools without
//!     speaking Tauri IPC
//!   * `atlas-serve` (future) reuses this same router behind Axum's
//!     standard bindings - the only difference is whether we bind to
//!     loopback or public.
//!
//! The server binds to 127.0.0.1 on an ephemeral port and stays
//! loopback-only for the desktop build.

use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use crate::mcp::{self, McpState};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Build the Atlas HTTP router. Kept in its own function so tests and
/// the hosted variant can mount it without depending on the Tauri
/// runtime. `state` carries the Arcs the MCP tool handlers need -
/// same instances the Tauri commands use, so a search done over IPC
/// and a search done over MCP hit the exact same cached index readers.
pub fn router(state: McpState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest_service("/mcp", mcp::build_mcp_service(state))
}

/// Spawn the Axum server on a background Tokio task. Returns the
/// bound port so the Tauri side can log it / expose it to the
/// front-end later.
pub async fn serve(state: McpState) -> anyhow::Result<u16> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?.port();
    tracing::info!("atlas http listening on http://127.0.0.1:{bound} (MCP at /mcp)");

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router(state)).await {
            tracing::error!("atlas http server error: {err}");
        }
    });

    Ok(bound)
}
