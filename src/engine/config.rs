//! User profiles from platform config dir (`streamtop/config.toml`).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use color_eyre::eyre::{eyre, Result, WrapErr};
use serde::Deserialize;

use crate::ui::app::SessionOpts;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub default: ProfileSection,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileSection {
    #[serde(default)]
    pub headers: Vec<String>,
    pub user_agent: Option<String>,
    pub interval_ms: Option<u64>,
    pub probe_headers: Option<bool>,
    pub probe_drm: Option<bool>,
    pub webhook: Option<String>,
    pub alert_on: Option<String>,
    pub allow_insecure_webhooks: Option<bool>,
    pub otel_endpoint: Option<String>,
    pub allow_insecure_otel: Option<bool>,
    pub tr101290: Option<bool>,
    pub probe_sei: Option<bool>,
    pub simulate_player: Option<bool>,
    pub throttle_kbps: Option<u64>,
    pub simulated_rtt_ms: Option<u64>,
}

/// Resolve config path: `$STREAMTOP_CONFIG` or platform config dir.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("STREAMTOP_CONFIG") {
        return PathBuf::from(p);
    }
    if let Some(base) = directories::BaseDirs::new() {
        return base.config_dir().join("streamtop").join("config.toml");
    }
    PathBuf::from("streamtop").join("config.toml")
}

pub fn load_config_file() -> Result<Option<ConfigFile>> {
    let path = config_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).wrap_err_with(|| format!("read {}", path.display()))?;
    let cfg: ConfigFile =
        toml::from_str(&raw).wrap_err_with(|| format!("parse {}", path.display()))?;
    Ok(Some(cfg))
}

/// Merge default + named profile into a base session; CLI values applied by caller afterward.
pub fn session_from_profile(
    profile_name: Option<&str>,
    mut base: SessionOpts,
) -> Result<SessionOpts> {
    let Some(cfg) = load_config_file()? else {
        if profile_name.is_some() {
            return Err(eyre!(
                "profile requested but no config at {}",
                config_path().display()
            ));
        }
        return Ok(base);
    };

    apply_section(&mut base, &cfg.default);

    if let Some(name) = profile_name {
        let section = cfg.profiles.get(name).ok_or_else(|| {
            eyre!(
                "unknown profile `{name}` (available: {})",
                cfg.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        apply_section(&mut base, section);
    }

    Ok(base)
}

fn apply_section(session: &mut SessionOpts, section: &ProfileSection) {
    if !section.headers.is_empty() {
        session.headers.clone_from(&section.headers);
    }
    if section.user_agent.is_some() {
        session.user_agent.clone_from(&section.user_agent);
    }
    if section.interval_ms.is_some() {
        session.interval_ms = section.interval_ms;
    }
    if let Some(p) = section.probe_headers {
        session.probe_headers = p;
    }
    if let Some(p) = section.probe_drm {
        session.probe_drm = p;
    }
    if section.webhook.is_some() {
        session.webhook_url.clone_from(&section.webhook);
    }
    if let Some(a) = &section.alert_on {
        session.alert_on.clone_from(a);
    }
    if let Some(v) = section.allow_insecure_webhooks {
        session.allow_insecure_webhooks = v;
    }
    if section.otel_endpoint.is_some() {
        session.otel_endpoint.clone_from(&section.otel_endpoint);
    }
    if let Some(v) = section.allow_insecure_otel {
        session.allow_insecure_otel = v;
    }
    if let Some(v) = section.tr101290 {
        session.tr101290 = v;
    }
    if let Some(v) = section.probe_sei {
        session.probe_sei = v;
    }
    if let Some(v) = section.simulate_player {
        session.simulate_player = v;
    }
    if section.throttle_kbps.is_some() {
        session.throttle_kbps = section.throttle_kbps;
    }
    if section.simulated_rtt_ms.is_some() {
        session.simulated_rtt_ms = section.simulated_rtt_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_section_overrides() {
        let mut s = SessionOpts {
            headers: vec!["A: 1".into()],
            user_agent: Some("old".into()),
            interval_ms: Some(1000),
            probe_headers: false,
            probe_drm: false,
            clearkey: None,
            export_incident: None,
            webhook_url: None,
            alert_on: "stall".into(),
            allow_insecure_webhooks: false,
            allow_insecure_otel: false,
            otel_endpoint: None,
            tr101290: false,
            probe_sei: false,
            simulate_player: false,
            throttle_kbps: None,
            simulated_rtt_ms: None,
            doh_provider: None,
        };
        apply_section(
            &mut s,
            &ProfileSection {
                headers: vec!["B: 2".into()],
                user_agent: Some("new".into()),
                interval_ms: None,
                probe_headers: Some(true),
                probe_drm: Some(true),
                webhook: Some("https://hooks.example".into()),
                alert_on: Some("http_5xx".into()),
                allow_insecure_webhooks: Some(true),
                otel_endpoint: Some("http://127.0.0.1:4318".into()),
                allow_insecure_otel: Some(true),
                tr101290: Some(true),
                probe_sei: Some(true),
                simulate_player: Some(true),
                throttle_kbps: Some(1500),
                simulated_rtt_ms: Some(80),
            },
        );
        assert_eq!(s.headers, vec!["B: 2".to_string()]);
        assert_eq!(s.user_agent.as_deref(), Some("new"));
        assert!(s.probe_headers);
        assert!(s.probe_drm);
        assert_eq!(s.webhook_url.as_deref(), Some("https://hooks.example"));
        assert_eq!(s.alert_on, "http_5xx");
        assert!(s.allow_insecure_webhooks);
        assert!(s.allow_insecure_otel);
        assert_eq!(s.otel_endpoint.as_deref(), Some("http://127.0.0.1:4318"));
        assert!(s.tr101290);
        assert!(s.probe_sei);
        assert!(s.simulate_player);
        assert_eq!(s.throttle_kbps, Some(1500));
        assert_eq!(s.simulated_rtt_ms, Some(80));
        assert_eq!(s.interval_ms, Some(1000));
    }
}
