//! Observations captured independently at the two boundaries.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line of the MCP evidence log, written by the fake MCP server *before*
/// the corresponding bytes are written to its stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpEvidence {
    /// "session-start" or "tool-response".
    pub event: String,
    pub pid: u32,
    /// Per-process sequence number.
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Whether the payload was generated for a planned call (false = argument mismatch).
    #[serde(default)]
    pub generated: bool,
    /// Exact bytes written to stdout for this response (base64, without the trailing newline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_b64: Option<String>,
}

/// Logical tool result as emitted by the MCP server.
#[derive(Debug, Clone)]
pub struct ToolObservation {
    pub call_id: String,
    pub raw_transport: Vec<u8>,
    /// Raw JSON string token of the text content item, as written on the wire.
    pub raw_text_token: Option<String>,
    pub extracted_content: Vec<u8>,
    pub content_sha256: String,
    pub json: Option<Value>,
    pub is_error: bool,
    /// Ordered MCP content blocks (empty for a JSON-RPC error).
    pub blocks: Vec<crate::model::content::Block>,
    /// MCP `structuredContent`, if present.
    pub structured: Option<Value>,
    /// JSON-RPC error message, when the server answered with an error instead of a result.
    pub protocol_error: Option<String>,
    pub arguments: Option<Value>,
    pub generated: bool,
}

/// One provider-visible occurrence of a tool result (a `role: "tool"` message).
#[derive(Debug, Clone)]
pub struct ProviderObservation {
    /// Raw JSON token of the message `content` field.
    pub raw_content_token: Option<String>,
    pub extracted_content: Vec<u8>,
    pub content_sha256: String,
    /// 0 for string content, else the number of content parts.
    pub content_parts: usize,
    /// Image parts in order: SHA-256 of the decoded inline data (`None` for a remote URL).
    pub images: Vec<Option<String>>,
}
