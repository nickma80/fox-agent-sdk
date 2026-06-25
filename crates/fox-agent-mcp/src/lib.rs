//! Fox Agent MCP — Model Context Protocol client for the Fox Agent SDK.
//!
//! This crate implements a lightweight MCP client that connects to external
//! MCP servers via stdio (subprocess) or SSE (HTTP long‑poll).  It provides
//! automatic tool discovery and execution, integrating seamlessly into the
//! fox‑agent‑sdk tool system.

pub mod client;
pub mod json_rpc;
pub mod tool_adapter;
pub mod transport;
pub mod types;

pub use client::{McpClient, McpClientError, McpServerHandle};
pub use tool_adapter::{McpToolDefinition, mcp_tool_to_definition};
pub use transport::{McpTransport, StdioTransport, StdioTransportConfig, TransportError};
pub use types::*;
