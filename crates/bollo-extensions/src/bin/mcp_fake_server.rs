//! Minimal MCP stdio server used only by integration tests; the implementation
//! lives in `bollo_extensions::mcp_fake` so other crates can ship the same
//! fixture binary.

fn main() {
    bollo_extensions::mcp_fake::serve_stdio();
}
