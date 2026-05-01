# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

- **Build**: `cargo build --release`
- **Run**: `./target/release/mcproxy <config_file>`
- **Run tests**: `cargo test`
- **Run a single test**: `cargo test test_name` (e.g., `cargo test test_health_endpoint`)
- **Run specific module tests**: `cargo test --lib config::tests`

## Architecture

MCProxy is a Rust MCP (Model-Context-Protocol) proxy server that aggregates tools/prompts/resources from multiple upstream MCP servers and exposes them through a single HTTP endpoint.

### Core Flow

```
MCP Clients → HTTP/JSON-RPC (/mcp) → ProxyServer → N upstream MCP servers (stdio or HTTP)
```

All tool/prompt/resource names from upstream servers are prefixed with `server_name___item_name` to avoid collisions.

### Key Modules

- **`main.rs`** — Entry point, config loading, env var overrides (`MCPROXY_HTTP_HOST`, `MCPROXY_HTTP_PORT`, etc.), graceful shutdown via ctrl-c.
- **`config.rs`** — JSON config parsing. `McpConfig` holds `mcpServers` (stdio or HTTP) and `httpServer` settings including middleware config.
- **`proxy.rs`** — `ProxyServer`: the core aggregator. Connects to all configured servers in parallel, caches tools/prompts/resources per server, implements `ServerHandler` for the rmcp framework.
- **`http_server.rs`** — Axum-based HTTP server. JSON-RPC 2.0 endpoint at `/mcp`, health check at `/health`. SSE streaming for the `search_available_tools` tool.
- **`error.rs`** — `ProxyError` enum with conversion to `McpError` for protocol compatibility.

### Middleware System

Two-tier middleware architecture configured via `httpServer.middleware`:

- **ProxyMiddleware** (`middleware/proxy.rs`) — operates on aggregated results from all servers after collection. Built-in: `description_enricher`, `tool_search`.
- **ClientMiddleware** (`middleware/client.rs`) — wraps individual downstream server calls with before/after hooks. Built-in: `logging`, `tool_filter`, `security`.
- **MiddlewareRegistry** (`middleware/registry.rs`) — factory pattern for creating middleware from config. New middleware must register a factory here.
- Client middleware supports per-server overrides in config (`middleware.client.servers.<name>`).

### Key Dependencies

- `rmcp` — MCP protocol implementation (client, server, transports)
- `axum` + `tower-http` — HTTP server with CORS
- `tantivy` — full-text search index for tool search
- `reqwest` — HTTP client for upstream MCP servers with optional Bearer auth

### Environment Variables

- `RUST_LOG` — log level (default: `mcproxy=info,rmcp=warn`)
- `RUST_LOG_FORMAT=json` — switch from human-readable to JSON log output
- `MCPROXY_HTTP_HOST`, `MCPROXY_HTTP_PORT`, `MCPROXY_CORS_ENABLED`, `MCPROXY_CORS_ORIGINS`, `MCPROXY_SHUTDOWN_TIMEOUT` — override config file values
