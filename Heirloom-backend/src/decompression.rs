/// Request decompression middleware for Heirloom-Protocol backend.
///
/// # Overview
///
/// Clients may compress request bodies using `Content-Encoding: gzip` to reduce
/// bandwidth. This module provides:
///
/// - A [`DecompressionConfig`] that is loaded from environment variables.
/// - A [`decompress_request`] axum extractor / layer helper that decodes gzip
///   bodies before the inner handler sees them.
/// - Decompression size limits to guard against zip-bomb payloads.
///
/// # Usage
///
/// ```rust,ignore
/// use heirloom_protocol_backend::decompression::{DecompressionConfig, decompress_body};
///
/// // In build_router, add the tower-http RequestDecompressionLayer:
/// use tower_http::decompression::RequestDecompressionLayer;
/// let app = Router::new()
///     /* routes … */
///     .layer(RequestDecompressionLayer::new());
/// ```
///
/// The tower-http `RequestDecompressionLayer` transparently decodes
/// `Content-Encoding: gzip`, `deflate`, and `br` bodies before they reach
/// handlers. The config below controls the max allowed decompressed body size;
/// oversized bodies yield **413 Payload Too Large**.
use std::io::Read;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Decompression configuration loaded from environment variables.
///
/// | Variable | Default | Description |
/// |---|---|---|
/// | `DECOMP_MAX_BODY_BYTES` | `10_485_760` (10 MiB) | Maximum decompressed body size |
/// | `DECOMP_ENABLED` | `true` | Toggle middleware on/off |
#[derive(Debug, Clone)]
pub struct DecompressionConfig {
    /// Maximum number of bytes allowed after decompression.
    /// Requests whose decompressed body exceeds this limit are rejected with 413.
    pub max_body_bytes: usize,
    /// Whether request decompression is enabled at all.
    pub enabled: bool,
}

impl Default for DecompressionConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 10 * 1024 * 1024, // 10 MiB
            enabled: true,
        }
    }
}

impl DecompressionConfig {
    /// Build config from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        Self {
            max_body_bytes: std::env::var("DECOMP_MAX_BODY_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 1024 * 1024),
            enabled: std::env::var("DECOMP_ENABLED")
                .ok()
                .map(|v| v.to_lowercase() != "false" && v != "0")
                .unwrap_or(true),
        }
    }
}

// ── Decompression helpers ──────────────────────────────────────────────────────

/// Supported `Content-Encoding` values that we can decompress.
#[derive(Debug, PartialEq, Eq)]
pub enum ContentEncoding {
    Gzip,
    Deflate,
    Identity,
    Unknown(String),
}

impl ContentEncoding {
    /// Parse from the raw `Content-Encoding` header value.
    pub fn from_header(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "gzip" | "x-gzip" => Self::Gzip,
            "deflate" => Self::Deflate,
            "identity" | "" => Self::Identity,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// Decompress `bytes` that are gzip-encoded.
///
/// Returns the decompressed bytes, or an error string suitable for logging.
/// The `max_bytes` guard prevents decompression-bomb attacks: if the
/// decompressed stream exceeds `max_bytes` the function returns `Err`.
pub fn decompress_gzip(bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;

    let decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();

    // Read with a hard cap to avoid unbounded memory growth.
    let mut limited = decoder.take(max_bytes as u64 + 1);
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("gzip decompression failed: {e}"))?;

    if out.len() > max_bytes {
        return Err(format!(
            "decompressed body exceeds limit of {max_bytes} bytes"
        ));
    }

    Ok(out)
}

/// Decompress `bytes` that are deflate-encoded (raw zlib).
///
/// Same size-limit semantics as [`decompress_gzip`].
pub fn decompress_deflate(bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, String> {
    use flate2::read::ZlibDecoder;

    let decoder = ZlibDecoder::new(bytes);
    let mut out = Vec::new();
    let mut limited = decoder.take(max_bytes as u64 + 1);
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("deflate decompression failed: {e}"))?;

    if out.len() > max_bytes {
        return Err(format!(
            "decompressed body exceeds limit of {max_bytes} bytes"
        ));
    }

    Ok(out)
}

/// Decompress `body_bytes` according to `content_encoding`.
///
/// Returns `Ok(decompressed)` or an error string.
/// `Identity` (no encoding) passes the bytes through unchanged.
/// Unknown encodings are also passed through unchanged (letting the inner
/// handler deal with them — most will 400 on their own).
pub fn decompress_body(
    body_bytes: &[u8],
    content_encoding: &str,
    config: &DecompressionConfig,
) -> Result<Vec<u8>, String> {
    if !config.enabled {
        return Ok(body_bytes.to_vec());
    }

    match ContentEncoding::from_header(content_encoding) {
        ContentEncoding::Gzip => decompress_gzip(body_bytes, config.max_body_bytes),
        ContentEncoding::Deflate => decompress_deflate(body_bytes, config.max_body_bytes),
        ContentEncoding::Identity | ContentEncoding::Unknown(_) => {
            // Pass through; identity / unknown encodings are not our concern.
            Ok(body_bytes.to_vec())
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    fn gzip_encode(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn deflate_encode(data: &[u8]) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn test_content_encoding_from_header() {
        assert_eq!(ContentEncoding::from_header("gzip"), ContentEncoding::Gzip);
        assert_eq!(
            ContentEncoding::from_header("x-gzip"),
            ContentEncoding::Gzip
        );
        assert_eq!(
            ContentEncoding::from_header("deflate"),
            ContentEncoding::Deflate
        );
        assert_eq!(
            ContentEncoding::from_header("identity"),
            ContentEncoding::Identity
        );
        assert_eq!(ContentEncoding::from_header(""), ContentEncoding::Identity);
        assert_eq!(
            ContentEncoding::from_header("br"),
            ContentEncoding::Unknown("br".to_string())
        );
    }

    #[test]
    fn test_decompress_gzip_roundtrip() {
        let original = b"hello heirloom-protocol vault data";
        let compressed = gzip_encode(original);
        let decompressed = decompress_gzip(&compressed, 1024).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_decompress_deflate_roundtrip() {
        let original = b"hello heirloom-protocol deflate data";
        let compressed = deflate_encode(original);
        let decompressed = decompress_deflate(&compressed, 1024).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_decompress_gzip_exceeds_limit() {
        // Compress a 100-byte payload but allow only 50 bytes out.
        let original = vec![b'x'; 100];
        let compressed = gzip_encode(&original);
        let result = decompress_gzip(&compressed, 50);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeds limit"));
    }

    #[test]
    fn test_decompress_body_gzip() {
        let config = DecompressionConfig::default();
        let original = b"vault payload compressed";
        let compressed = gzip_encode(original);
        let out = decompress_body(&compressed, "gzip", &config).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn test_decompress_body_identity_passthrough() {
        let config = DecompressionConfig::default();
        let data = b"plain body no encoding";
        let out = decompress_body(data, "identity", &config).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn test_decompress_body_disabled() {
        let config = DecompressionConfig {
            enabled: false,
            ..Default::default()
        };
        // Even gzip bytes should pass through unchanged when disabled.
        let original = b"raw bytes";
        let out = decompress_body(original, "gzip", &config).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn test_config_defaults() {
        let cfg = DecompressionConfig::default();
        assert_eq!(cfg.max_body_bytes, 10 * 1024 * 1024);
        assert!(cfg.enabled);
    }

    #[test]
    fn test_config_from_env() {
        std::env::set_var("DECOMP_MAX_BODY_BYTES", "2048");
        std::env::set_var("DECOMP_ENABLED", "true");
        let cfg = DecompressionConfig::from_env();
        assert_eq!(cfg.max_body_bytes, 2048);
        assert!(cfg.enabled);
        std::env::remove_var("DECOMP_MAX_BODY_BYTES");
        std::env::remove_var("DECOMP_ENABLED");
    }

    #[test]
    fn test_config_disabled_via_env() {
        std::env::set_var("DECOMP_ENABLED", "false");
        let cfg = DecompressionConfig::from_env();
        assert!(!cfg.enabled);
        std::env::remove_var("DECOMP_ENABLED");
    }
}
