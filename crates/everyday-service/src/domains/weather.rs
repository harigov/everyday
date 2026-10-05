//! The weather, for the Overview's own widget.
//!
//! One command: `weather`, a thin wrapper over
//! [`crate::weather::forecast`] -- the same function the assistant's
//! `get_weather` tool already calls in `crate::agent` -- for a caller that
//! is not the assistant. The Overview's Weather widget
//! (`ui/src/lib/dashboard.ts`) is added to the dashboard like any other
//! widget and reads this through its own `needs`.
//!
//! `place` defaults to the profile's location here too, but a vault with
//! neither a named place nor one in the profile gets
//! [`codes::NO_LOCATION`] rather than the tool's own prose refusal --
//! the interface turns that code into a button ("Set your location in
//! Settings → About You"), and a code is what a button can match on, not a
//! sentence written for a model to paraphrase.
//!
//! Results are cached for a few minutes per place -- see
//! [`crate::weather_cache`] -- so the widget's own fifteen-minute refresh,
//! and more than one window with it open, do not each cost a round trip.
//!
//! # Not gated on "let it use the web"
//!
//! `AgentSettings::web` (Settings → Assistant → "Let it use the web") is
//! what decides whether the assistant may reach `web_search`,
//! `read_web_page` and `get_weather` *on its own initiative*, mid-
//! conversation, deciding for itself that a question needs the network --
//! see `crate::agent`'s own doc on why those three are one switch. Putting
//! the Weather widget on the Overview is not that: it is a person reaching
//! for the network themselves, once, by hand, the same deliberate act as
//! typing a city into a browser. Gating it on the assistant's switch would
//! mean somebody who has never turned the assistant on -- or has turned
//! its web access off because they do not want *it* deciding to search --
//! could not see the weather on their own dashboard either, which answers a
//! question nobody asked. It stays off the Overview until it is added by
//! hand, which is the consent a widget already needs.
//!
//! Still refused on a locked vault, exactly like `web_search`: see
//! [`Service::require_unlocked`]'s own doc on why the lock screen must not
//! be where a request leaves the machine.

use std::sync::Arc;

use everyday_core::weather::{DEFAULT_DAYS, Report};
use serde::Deserialize;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWeather {
    /// A town or city, optionally with its region or country. Left out, or
    /// blank, for the profile's own location.
    #[serde(default)]
    pub place: Option<String>,
}

/// The forecast for `args.place`, or the profile's location when it is
/// left out or blank.
async fn weather(svc: Arc<Service>, _ctx: Ctx, args: GetWeather) -> CommandResult<Report> {
    let vault = svc.require_unlocked()?;
    let profile = {
        let vault = vault.clone();
        blocking(move || Ok(vault.profile()?)).await?
    };
    let named = args.place.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let place = named.unwrap_or(profile.location.trim());
    if place.is_empty() {
        return Err(CommandError::new(
            codes::NO_LOCATION,
            "no place was given and the profile has no location",
        ));
    }

    if let Some(cached) = svc.weather_cached(place) {
        return Ok(cached);
    }
    let report = crate::weather::forecast(place, DEFAULT_DAYS, None).await?;
    svc.weather_cache_put(place, report.clone());
    Ok(report)
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "weather", scope: Web, effect: Read,
    args: GetWeather, returns: "WeatherReport",
    signature: &[("place", "string | null", false)],
    run: weather,
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_or_missing_place_is_treated_the_same_as_none() {
        // `weather_args` in `crate::agent` already covers "blank place falls
        // back to the profile" for the assistant tool's own arguments; this
        // is the same rule, re-expressed for this command's `GetWeather`,
        // which is a different struct the surface snapshot tests read on
        // its own terms.
        let blank = GetWeather { place: Some("   ".into()) };
        assert_eq!(blank.place.as_deref().map(str::trim).filter(|p| !p.is_empty()), None);
        let named = GetWeather { place: Some(" Paris ".into()) };
        assert_eq!(named.place.as_deref().map(str::trim).filter(|p| !p.is_empty()), Some("Paris"));
    }
}
