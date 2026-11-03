//! Shared HTTP plumbing for the provider transports.
//!
//! Both [`crate::client::ReqwestTransport`] (Anthropic) and
//! [`crate::openai::OpenAiTransport`] (OpenAI-compatible) go through this
//! module, so the security- and robustness-relevant rules live in ONE place:
//!
//! - **Base-URL validation** ([`validate_base_url`]): the API key is attached to
//!   every request, so the base URL must be `https://` — plain `http://` is
//!   allowed only for loopback hosts (`localhost`, `127.0.0.0/8`, `::1`, for
//!   local LLM servers). Other schemes, an empty host, userinfo
//!   (`user:pass@`), a query string, or a fragment are rejected. The transports
//!   re-validate before every send, so a bad stored URL fails closed (the key
//!   is never sent).
//! - **Endpoint joining** ([`join_endpoint`]): trailing slashes are trimmed and
//!   a base that already ends in `/v1` does not get a second `/v1`.
//! - **Timeouts**: a connect timeout ([`CONNECT_TIMEOUT`]) plus a *read*
//!   (idle-between-chunks) timeout — never a whole-request deadline, which
//!   would kill long streamed thinking turns.
//! - **Body cap** ([`MAX_SSE_BODY_BYTES`]): the accumulated SSE body is bounded.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use crate::client::{RawResponse, TransportError};

/// Connect-phase timeout for the provider HTTP clients.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Hard cap on the buffered SSE response body (32 MiB). A response that grows
/// past this fails with [`TransportError::BodyTooLarge`].
pub const MAX_SSE_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Validate a provider base URL before any credential is sent to it.
///
/// Accepts `https://<host>[:port][/path]`, or `http://` when the host is
/// loopback (`localhost`, any `127.0.0.0/8` address, or `[::1]`). Rejects any
/// other scheme, an empty host, userinfo in the URL, a query string, and a
/// fragment. Leading/trailing whitespace is rejected too (callers should trim
/// first — a padded URL is almost certainly a paste error).
///
/// Returns a human-readable reason on rejection, suitable for surfacing in the
/// settings UI.
pub fn validate_base_url(base_url: &str) -> Result<(), String> {
    if base_url.is_empty() {
        return Err("base URL is empty".to_string());
    }
    if base_url.trim() != base_url {
        return Err("base URL has leading or trailing whitespace".to_string());
    }
    let url = reqwest::Url::parse(base_url).map_err(|e| format!("invalid base URL: {e}"))?;
    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err("base URL has no host".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("base URL must not contain credentials (user:pass@)".to_string());
    }
    if url.query().is_some() {
        return Err("base URL must not contain a query string".to_string());
    }
    if url.fragment().is_some() {
        return Err("base URL must not contain a fragment".to_string());
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        "http" => Err(format!(
            "base URL must use https:// (plain http:// is only allowed for localhost / 127.0.0.0/8 / ::1, got host {host:?})"
        )),
        other => Err(format!(
            "base URL scheme {other:?} is not allowed (use https://)"
        )),
    }
}

/// Whether a URL host (as returned by `Url::host_str`) is loopback.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return v4.is_loopback();
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(v6) = bare.parse::<Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// Validate `base_url` and join it with an API `path` that starts with `/v1/`
/// (e.g. `/v1/messages`, `/v1/chat/completions`).
///
/// Trailing slashes on the base are trimmed (`https://host/` → no `//v1`), and
/// when the base already ends in `/v1` the path's `/v1` prefix is not repeated
/// (`http://localhost:11434/v1` → `http://localhost:11434/v1/chat/completions`).
/// For the default bases the result is byte-identical to the historical
/// `format!("{base}/v1/...")`.
pub fn join_endpoint(base_url: &str, path: &str) -> Result<String, String> {
    validate_base_url(base_url)?;
    let base = base_url.trim_end_matches('/');
    let path = if base.ends_with("/v1") {
        path.strip_prefix("/v1").unwrap_or(path)
    } else {
        path
    };
    Ok(format!("{base}{path}"))
}

/// Metadata about a response that is not part of [`RawResponse`] (kept
/// separate so `RawResponse` stays a plain two-field struct for test doubles).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResponseMeta {
    /// The `retry-after` header, when present and expressed in seconds.
    pub retry_after: Option<Duration>,
}

/// Parse a `retry-after` header value given in (possibly fractional) seconds.
/// HTTP-date values are ignored (`None`).
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some(Duration::from_secs_f64(secs.min(3600.0)))
    } else {
        None
    }
}

/// The reqwest client + dedicated current-thread runtime both transports use.
pub(crate) struct HttpCore {
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl HttpCore {
    /// Build the client with [`CONNECT_TIMEOUT`] and, when `read_timeout` is
    /// `Some`, a per-read idle timeout (reset after every received chunk).
    /// There is deliberately no total-request deadline.
    pub(crate) fn new(read_timeout: Option<Duration>) -> Self {
        let mut builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
        if let Some(d) = read_timeout {
            builder = builder.read_timeout(d);
        }
        Self {
            http: builder
                .build()
                .expect("failed to build morphe-ai reqwest client"),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build morphe-ai tokio runtime"),
        }
    }

    /// POST `body` to `url` with `headers`, stream the response body (capped at
    /// [`MAX_SSE_BODY_BYTES`]) and return it with the response metadata.
    pub(crate) fn post(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<(RawResponse, ResponseMeta), TransportError> {
        let http = self.http.clone();
        self.runtime.block_on(async move {
            let mut rb = http.post(url);
            for (k, v) in headers {
                rb = rb.header(*k, v);
            }
            let resp = rb
                .body(body)
                .send()
                .await
                .map_err(|e| TransportError::Http(e.to_string()))?;
            let status = resp.status().as_u16();
            let meta = ResponseMeta {
                retry_after: resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after),
            };
            use futures_util::StreamExt;
            let mut acc = BodyAccumulator::new(MAX_SSE_BODY_BYTES);
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                acc.push(&chunk.map_err(|e| TransportError::Http(e.to_string()))?)?;
            }
            Ok((
                RawResponse {
                    status,
                    body: acc.into_string()?,
                },
                meta,
            ))
        })
    }
}

/// Size-capped byte accumulator for a streamed body (pure; unit-tested).
pub(crate) struct BodyAccumulator {
    buf: Vec<u8>,
    limit: usize,
}

impl BodyAccumulator {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        if self.buf.len().saturating_add(chunk.len()) > self.limit {
            return Err(TransportError::BodyTooLarge { limit: self.limit });
        }
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    pub(crate) fn into_string(self) -> Result<String, TransportError> {
        String::from_utf8(self.buf).map_err(|e| TransportError::Http(format!("non-utf8 body: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls_are_accepted() {
        for ok in [
            "https://api.anthropic.com",
            "https://api.openai.com/",
            "https://openrouter.ai/api/v1",
            "https://example.com:8443/proxy",
        ] {
            assert!(validate_base_url(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn http_is_allowed_only_for_loopback() {
        for ok in [
            "http://localhost:11434/v1",
            "http://LOCALHOST:8080",
            "http://127.0.0.1:1234",
            "http://127.8.9.10",
            "http://[::1]:8080/v1",
        ] {
            assert!(validate_base_url(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in [
            "http://api.anthropic.com",
            "http://192.168.1.10:11434",
            "http://10.0.0.1",
            "http://localhost.evil.com",
            "http://[::2]",
            "http://0.0.0.0",
        ] {
            let err = validate_base_url(bad).expect_err(bad);
            assert!(err.contains("https"), "{bad}: {err}");
        }
    }

    #[test]
    fn other_schemes_userinfo_and_junk_are_rejected() {
        for bad in [
            "",
            " https://api.anthropic.com",
            "ftp://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "api.anthropic.com",
            "https://user:pass@api.anthropic.com",
            "https://user@api.anthropic.com",
            "https://api.anthropic.com/?x=1",
            "https://api.anthropic.com/#frag",
            "https://",
        ] {
            assert!(
                validate_base_url(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn join_keeps_default_anthropic_url_byte_identical() {
        assert_eq!(
            join_endpoint("https://api.anthropic.com", "/v1/messages").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_endpoint("https://api.openai.com", "/v1/chat/completions").unwrap(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn join_trims_trailing_slashes() {
        assert_eq!(
            join_endpoint("https://api.anthropic.com/", "/v1/messages").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_endpoint("https://proxy.example/anthropic//", "/v1/messages").unwrap(),
            "https://proxy.example/anthropic/v1/messages"
        );
    }

    #[test]
    fn join_does_not_double_v1() {
        assert_eq!(
            join_endpoint("http://localhost:11434/v1", "/v1/chat/completions").unwrap(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            join_endpoint("https://openrouter.ai/api/v1/", "/v1/chat/completions").unwrap(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        // A path segment that merely ends in "v1" text is not a /v1 segment.
        assert_eq!(
            join_endpoint("https://example.com/apiv1", "/v1/chat/completions").unwrap(),
            "https://example.com/apiv1/v1/chat/completions"
        );
    }

    #[test]
    fn join_rejects_invalid_base() {
        assert!(join_endpoint("http://evil.example", "/v1/messages").is_err());
    }

    #[test]
    fn retry_after_parses_seconds_only() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(
            parse_retry_after(" 1.5 "),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn body_accumulator_caps_size() {
        let mut acc = BodyAccumulator::new(8);
        acc.push(b"1234").unwrap();
        acc.push(b"5678").unwrap();
        match acc.push(b"9") {
            Err(TransportError::BodyTooLarge { limit }) => assert_eq!(limit, 8),
            other => panic!("expected BodyTooLarge, got {other:?}"),
        }
        let mut ok = BodyAccumulator::new(8);
        ok.push(b"abc").unwrap();
        assert_eq!(ok.into_string().unwrap(), "abc");
    }

    #[test]
    fn http_core_builds_with_and_without_read_timeout() {
        let _a = HttpCore::new(Some(Duration::from_secs(5)));
        let _b = HttpCore::new(None);
    }
}
