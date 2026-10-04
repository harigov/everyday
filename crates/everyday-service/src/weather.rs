//! The forecast, fetched.
//!
//! The other half of [`everyday_core::weather`], and the smaller one on
//! purpose: the core builds both addresses, reads both answers and decides
//! which of several Portlands was meant; this file opens the sockets in
//! between. The same split [`crate::websearch`] makes, for the same reason
//! -- everything here that could be wrong about the *weather* is testable
//! offline in the core, and all that is left to get wrong is the network.
//!
//! Through [`crate::http::client`], the shared one, rather than the
//! stranger-proof [`crate::http::public_client`]: both hosts are fixed in
//! the core and neither is a name a model or a web page chose, which is the
//! line `http`'s own module doc draws between the two.

use everyday_core::weather::{
    self, Lookup, MAX_RESPONSE_BYTES, PROVIDER, Place, Report, Units, lookups,
};

use crate::error::{CommandError, CommandResult, codes};
use crate::http;

/// The forecast for `place`, `days` days ahead.
///
/// `units` of `None` means "what is customary there" -- see
/// [`Units::customary_in`] -- which can only be decided once the place has
/// been found, so it is decided here rather than by the caller.
pub async fn forecast(place: &str, days: u8, units: Option<Units>) -> CommandResult<Report> {
    let found = find(place).await?;
    let units = units.unwrap_or_else(|| Units::customary_in(&found.country_code));
    let url = weather::forecast_url(&found, days, units);
    let body = get(&url, "the forecast").await?;
    weather::parse_forecast(&body, &found, units).map_err(|e| {
        tracing::warn!(error = %e, "could not read a forecast");
        CommandError::new(
            codes::UNREADABLE,
            format!("{PROVIDER} answered with a forecast this app could not read."),
        )
    })
}

/// Which place `place` is, asking the geocoder each of the core's
/// [`lookups`] in turn until one of them finds something.
async fn find(place: &str) -> CommandResult<Place> {
    let attempts: Vec<Lookup> = lookups(place);
    if attempts.is_empty() {
        return Err(CommandError::new(
            codes::INVALID,
            "no place was given to look the weather up for",
        ));
    }
    for lookup in attempts {
        let body = get(&lookup.url(), "the place").await?;
        let places = weather::parse_places(&body).map_err(|e| {
            tracing::warn!(error = %e, "could not read the geocoder's answer");
            CommandError::new(
                codes::UNREADABLE,
                format!("{PROVIDER} answered with a list of places this app could not read."),
            )
        })?;
        if let Some(found) = weather::choose(places, &lookup.qualifiers) {
            return Ok(found);
        }
    }
    Err(CommandError::new(
        codes::NOT_FOUND,
        format!(
            "{PROVIDER} knows no place called \u{201c}{}\u{201d}. Try the town or city's own \
             name, or the city and its country.",
            place.trim()
        ),
    ))
}

/// GET one of the two endpoints and hand the body back as text.
///
/// `what` names the half of the job this was, for a failure's sentence.
async fn get(url: &str, what: &str) -> CommandResult<String> {
    let response = http::client()?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| describe(&e, what))?;

    let status = response.status();
    if !status.is_success() {
        // Open-Meteo explains a 400 in its own body -- "forecast days is
        // invalid", "latitude must be in range" -- and that sentence is
        // better than any this file could guess at. Read small, and only
        // for that.
        let reason = if status.as_u16() == 400 {
            http::read_capped(response, 16 * 1024, String::new)
                .await
                .ok()
                .and_then(|b| weather::error_reason(&String::from_utf8_lossy(&b)))
        } else {
            None
        };
        return Err(CommandError::new(codes::NETWORK, explain_status(status, reason)));
    }

    let body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
        format!("{PROVIDER} sent more than this app will read for {what}")
    })
    .await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn explain_status(status: reqwest::StatusCode, reason: Option<String>) -> String {
    match (status.as_u16(), reason) {
        (400, Some(reason)) => format!("{PROVIDER} could not answer that: {reason}"),
        (401 | 403, _) => format!("{PROVIDER} refused the request."),
        (404 | 410, _) => format!("{PROVIDER} had nothing at that address."),
        (429, _) => format!(
            "{PROVIDER} asked us to slow down -- it limits how often it is asked. Try again \
             in a minute."
        ),
        (500..=599, _) => format!("{PROVIDER} is having trouble ({status})."),
        _ => format!("{PROVIDER} answered {status}."),
    }
}

fn describe(e: &reqwest::Error, what: &str) -> CommandError {
    let message = if e.is_timeout() {
        format!("{PROVIDER} did not answer in time.")
    } else if e.is_connect() {
        format!("could not reach {PROVIDER}. Check the network.")
    } else {
        // Without the URL: it carries the place, which is somewhere a
        // person lives.
        format!("looking up {what} failed: {}", http::strip_url(&e.to_string()))
    };
    CommandError::new(codes::NETWORK, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_quotes_open_meteos_own_reason() {
        let said = explain_status(
            reqwest::StatusCode::BAD_REQUEST,
            Some("Latitude must be in range of -90 to 90°.".into()),
        );
        assert!(said.contains("Latitude must be in range"), "got {said}");
        let busy = explain_status(reqwest::StatusCode::TOO_MANY_REQUESTS, None);
        assert!(busy.contains("slow down"), "got {busy}");
        let down = explain_status(reqwest::StatusCode::BAD_GATEWAY, None);
        assert!(down.contains("having trouble"), "got {down}");
    }
}
