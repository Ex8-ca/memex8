// Transport module
//
// The MCP stdio transport is implemented directly in server.rs
// since it needs access to the Engine and tool definitions.
//
// Future transports:
// - SSE (Server-Sent Events) via Axum
// - HTTP Streamable
