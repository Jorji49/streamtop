//! Pure-Rust DNS-over-HTTPS resolution timing (passive, no unsafe code).

use std::time::Instant;

use color_eyre::eyre::{eyre, Result, WrapErr};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DohProvider {
    Cloudflare,
    Google,
    Custom(String),
}

impl DohProvider {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cloudflare" | "cf" => Ok(Self::Cloudflare),
            "google" | "google-public" => Ok(Self::Google),
            url if url.starts_with("http://") || url.starts_with("https://") => {
                Ok(Self::Custom(raw.trim().to_string()))
            }
            other => Err(eyre!(
                "invalid --doh-provider {other:?}; use cloudflare, google, or a custom DoH JSON URL"
            )),
        }
    }

    fn endpoint(&self, host: &str) -> String {
        match self {
            Self::Cloudflare => format!("https://cloudflare-dns.com/dns-query?name={host}&type=A"),
            Self::Google => format!("https://dns.google/resolve?name={host}&type=A"),
            Self::Custom(url) => {
                if url.contains('?') {
                    format!("{url}&name={host}&type=A")
                } else {
                    format!("{url}?name={host}&type=A")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DohResult {
    pub host: String,
    pub addresses: Vec<String>,
    pub doh_ms: u64,
}

#[derive(Debug, Deserialize)]
struct DohJsonAnswer {
    #[serde(default)]
    data: String,
}

#[derive(Debug, Deserialize)]
struct DohJsonResponse {
    #[serde(default, rename = "Answer")]
    answer: Vec<DohJsonAnswer>,
}

/// Resolve `host` via DoH JSON API and return lookup latency.
pub async fn resolve_doh(client: &Client, host: &str, provider: &DohProvider) -> Result<DohResult> {
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return Err(eyre!("DoH host is empty"));
    }
    let url = provider.endpoint(host);
    let started = Instant::now();
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/dns-json")
        .send()
        .await
        .wrap_err_with(|| format!("DoH GET failed: {url}"))?;
    if !response.status().is_success() {
        return Err(eyre!("DoH HTTP {} for {host}", response.status()));
    }
    let body: DohJsonResponse = response.json().await.wrap_err("DoH JSON parse failed")?;
    let doh_ms = started.elapsed().as_millis() as u64;
    let mut addresses = Vec::new();
    for ans in body.answer {
        if !ans.data.is_empty() && ans.data.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            addresses.push(ans.data);
        }
    }
    Ok(DohResult {
        host: host.to_string(),
        addresses,
        doh_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parse_cloudflare_and_custom() {
        assert_eq!(
            DohProvider::parse("cloudflare").expect("cf"),
            DohProvider::Cloudflare
        );
        let custom = DohProvider::parse("https://doh.example/dns-query").expect("custom");
        assert_eq!(
            custom,
            DohProvider::Custom("https://doh.example/dns-query".into())
        );
    }

    #[tokio::test]
    async fn doh_fallback_when_unreachable() {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("client");
        let err = resolve_doh(
            &client,
            "example.com",
            &DohProvider::Custom("http://127.0.0.1:1/dns".into()),
        )
        .await;
        assert!(err.is_err());
    }
}
