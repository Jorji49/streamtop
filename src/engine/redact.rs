//! Redact secrets from URLs, headers, curl, HAR, and logs.

use std::borrow::Cow;

use url::Url;

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
    "id_token",
    "refresh_token",
    "jwt",
    "auth",
    "key",
    "api_key",
    "apikey",
    "signature",
    "sig",
    "policy",
    "hdnts",
    "hdnea",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "password",
    "secret",
    "session",
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

/// Redact userinfo, sensitive query parameters, and sensitive fragment params.
pub fn redact_url(url: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url) {
        let had_userinfo = !parsed.username().is_empty() || parsed.password().is_some();
        if had_userinfo {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
        }
        let mut out = parsed.to_string();
        // Url::to_string may re-encode; still scrub query/fragment by string pass.
        out = scrub_query_and_fragment(&out);
        if had_userinfo && !out.contains(REDACTED) {
            // Ensure credentials never leak even if query scrub was a no-op.
            if let Ok(mut again) = Url::parse(&out) {
                let _ = again.set_username("");
                let _ = again.set_password(None);
                return again.to_string();
            }
        }
        return out;
    }
    scrub_query_and_fragment(url)
}

fn scrub_query_and_fragment(url: &str) -> String {
    let (without_frag, frag) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let Some((base, query)) = without_frag.split_once('?') else {
        return frag.map_or_else(
            || without_frag.to_string(),
            |f| format!("{without_frag}#{}", scrub_param_string(f)),
        );
    };
    let mut out = format!("{base}?{}", scrub_param_string(query));
    if let Some(f) = frag {
        out.push('#');
        out.push_str(&scrub_param_string(f));
    }
    out
}

fn scrub_param_string(raw: &str) -> String {
    raw.split('&')
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
        .collect::<Vec<_>>()
        .join("&")
}

fn is_sensitive_query_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    SENSITIVE_QUERY_KEYS.iter().any(|s| k == *s)
        || k.starts_with("x-amz-")
        || k.contains("token")
        || k.contains("signature")
        || k.contains("secret")
        || k.contains("password")
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
    let s = ua?;
    let lower = s.to_ascii_lowercase();
    Some(if lower.contains("token") || lower.contains("key=") {
        REDACTED.to_string()
    } else {
        s.to_string()
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

    #[test]
    fn redacts_userinfo() {
        let r = redact_url("https://user:sekrit@cdn.example/path?ok=1");
        assert!(!r.contains("sekrit"));
        assert!(!r.contains("user:"));
        assert!(r.contains("cdn.example"));
    }

    #[test]
    fn redacts_fragment_token() {
        let r = redact_url("https://cdn.example/x#token=abc&n=1");
        assert!(r.contains("token=[REDACTED]"));
        assert!(r.contains("n=1"));
    }

    #[test]
    fn redacts_cdn_policy_and_jwt() {
        let r = redact_url("https://cdn.example/x?Policy=abc&jwt=eyJ&hdnts=1");
        assert!(r.contains("Policy=[REDACTED]"));
        assert!(r.contains("jwt=[REDACTED]"));
        assert!(r.contains("hdnts=[REDACTED]"));
    }
}
