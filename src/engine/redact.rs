//! Redact secrets from URLs, headers, curl, HAR, and logs.

use std::borrow::Cow;

const SENSITIVE_HEADER_NAMES: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
    "x-amz-security-token",
    "x-amz-credential",
    "x-amz-signature",
    "x-amz-signedheaders",
];

const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "token",
    "access_token",
    "auth",
    "key",
    "api_key",
    "apikey",
    "signature",
    "sig",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "password",
    "secret",
];

pub const REDACTED: &str = "[REDACTED]";

/// Mask sensitive header lines (`Key: Value`).
pub fn redact_header_line(line: &str) -> String {
    let Some((name, _)) = line.split_once(':') else {
        return line.to_string();
    };
    if is_sensitive_header(name.trim()) {
        format!("{}: {REDACTED}", name.trim())
    } else {
        line.to_string()
    }
}

pub fn redact_headers(headers: &[String]) -> Vec<String> {
    headers.iter().map(|h| redact_header_line(h)).collect()
}

pub fn is_sensitive_header(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    SENSITIVE_HEADER_NAMES.iter().any(|s| n == *s)
        || n.starts_with("x-amz-")
        || n.contains("token")
        || n.contains("authorization")
        || (n.contains("key") && !n.contains("keyformat"))
}

/// Redact sensitive query parameters in a URL (and fragment).
pub fn redact_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, frag) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };
    let parts: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if is_sensitive_query_key(k) {
                format!("{k}={REDACTED}")
            } else if v.is_empty() && !pair.contains('=') {
                k.to_string()
            } else {
                format!("{k}={v}")
            }
        })
        .collect();
    let mut out = if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parts.join("&"))
    };
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

fn is_sensitive_query_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    SENSITIVE_QUERY_KEYS.iter().any(|s| k == *s)
        || k.starts_with("x-amz-")
        || k.contains("token")
        || k.contains("signature")
        || k.contains("secret")
        || (k.contains("key") && k != "keyformat")
}

/// Redact secrets that appear as `Authorization: …` / `Cookie: …` inside free text.
pub fn redact_text(text: &str) -> String {
    let mut out = text.to_string();
    for name in [
        "Authorization",
        "Cookie",
        "Set-Cookie",
        "Proxy-Authorization",
        "X-Api-Key",
        "X-Auth-Token",
    ] {
        out = redact_inline_header(&out, name);
    }
    // Rough URL scrubbing for embedded http(s) links
    if out.contains("http") {
        out = scrub_urls_in_text(&out);
    }
    out
}

fn redact_inline_header(text: &str, name: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = format!("{}:", name.to_ascii_lowercase());
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    let mut rest_lower = lower.as_str();
    while let Some(idx) = rest_lower.find(&needle) {
        result.push_str(&rest[..idx]);
        result.push_str(name);
        result.push_str(": ");
        result.push_str(REDACTED);
        let after = idx + needle.len();
        let tail = &rest[after..];
        let trimmed = tail.trim_start();
        let skipped = tail.len() - trimmed.len();
        // Consume the full header value (spaces allowed) until a structural delimiter.
        let end = trimmed
            .find(['\n', '\r', '"', '\'', ',', '}'])
            .unwrap_or(trimmed.len());
        rest = &trimmed[end..];
        rest_lower = &rest_lower[after + skipped + end..];
    }
    result.push_str(rest);
    result
}

fn scrub_urls_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '>')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        out.push_str(&redact_url(url));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

pub fn redact_user_agent(ua: Option<&str>) -> Option<String> {
    ua.map(|s| {
        if s.to_ascii_lowercase().contains("token") || s.to_ascii_lowercase().contains("key=") {
            REDACTED.to_string()
        } else {
            s.to_string()
        }
    })
}

/// Cow helper when no change needed.
pub fn maybe_redact_url(url: &str) -> Cow<'_, str> {
    let red = redact_url(url);
    if red == url {
        Cow::Borrowed(url)
    } else {
        Cow::Owned(red)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_authorization_header() {
        assert_eq!(
            redact_header_line("Authorization: Bearer secret-token"),
            format!("Authorization: {REDACTED}")
        );
        assert_eq!(redact_header_line("X-Custom: ok"), "X-Custom: ok");
    }

    #[test]
    fn redacts_query_token() {
        let u = "https://cdn.example/seg.ts?token=abc&bitrate=1";
        let r = redact_url(u);
        assert!(r.contains("token=[REDACTED]"));
        assert!(r.contains("bitrate=1"));
    }

    #[test]
    fn redacts_x_amz_query() {
        let u = "https://s3.example/o?X-Amz-Signature=deadbeef&X-Amz-Date=1";
        let r = redact_url(u);
        assert!(r
            .to_ascii_lowercase()
            .contains("x-amz-signature=[redacted]"));
        assert!(!r.contains("deadbeef"));
    }

    #[test]
    fn redacts_text_cookie() {
        let t = redact_text("Cookie: session=abc\nOK");
        assert!(t.contains(&format!("Cookie: {REDACTED}")));
    }
}
