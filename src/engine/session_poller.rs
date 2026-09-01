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

/// Attach session DoH provider when `--doh-provider` is set.
pub fn apply_session_doh(poller: ManifestPoller, session: &SessionOpts) -> Result<ManifestPoller> {
    if let Some(provider) = parse_doh_provider(session)? {
        Ok(poller.with_doh_provider(Some(provider)))
    } else {
        Ok(poller)
    }
}
