use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// What came back for a sent request, or why nothing did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    /// Decoded bytes read.
    pub body_size: usize,
    /// Body bytes read off the wire, before `Content-Encoding` was decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_size: Option<u64>,
    /// The body was too large to keep whole; `body` holds only a prefix.
    pub body_stored: bool,
    #[serde(default)]
    pub is_binary: bool,
    /// Raw bytes of a binary body, for the live response's preview only.
    /// Never persisted, so history and collections don't fill with blobs.
    #[serde(skip)]
    pub binary_data: Option<Vec<u8>>,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// The redirects followed to get here, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirects: Vec<Redirect>,
    /// Live connection diagnostics for the final response.
    #[serde(skip)]
    pub connection: Option<ConnectionDetails>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDetails {
    pub http_version: &'static str,
    pub local_addr: Option<SocketAddr>,
    pub remote_addr: Option<SocketAddr>,
    pub tls: Option<TlsDetails>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsDetails {
    pub version: Option<&'static str>,
    /// Whether the client enforced certificate verification on this connection.
    pub verified: Option<bool>,
    pub certificate: Option<CertificateDetails>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateDetails {
    pub subject: String,
    pub issuer: String,
    pub valid_from: String,
    pub valid_until: String,
}

/// A redirect that was followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redirect {
    pub status: u16,
    /// Where it led.
    pub to: String,
}

/// The rich preview a binary response's content type calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryPreviewKind {
    Spreadsheet,
    /// A binary type with no richer preview than "Binary response".
    Other,
}

impl HttpResponse {
    /// Which preview `binary_data` should be rendered with, from Content-Type.
    pub fn binary_preview_kind(&self) -> BinaryPreviewKind {
        let content_type = self
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.to_ascii_lowercase())
            .unwrap_or_default();

        if content_type.contains("spreadsheet")
            || content_type.contains("ms-excel")
            || content_type.contains("opendocument.spreadsheet")
        {
            BinaryPreviewKind::Spreadsheet
        } else {
            BinaryPreviewKind::Other
        }
    }

    /// Whether the Content-Type is HTML.
    pub fn is_html(&self) -> bool {
        self.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                && value.to_ascii_lowercase().contains("text/html")
        })
    }
}
