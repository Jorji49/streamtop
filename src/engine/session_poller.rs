//! Shared `ManifestPoller` wiring from `SessionOpts`.

use color_eyre::eyre::Result;

use crate::engine::doh::DohProvider;
use crate::engine::ManifestPoller;
use crate::ui::app::SessionOpts;

pub fn parse_doh_provider(session: &SessionOpts) -> Result<Option<DohProvider>> {
    match &session.doh_provider {
        Some(raw) => Ok(Some(DohProvider::parse(raw)?)),
        None => Ok(None),
    }
}

/// Attach session DoH, OTLP export, and evidence capture.
pub fn apply_session_doh(poller: ManifestPoller, session: &SessionOpts) -> Result<ManifestPoller> {
    let poller = if let Some(provider) = parse_doh_provider(session)? {
        poller.with_doh_provider(Some(provider))
    } else {
        poller
    };
    let poller = poller.with_session_otel(session)?;
    Ok(poller.with_session_evidence(session))
}
