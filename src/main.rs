//! MCP (Model Context Protocol) Proxy Server
//! 
//! This application acts as a proxy that aggregates multiple MCP servers and exposes
//! them through a single HTTP endpoint. It supports both stdio (subprocess) and HTTP
//! MCP servers as backends.
//! 
//! # Architecture
//! 
//! The proxy consists of several key components:
//! 
//! - **Main Module**: Entry point and application lifecycle management
//! - **Config Module**: Configuration management with environment variable support
//! - **Error Module**: Centralized error handling with `ProxyError` type
//! - **HTTP Server Module**: HTTP/JSON-RPC server implementation
//! - **Proxy Module**: Core proxy logic for aggregating MCP servers
//! 
//! # Usage
//! 
//! ```bash
//! mcproxy <config_file>
//! ```
//! 
//! The configuration file should be in JSON format specifying the MCP servers to
//! connect to and HTTP server settings.

use std::{env, sync::Arc};
use std::os::unix::fs::PermissionsExt;

use config::load_config;
use proxy::ProxyServer;
use tracing::{error, info, warn};

mod config;
mod error;
mod http_server;
mod proxy;
mod middleware;

/// Parse an octal permission mode string like "0777" or "0o660"
fn parse_octal_mode(s: &str) -> Result<u32, String> {
    let s = s.trim();
    let digits = if s.starts_with("0o") || s.starts_with("0O") {
        &s[2..]
    } else {
        s
    };
    u32::from_str_radix(digits, 8)
        .map_err(|e| format!("Invalid unix socket mode '{}': {}", s, e))
}

/// Apply environment variable overrides to the configuration
fn apply_env_overrides(config: &mut config::McpConfig) {
    // Ensure HTTP server config exists
    if config.http_server.is_none() {
        config.http_server = Some(config::HttpServerConfig::default());
    }
    
    if let Some(http_config) = config.http_server.as_mut() {
        // Override host if MCPROXY_HTTP_HOST is set
        if let Ok(host) = env::var("MCPROXY_HTTP_HOST") {
            info!("Overriding HTTP host from environment: {}", host);
            http_config.host = host;
        }
        
        // Override port if MCPROXY_HTTP_PORT is set
        if let Ok(port_str) = env::var("MCPROXY_HTTP_PORT") {
            if let Ok(port) = port_str.parse::<u16>() {
                info!("Overriding HTTP port from environment: {}", port);
                http_config.port = port;
            } else {
                warn!("Invalid MCPROXY_HTTP_PORT value: {}", port_str);
            }
        }
        
        // Override CORS enabled if MCPROXY_CORS_ENABLED is set
        if let Ok(cors_str) = env::var("MCPROXY_CORS_ENABLED") {
            match cors_str.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => {
                    info!("Enabling CORS from environment");
                    http_config.cors_enabled = true;
                }
                "false" | "0" | "no" | "off" => {
                    info!("Disabling CORS from environment");
                    http_config.cors_enabled = false;
                }
                _ => warn!("Invalid MCPROXY_CORS_ENABLED value: {}", cors_str),
            }
        }
        
        // Override CORS origins if MCPROXY_CORS_ORIGINS is set
        if let Ok(origins_str) = env::var("MCPROXY_CORS_ORIGINS") {
            let origins: Vec<String> = origins_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !origins.is_empty() {
                info!("Overriding CORS origins from environment: {:?}", origins);
                http_config.cors_origins = origins;
            }
        }
        
        // Override shutdown timeout if MCPROXY_SHUTDOWN_TIMEOUT is set
        if let Ok(timeout_str) = env::var("MCPROXY_SHUTDOWN_TIMEOUT") {
            if let Ok(timeout) = timeout_str.parse::<u64>() {
                info!("Overriding shutdown timeout from environment: {} seconds", timeout);
                http_config.shutdown_timeout = timeout;
            } else {
                warn!("Invalid MCPROXY_SHUTDOWN_TIMEOUT value: {}", timeout_str);
            }
        }

        // Override unix socket path if MCPROXY_UNIX_SOCKET is set
        if let Ok(socket_path) = env::var("MCPROXY_UNIX_SOCKET") {
            info!("Overriding unix socket path from environment: {}", socket_path);
            http_config.unix_socket = Some(socket_path);
        }

        // Override unix socket mode if MCPROXY_UNIX_SOCKET_MODE is set
        if let Ok(mode_str) = env::var("MCPROXY_UNIX_SOCKET_MODE") {
            info!("Overriding unix socket mode from environment: {}", mode_str);
            http_config.unix_socket_mode = Some(mode_str);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Configure logging with reduced verbosity
    let log_level = env::var("RUST_LOG").unwrap_or_else(|_| "mcproxy=info,rmcp=warn".to_string());
    
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(&log_level)
        .with_target(false);
    
    // Use JSON format in production, human-readable format in development
    if env::var("RUST_LOG_FORMAT").unwrap_or_default() == "json" {
        subscriber.json().init();
    } else {
        subscriber.init();
    }

    info!("Starting MCP Proxy HTTP Server");

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <config_file>", args[0]);
        std::process::exit(1);
    }

    let config_path = &args[1];
    let mut config = load_config(config_path)?;
    
    // Apply environment variable overrides
    apply_env_overrides(&mut config);

    // Create the proxy server and connect to all MCP servers
    info!("Connecting to {} MCP servers...", config.mcp_servers.len());
    let proxy_server = ProxyServer::new(config.clone()).await?;
    let shared_proxy = Arc::new(proxy_server);

    // Get HTTP server configuration with defaults
    let http_config = config
        .http_server
        .as_ref()
        .ok_or("HTTP server configuration is required")?;

    // Create HTTP server using the http_server module
    let app = http_server::create_router(shared_proxy.clone(), http_config);

    info!("   🔗 Connected to {} MCP servers", config.mcp_servers.len());

    // List connected servers with their tools
    let servers = shared_proxy.servers.read().await;
    for (name, server) in servers.iter() {
        info!("   └─ 🔌 {}", name);

        // Display tools for each server
        if !server.tools.is_empty() {
            for tool in &server.tools {
                info!("      └─ 🔧 {}", tool.name);
            }
        } else {
            info!("      └─ (no tools available)");
        }
    }
    drop(servers);

    // Set up graceful shutdown signal handler
    let shared_proxy_shutdown = shared_proxy.clone();
    let shutdown_signal = async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
        info!("🛑 Received shutdown signal, gracefully shutting down...");

        // Shutdown the proxy server and all connected MCP servers
        shared_proxy_shutdown.shutdown().await;
    };

    if let Some(ref socket_path) = http_config.unix_socket {
        // Unix socket mode
        // Remove stale socket file
        let _ = std::fs::remove_file(socket_path);

        let listener = tokio::net::UnixListener::bind(socket_path)?;

        // Apply chmod if configured
        if let Some(ref mode_str) = http_config.unix_socket_mode {
            let mode = parse_octal_mode(mode_str)
                .map_err(|e| format!("{}", e))?;
            std::fs::set_permissions(socket_path, PermissionsExt::from_mode(mode))?;
            info!("🚀 MCP Proxy listening on unix://{} (mode {:o})", socket_path, mode);
        } else {
            info!("🚀 MCP Proxy listening on unix://{}", socket_path);
        }
        info!("   📡 MCP endpoint: unix://{}/mcp", socket_path);
        info!("   🔍 Health check: unix://{}/health", socket_path);

        let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal);

        if let Err(e) = server.await {
            error!("HTTP server error: {}", e);
        } else {
            info!("✅ Server shutdown complete");
        }
    } else {
        // TCP mode
        let bind_addr = http_server::parse_bind_address(http_config)?;
        info!("Binding HTTP server to {}", bind_addr);

        let listener = tokio::net::TcpListener::bind(bind_addr).await?;

        info!("🚀 MCP Proxy HTTP Server listening on http://{}", bind_addr);
        info!("   📡 MCP endpoint: http://{}/mcp", bind_addr);
        info!("   🔍 Health check: http://{}/health", bind_addr);

        let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal);

        if let Err(e) = server.await {
            error!("HTTP server error: {}", e);
        } else {
            info!("✅ Server shutdown complete");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpConfig;
    use axum::{
        body::Body,
        http::{header, Method, Request, StatusCode},
        Router,
    };
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use tower::ServiceExt;
    
    /// Test fixture builder for creating test configurations and apps
    struct TestFixture {
        config: McpConfig,
    }
    
    impl TestFixture {
        /// Create a new test fixture builder
        fn builder() -> TestFixtureBuilder {
            TestFixtureBuilder::default()
        }
        
        /// Build a test app from this fixture
        async fn build_app(self) -> Router {
            let proxy = ProxyServer::new(self.config).await.unwrap();
            create_test_app(proxy).await
        }
    }
    
    #[derive(Default)]
    struct TestFixtureBuilder {
        mcp_servers: HashMap<String, config::ServerConfig>,
        host: Option<String>,
        port: Option<u16>,
        cors_enabled: Option<bool>,
        cors_origins: Option<Vec<String>>,
        shutdown_timeout: Option<u64>,
    }
    
    impl TestFixtureBuilder {
        #[allow(dead_code)]
        fn with_server(mut self, name: &str, config: config::ServerConfig) -> Self {
            self.mcp_servers.insert(name.to_string(), config);
            self
        }
        
        #[allow(dead_code)]
        fn with_host(mut self, host: &str) -> Self {
            self.host = Some(host.to_string());
            self
        }
        
        #[allow(dead_code)]
        fn with_port(mut self, port: u16) -> Self {
            self.port = Some(port);
            self
        }
        
        #[allow(dead_code)]
        fn with_cors(mut self, enabled: bool, origins: Vec<&str>) -> Self {
            self.cors_enabled = Some(enabled);
            self.cors_origins = Some(origins.into_iter().map(|s| s.to_string()).collect());
            self
        }
        
        #[allow(dead_code)]
        fn with_shutdown_timeout(mut self, timeout: u64) -> Self {
            self.shutdown_timeout = Some(timeout);
            self
        }
        
        fn build(self) -> TestFixture {
            let config = McpConfig {
                mcp_servers: self.mcp_servers,
                http_server: Some(config::HttpServerConfig {
                    host: self.host.unwrap_or_else(|| "127.0.0.1".to_string()),
                    port: self.port.unwrap_or(0),
                    cors_enabled: self.cors_enabled.unwrap_or(true),
                    cors_origins: self.cors_origins.unwrap_or_else(|| vec!["*".to_string()]),
                    shutdown_timeout: self.shutdown_timeout.unwrap_or(5),
                    unix_socket: None,
                    unix_socket_mode: None,
                    middleware: config::MiddlewareConfig::default(),
                }),
            };
            TestFixture { config }
        }
    }

    /// Create a test configuration
    fn create_test_config() -> McpConfig {
        TestFixture::builder().build().config
    }

    /// Helper to create a test app with proxy server
    async fn create_test_app(proxy: ProxyServer) -> Router {
        let shared_proxy = Arc::new(proxy);
        let test_config = config::HttpServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            cors_enabled: true,
            cors_origins: vec!["*".to_string()],
            shutdown_timeout: 5,
            unix_socket: None,
            unix_socket_mode: None,
            middleware: config::MiddlewareConfig::default(),
        };
        http_server::create_router(shared_proxy, &test_config)
    }

    /// Helper to create a test app with default config
    async fn create_default_test_app() -> Router {
        TestFixture::builder().build().build_app().await
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let config = create_test_config();
        let proxy = ProxyServer::new(config).await.unwrap();
        let app = create_test_app(proxy).await;

        let request = Request::builder()
            .method(Method::GET)
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["service"], "mcproxy");
        assert_eq!(body["status"], "healthy");
    }

    #[tokio::test]
    async fn test_cors_headers() {
        let app = create_default_test_app().await;

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/mcp")
            .header("Origin", "https://example.com")
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "Content-Type")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert!(headers.contains_key("access-control-allow-origin"));
        assert!(headers.contains_key("access-control-allow-methods"));
        assert!(headers.contains_key("access-control-allow-headers"));
    }

    #[tokio::test]
    async fn test_jsonrpc_parse_error() {
        let app = create_default_test_app().await;

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("invalid json"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Axum returns 400 (Bad Request) for malformed JSON, which is acceptable
        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn test_jsonrpc_invalid_version() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "1.0",
            "id": 1,
            "method": "ping"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert!(body["error"].is_object());
        assert_eq!(body["error"]["code"], -32600); // Invalid Request
    }

    #[tokio::test]
    async fn test_ping_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());
    }

    #[tokio::test]
    async fn test_initialize_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "roots": {
                        "listChanged": true
                    },
                    "sampling": {}
                },
                "clientInfo": {
                    "name": "TestClient",
                    "version": "1.0.0"
                }
            }
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());

        // Verify the result contains server info
        let result = &body["result"];
        assert!(result.get("protocolVersion").is_some());
        assert!(result.get("capabilities").is_some());
        assert!(result.get("serverInfo").is_some());
    }

    #[tokio::test]
    async fn test_tools_list_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());

        // Verify the result has the expected structure
        let result = &body["result"];
        assert!(result.get("tools").is_some());
        // Note: next_cursor is None when there are no more results, so it's omitted from JSON
    }

    #[tokio::test]
    async fn test_tools_call_missing_params() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body.get("result").is_none());
        assert!(body["error"].is_object());
        assert_eq!(body["error"]["code"], -32602); // Invalid params
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "unknown/method"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body.get("result").is_none());
        assert!(body["error"].is_object());
        assert_eq!(body["error"]["code"], -32602); // Invalid params (our current implementation)
    }

    #[tokio::test]
    async fn test_prompts_list_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "prompts/list"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());

        // Verify the result has the expected structure
        let result = &body["result"];
        assert!(result.get("prompts").is_some());
        // Note: next_cursor is None when there are no more results, so it's omitted from JSON
    }

    #[tokio::test]
    async fn test_resources_list_method() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/list"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());

        // Verify the result has the expected structure
        let result = &body["result"];
        assert!(result.get("resources").is_some());
        // Note: next_cursor is None when there are no more results, so it's omitted from JSON
    }

    #[tokio::test]
    async fn test_tools_call_empty_name() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "",
                "arguments": {}
            }
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body.get("result").is_none());
        assert!(body["error"].is_object());
        // Should get invalid format error
    }

    #[tokio::test]
    async fn test_tools_call_invalid_format() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invalid_tool_name_without_prefix",
                "arguments": {}
            }
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body.get("result").is_none());
        assert!(body["error"].is_object());
        // Should get invalid format error
    }

    #[tokio::test]
    async fn test_tools_call_server_not_found() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "nonexistent_server___some_tool",
                "arguments": {}
            }
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body.get("result").is_none());
        assert!(body["error"].is_object());
        // Should get server not found error
    }

    #[tokio::test]
    async fn test_empty_config_no_servers() {
        // Test that proxy works even with no servers configured
        let fixture = TestFixture::builder().build();
        let app = fixture.build_app().await;

        // Test tools/list returns empty array
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(body["result"].is_object());
        assert!(body["result"]["tools"].is_array());
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_request_without_id() {
        let app = create_default_test_app().await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "method": "ping"
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&request_body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        
        assert_eq!(body["jsonrpc"], "2.0");
        assert!(body.get("id").is_none() || body["id"].is_null());
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());
    }

    #[test]
    fn test_parse_octal_mode() {
        assert_eq!(parse_octal_mode("0777").unwrap(), 0o777);
        assert_eq!(parse_octal_mode("0o777").unwrap(), 0o777);
        assert_eq!(parse_octal_mode("777").unwrap(), 0o777);
        assert_eq!(parse_octal_mode("0660").unwrap(), 0o660);
        assert_eq!(parse_octal_mode("0o660").unwrap(), 0o660);
        assert_eq!(parse_octal_mode("  0777  ").unwrap(), 0o777);
        assert!(parse_octal_mode("0999").is_err());
        assert!(parse_octal_mode("abc").is_err());
    }

    // ── Unix socket integration tests ──────────────────────────────────

    /// Send a raw HTTP/1.1 request over a Unix socket and return the response body.
    /// Handles both Content-Length and chunked transfer encoding responses.
    async fn http_get_over_unix(socket_path: &str, uri: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(socket_path).await
            .expect("Failed to connect to Unix socket");
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            uri
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        // Don't shutdown write side — we need to read the response
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8(buf).unwrap();
        extract_http_body(&response)
    }

    /// Send a raw HTTP/1.1 POST request over a Unix socket and return the response body.
    async fn http_post_over_unix(socket_path: &str, uri: &str, body: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(socket_path).await
            .expect("Failed to connect to Unix socket");
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            uri, body.len(), body
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        // Don't shutdown write side — we need to read the response
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8(buf).unwrap();
        extract_http_body(&response)
    }

    /// Extract the body from a raw HTTP response, handling chunked transfer encoding.
    fn extract_http_body(response: &str) -> String {
        let (headers, body) = response.split_once("\r\n\r\n").unwrap_or(("", response));

        if headers.contains("Transfer-Encoding: chunked") {
            // Decode chunked encoding
            let mut decoded = String::new();
            let mut remaining = body;
            loop {
                let (size_str, rest) = remaining.split_once("\r\n").unwrap_or(("", ""));
                let chunk_size = usize::from_str_radix(size_str.trim(), 16).unwrap_or(0);
                if chunk_size == 0 {
                    break;
                }
                let chunk_data = &rest[..chunk_size.min(rest.len())];
                decoded.push_str(chunk_data);
                remaining = &rest[chunk_size.min(rest.len())..];
                // Skip trailing \r\n after chunk data
                if remaining.starts_with("\r\n") {
                    remaining = &remaining[2..];
                }
            }
            decoded
        } else {
            body.to_string()
        }
    }

    /// Spawn the server on a Unix socket. Returns the socket path.
    /// The server shuts down when the `shutdown_tx` sender is dropped.
    async fn spawn_unix_server(socket_path: String, mode: Option<String>) -> tokio::task::JoinHandle<()> {
        let config = McpConfig {
            mcp_servers: HashMap::new(),
            http_server: Some(config::HttpServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0, // ignored — we use unix_socket
                cors_enabled: true,
                cors_origins: vec!["*".to_string()],
                shutdown_timeout: 5,
                unix_socket: Some(socket_path.clone()),
                unix_socket_mode: mode.clone(),
                middleware: config::MiddlewareConfig::default(),
            }),
        };

        let proxy = ProxyServer::new(config).await.unwrap();
        let shared_proxy = Arc::new(proxy);

        let http_config = config::HttpServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            cors_enabled: true,
            cors_origins: vec!["*".to_string()],
            shutdown_timeout: 5,
            unix_socket: Some(socket_path.clone()),
            unix_socket_mode: mode,
            middleware: config::MiddlewareConfig::default(),
        };

        let app = http_server::create_router(shared_proxy, &http_config);

        // Remove stale socket
        let _ = std::fs::remove_file(&socket_path);

        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

        // Apply chmod if configured
        if let Some(ref mode_str) = http_config.unix_socket_mode {
            let mode = parse_octal_mode(mode_str).unwrap();
            std::fs::set_permissions(&socket_path, PermissionsExt::from_mode(mode)).unwrap();
        }

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });

        let handle = tokio::spawn(async move {
            let _ = server.await;
            // Cleanup socket on shutdown
            let _ = std::fs::remove_file(&socket_path);
        });

        // Give the server a moment to start accepting connections
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Leak the shutdown_tx so caller can drop it to stop the server
        // We return the handle; the server runs until the JoinHandle is aborted
        std::mem::forget(shutdown_tx);

        handle
    }

    #[tokio::test]
    async fn test_unix_socket_health_check() {
        let socket_path = format!("/tmp/mcproxy_test_health_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        let handle = spawn_unix_server(socket_path.clone(), None).await;

        let body = http_get_over_unix(&socket_path, "/health").await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["status"], "healthy");
        assert_eq!(resp["service"], "mcproxy");

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_ping() {
        let socket_path = format!("/tmp/mcproxy_test_ping_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        let handle = spawn_unix_server(socket_path.clone(), None).await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "ping"
        }).to_string();

        let body = http_post_over_unix(&socket_path, "/mcp", &request_body).await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert!(resp["result"].is_object());

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_tools_list() {
        let socket_path = format!("/tmp/mcproxy_test_tools_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        let handle = spawn_unix_server(socket_path.clone(), None).await;

        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }).to_string();

        let body = http_post_over_unix(&socket_path, "/mcp", &request_body).await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["result"]["tools"].is_array());

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_stale_file_cleanup() {
        let socket_path = format!("/tmp/mcproxy_test_stale_{}.sock", std::process::id());

        // Create a fake stale socket file (just a regular file)
        std::fs::write(&socket_path, "stale").unwrap();
        assert!(std::path::Path::new(&socket_path).exists());

        // Spawning the server should clean it up and bind successfully
        let handle = spawn_unix_server(socket_path.clone(), None).await;

        // Server should be running — health check must work
        let body = http_get_over_unix(&socket_path, "/health").await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["status"], "healthy");

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_chmod_applied() {
        let socket_path = format!("/tmp/mcproxy_test_chmod_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        let handle = spawn_unix_server(socket_path.clone(), Some("0777".to_string())).await;

        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o777);

        // Also verify it actually works
        let body = http_get_over_unix(&socket_path, "/health").await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["status"], "healthy");

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_restricted_mode() {
        let socket_path = format!("/tmp/mcproxy_test_restricted_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        let handle = spawn_unix_server(socket_path.clone(), Some("0600".to_string())).await;

        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_unix_socket_no_mode_default_permissions() {
        let socket_path = format!("/tmp/mcproxy_test_nomode_{}.sock", std::process::id());
        let _ = std::fs::remove_file(&socket_path);

        // No unixSocketMode configured — should use process umask
        let handle = spawn_unix_server(socket_path.clone(), None).await;

        // Socket must exist and be usable
        assert!(std::path::Path::new(&socket_path).exists());
        let body = http_get_over_unix(&socket_path, "/health").await;
        let resp: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(resp["status"], "healthy");

        handle.abort();
        let _ = handle.await;
    }

    #[test]
    fn test_config_parse_unix_socket() {
        let json_data = r#"
        {
          "mcpServers": {},
          "httpServer": {
            "unixSocket": "/var/run/mcproxy.sock",
            "unixSocketMode": "0660"
          }
        }
        "#;

        let config: McpConfig = serde_json::from_str(json_data).unwrap();
        let http = config.http_server.as_ref().unwrap();
        assert_eq!(http.unix_socket.as_deref(), Some("/var/run/mcproxy.sock"));
        assert_eq!(http.unix_socket_mode.as_deref(), Some("0660"));
        // host/port should be defaults
        assert_eq!(http.host, "localhost");
        assert_eq!(http.port, 8080);
    }

    #[test]
    fn test_config_parse_no_unix_socket() {
        let json_data = r#"
        {
          "mcpServers": {},
          "httpServer": {
            "host": "0.0.0.0",
            "port": 9000
          }
        }
        "#;

        let config: McpConfig = serde_json::from_str(json_data).unwrap();
        let http = config.http_server.as_ref().unwrap();
        assert!(http.unix_socket.is_none());
        assert!(http.unix_socket_mode.is_none());
    }

    #[test]
    fn test_config_parse_unix_socket_mode_optional() {
        let json_data = r#"
        {
          "mcpServers": {},
          "httpServer": {
            "unixSocket": "/tmp/mcproxy.sock"
          }
        }
        "#;

        let config: McpConfig = serde_json::from_str(json_data).unwrap();
        let http = config.http_server.as_ref().unwrap();
        assert_eq!(http.unix_socket.as_deref(), Some("/tmp/mcproxy.sock"));
        assert!(http.unix_socket_mode.is_none());
    }
}
