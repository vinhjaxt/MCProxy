//! HTTP server module for serving the MCP proxy over HTTP
//!
//! This module uses rmcp's built-in Streamable HTTP transport to serve the MCP protocol,
//! providing full compliance with the MCP Streamable HTTP specification:
//! - `POST /mcp` - for client requests (returns JSON or SSE stream)
//! - `GET /mcp` - for opening SSE connection for server-initiated messages
//! - `DELETE /mcp` - for session termination
//! - `Mcp-Session-Id` header on responses (session management)
//!
//! A separate health check endpoint is available at `/health`.

use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    http::Request,
    response::{Json, Response},
    routing::{any_service, get},
    Router,
};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::{LocalSessionManager, SessionConfig},
};
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::{config::HttpServerConfig, proxy::ProxyServer};

/// Create the HTTP router for the MCP proxy.
///
/// Uses rmcp's `StreamableHttpService` for the `/mcp` endpoint, which handles
/// POST (JSON-RPC requests), GET (SSE streams), and DELETE (session termination)
/// in compliance with the MCP Streamable HTTP specification.
pub fn create_router(proxy_server: Arc<ProxyServer>, http_config: &HttpServerConfig) -> Router {
    let cors = build_cors_layer(http_config);

    let session_manager = Arc::new(LocalSessionManager {
        sessions: Default::default(),
        session_config: SessionConfig::default(),
    });

    let mcp_service = StreamableHttpService::new(
        {
            let proxy = (*proxy_server).clone();
            move || Ok(proxy.clone())
        },
        session_manager,
        StreamableHttpServerConfig {
            sse_keep_alive: Some(std::time::Duration::from_secs(15)),
            stateful_mode: false,
        },
    );

    let mcp_service = AxumMcpService(mcp_service);

    Router::new()
        .route("/mcp", any_service(mcp_service))
        .route("/health", get(health_check))
        .layer(cors)
}

fn build_cors_layer(http_config: &HttpServerConfig) -> CorsLayer {
    if !http_config.cors_enabled {
        return CorsLayer::new();
    }

    let mut cors_layer = CorsLayer::new();

    if http_config.cors_origins.contains(&"*".to_string()) {
        cors_layer = cors_layer.allow_origin(Any);
    } else {
        for origin in &http_config.cors_origins {
            if let Ok(header_value) = origin.parse::<axum::http::HeaderValue>() {
                cors_layer = cors_layer.allow_origin(header_value);
            }
        }
    }

    cors_layer.allow_methods([
        axum::http::Method::GET,
        axum::http::Method::POST,
        axum::http::Method::DELETE,
        axum::http::Method::OPTIONS,
    ])
    .allow_headers([
        axum::http::header::CONTENT_TYPE,
        axum::http::header::AUTHORIZATION,
        axum::http::header::ACCEPT,
    ])
}

/// Parse bind address from HTTP config
pub fn parse_bind_address(http_config: &HttpServerConfig) -> Result<SocketAddr, String> {
    let host_ip = if http_config.host == "localhost" {
        "127.0.0.1"
    } else {
        &http_config.host
    };

    let bind_address_str = format!("{}:{}", host_ip, http_config.port);
    info!(
        "Parsed HTTP config - host: '{}', port: {}, bind_address: '{}'",
        http_config.host, http_config.port, bind_address_str
    );

    bind_address_str
        .parse()
        .map_err(|e| format!("Invalid bind address '{}': {}", bind_address_str, e))
}

/// Wrapper that adapts rmcp's `StreamableHttpService` (tower `Service`) for axum.
///
/// The rmcp service returns `http::Response<BoxBody<Bytes, Infallible>>` which
/// we convert to axum's `Response` by mapping the body.
#[derive(Clone)]
struct AxumMcpService<S>(StreamableHttpService<S>);

impl<S> tower::Service<Request<Body>> for AxumMcpService<S>
where
    S: rmcp::Service<rmcp::RoleServer> + Send + Sync + 'static + Clone,
{
    type Response = Response;
    type Error = Infallible;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let service = self.0.clone();
        Box::pin(async move {
            let response = service.handle(req).await;
            let (parts, body) = response.into_parts();
            // BoxBody<Bytes, Infallible> -> Body: Infallible never happens, so this is safe
            let body = Body::new(body);
            Ok(Response::from_parts(parts, body))
        })
    }
}

async fn health_check() -> Json<Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "mcproxy"
    }))
}
