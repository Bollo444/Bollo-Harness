//! Test fixture binary: the fake MCP stdio server used by the cross-crate
//! integration suites. It is not part of the product surface.

fn main() {
    bollo_extensions::mcp_fake::serve_stdio();
}
