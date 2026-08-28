//! DNS resolution, SSRF blocklists, and pinned socket selection for outbound HTTP.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use color_eyre::eyre::{eyre, Result};
use url::Url;

/// Reject destinations that resolve to loopback, private, link-local, or cloud metadata.
pub fn validate_outbound_url(raw: &str, allow_insecure: bool) -> Result<()> {
    if allow_insecure {
        return Ok(());
    }
    let parsed = Url::parse(raw).map_err(|e| eyre!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(eyre!("scheme `{other}` not allowed (use http/https)")),
    }
    let host = parsed.host_str().ok_or_else(|| eyre!("URL missing host"))?;
    if is_blocked_hostname(host) {
        return Err(eyre!(
            "host `{host}` blocked (loopback/internal); use --allow-insecure-webhooks to override"
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(eyre!(
                "IP {ip} blocked (private/link-local/metadata); use --allow-insecure-webhooks to override"
            ));
        }
        return Ok(());
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = resolve_pinned_addrs(host, port, allow_insecure)?;
    if addrs.is_empty() {
        return Err(eyre!("host `{host}` resolved to no addresses"));
    }
    Ok(())
}

pub fn is_blocked_hostname(host: &str) -> bool {
    let host_l = host.to_ascii_lowercase();
    host_l == "localhost"
        || host_l.ends_with(".localhost")
        || host_l == "metadata.google.internal"
        || host_l == "metadata.azure.com"
        || host_l == "metadata.goog"
        || host_l == "instance-data.ec2.internal"
        || host_l.ends_with(".internal")
        || host_l.ends_with(".local")
}

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || o[0] == 0
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let octets = ip.octets();
    if (octets[0] & 0xfe) == 0xfc {
        return true;
    }
    if octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80 {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    false
}

/// Resolve host and return connectable addresses (all must pass SSRF checks when enforced).
pub fn resolve_pinned_addrs(
    host: &str,
    port: u16,
    allow_insecure: bool,
) -> Result<Vec<SocketAddr>> {
    if !allow_insecure {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return Err(eyre!("blocked literal IP {ip}"));
            }
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| eyre!("DNS resolve failed for `{host}`: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(eyre!("DNS returned no addresses for `{host}`"));
    }
    if !allow_insecure {
        for addr in &addrs {
            if is_blocked_ip(addr.ip()) {
                return Err(eyre!(
                    "host `{host}` resolves to blocked address {}; use --allow-insecure-webhooks to override",
                    addr.ip()
                ));
            }
        }
    }
    Ok(addrs)
}

/// Prefer IPv4 for CDN compatibility; stable ordering for pinning.
pub fn pick_connect_addr(addrs: &[SocketAddr]) -> SocketAddr {
    if addrs.is_empty() {
        return SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    }
    let mut sorted: Vec<SocketAddr> = addrs.to_vec();
    sorted.sort_by_key(|a| if a.is_ipv4() { 0 } else { 1 });
    sorted[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_metadata_and_private() {
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(is_blocked_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn validate_blocks_loopback_url() {
        assert!(validate_outbound_url("http://127.0.0.1/hook", false).is_err());
        assert!(validate_outbound_url("http://127.0.0.1:9/hook", true).is_ok());
    }
}
