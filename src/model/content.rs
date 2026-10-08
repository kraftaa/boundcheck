//! Protocol-neutral model of an MCP tool result's content.
//!
//! An MCP result is an ordered list of blocks plus optional
//! `structuredContent`, `_meta` and error status, or a JSON-RPC error. A
//! provider protocol may not be able to carry every block type in a tool
//! result; what it can carry is declared by [`Representable`], so the
//! comparison never calls a required transformation a corruption.

use crate::compare::content::sha256_hex;
use base64::Engine;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// Image or audio: identified by MIME type and the hash of the decoded bytes.
    Image {
        mime: String,
        bytes: usize,
        sha256: String,
        #[serde(skip)]
        base64: String,
    },
    Audio {
        mime: String,
        bytes: usize,
        sha256: String,
        #[serde(skip)]
        base64: String,
    },
    ResourceLink {
        uri: String,
    },
    /// Embedded resource with either `text` or a base64 `blob`.
    Resource {
        uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        blob_sha256: Option<String>,
        #[serde(skip)]
        blob_base64: Option<String>,
    },
    Other {
        kind: String,
    },
}

impl Block {
    pub fn kind(&self) -> &str {
        match self {
            Block::Text { .. } => "text",
            Block::Image { .. } => "image",
            Block::Audio { .. } => "audio",
            Block::ResourceLink { .. } => "resource_link",
            Block::Resource { .. } => "resource",
            Block::Other { kind } => kind,
        }
    }

    /// Exact strings whose presence in the provider request proves the block
    /// was carried in some form (base64 data, URI, embedded text).
    pub fn markers(&self) -> Vec<&str> {
        match self {
            Block::Text { text } => vec![text.as_str()],
            Block::Image { base64, .. } | Block::Audio { base64, .. } => vec![base64.as_str()],
            Block::ResourceLink { uri } => vec![uri.as_str()],
            Block::Resource { uri, text, blob_base64, .. } => {
                let mut m = vec![uri.as_str()];
                m.extend(text.as_deref());
                m.extend(blob_base64.as_deref());
                m
            }
            Block::Other { .. } => vec![],
        }
    }
}

fn decoded(data: &str) -> (usize, String) {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_else(|_| data.as_bytes().to_vec());
    (bytes.len(), sha256_hex(&bytes))
}

/// Parse an MCP `content` array.
pub fn parse_blocks(content: &[Value]) -> Vec<Block> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    content
        .iter()
        .map(|b| match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => Block::Text { text: s(b, "text") },
            "image" => {
                let (bytes, sha256) = decoded(&s(b, "data"));
                Block::Image { mime: s(b, "mimeType"), bytes, sha256, base64: s(b, "data") }
            }
            "audio" => {
                let (bytes, sha256) = decoded(&s(b, "data"));
                Block::Audio { mime: s(b, "mimeType"), bytes, sha256, base64: s(b, "data") }
            }
            "resource_link" => Block::ResourceLink { uri: s(b, "uri") },
            "resource" => {
                let r = b.get("resource").cloned().unwrap_or(Value::Null);
                let blob = r.get("blob").and_then(Value::as_str).map(str::to_owned);
                Block::Resource {
                    uri: s(&r, "uri"),
                    text: r.get("text").and_then(Value::as_str).map(str::to_owned),
                    blob_sha256: blob.as_deref().map(|d| decoded(d).1),
                    blob_base64: blob,
                }
            }
            other => Block::Other { kind: other.to_owned() },
        })
        .collect()
}

/// Which MCP block types a provider protocol can carry in a tool result.
#[derive(Debug, Clone, Copy)]
pub struct Representable {
    pub image: bool,
    pub audio: bool,
    pub resource: bool,
}

impl Representable {
    /// OpenAI Chat Completions: `role: "tool"` content is a string or text parts only.
    pub const CHAT_COMPLETIONS: Representable = Representable { image: false, audio: false, resource: false };
    /// OpenAI Responses: `function_call_output.output` may hold `input_text` and `input_image` items.
    pub const RESPONSES: Representable = Representable { image: true, audio: false, resource: false };

    pub fn carries(&self, b: &Block) -> bool {
        match b {
            Block::Text { .. } => true,
            Block::Image { .. } => self.image,
            Block::Audio { .. } => self.audio,
            Block::ResourceLink { .. } | Block::Resource { .. } | Block::Other { .. } => self.resource,
        }
    }
}

/// Separators a runtime may use when presenting several text blocks as one string.
pub const TEXT_JOINS: &[&str] = &["", "\n", "\n\n"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_every_block_type() {
        let blocks = parse_blocks(&[
            json!({"type": "text", "text": "a"}),
            json!({"type": "image", "data": "AAEC", "mimeType": "image/png"}),
            json!({"type": "audio", "data": "AAEC", "mimeType": "audio/wav"}),
            json!({"type": "resource_link", "uri": "bc://x"}),
            json!({"type": "resource", "resource": {"uri": "bc://y", "text": "t"}}),
            json!({"type": "resource", "resource": {"uri": "bc://z", "blob": "AAEC"}}),
            json!({"type": "future_kind"}),
        ]);
        let kinds: Vec<&str> = blocks.iter().map(Block::kind).collect();
        assert_eq!(kinds, ["text", "image", "audio", "resource_link", "resource", "resource", "future_kind"]);
        assert!(matches!(&blocks[1], Block::Image { bytes: 3, .. }));
        assert_eq!(blocks[4].markers(), vec!["bc://y", "t"]);
        assert!(!Representable::CHAT_COMPLETIONS.carries(&blocks[1]));
        assert!(Representable::CHAT_COMPLETIONS.carries(&blocks[0]));
    }
}
