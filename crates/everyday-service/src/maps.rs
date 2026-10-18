//! Places and directions, fetched -- and the assistant's two tools that ask
//! for them.
//!
//! The other half of [`everyday_core::maps`], and the smaller one on purpose:
//! the core builds every address, reads every answer and decides how a
//! distance or a departure time reads; this file opens the sockets in
//! between. The same split [`crate::weather`] makes, for its reason --
//! everything here that could be wrong about a *place* is testable offline
//! in the core, and all that is left to get wrong is the network.
//!
//! Through [`crate::http::client`], the shared one, rather than the
//! stranger-proof [`crate::http::public_client`]: every host is fixed in the
//! core, and none is a name a model or a web page chose -- the line `http`'s
//! own module doc draws between the two. What a model chooses is only the
//! words of a place, which go into a query string.
//!
//! # One question a second, to Nominatim
//!
//! OpenStreetMap's geocoder is a volunteer-run service whose usage policy
//! asks for no more than one request a second from any one client, and a
//! single call to `get_directions` asks it two questions back to back --
//! where the trip starts and where it ends -- while `search_places` can ask
//! three. So every Nominatim request in the process waits its turn behind
//! [`NOMINATIM_TURN`], at least [`NOMINATIM_GAP`] after the one before. It
//! costs a second on a directions call, which is a second nobody notices
//! next to the route planner's own, and it is what keeps this application on
//! the right side of a policy whose remedy for the wrong side is a block.
//!
//! The route planner and Transitous are asked once a call and need no such
//! pacing.

use std::time::{Duration, Instant};

use everyday_core::maps::{
    self, Area, DirectionsArgs, GEOCODER, Located, Mode, PlacesArgs, PlacesReport, Point, ROUTER,
    Route, SEARCH_RADII_KM, TRANSIT, Transit, When,
};
use everyday_core::weather::Units;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{CommandError, CommandResult, codes};
use crate::http;

/// The assistant's tool for finding a place, or places of a kind near one.
pub(crate) const SEARCH_PLACES: &str = "search_places";
/// The assistant's tool for getting from one place to another.
pub(crate) const GET_DIRECTIONS: &str = "get_directions";

/// The least time between two requests to Nominatim. A little over the
/// policy's second, because the policy is about when requests *arrive*.
const NOMINATIM_GAP: Duration = Duration::from_millis(1100);

/// When the last Nominatim request finished, held across the next one so
/// that two calls running at once take turns. See the module doc.
static NOMINATIM_TURN: Mutex<Option<Instant>> = Mutex::const_new(None);

/// Places matching `args.query` around `args.near`, nearest first.
///
/// Looks within a walk first, then within a short drive, and last anywhere
/// at all with the near ones preferred -- see
/// [`SEARCH_RADII_KM`](everyday_core::maps::SEARCH_RADII_KM) and
/// [`Area`]. The first of those to find anything is the answer, so a search
/// for a cafe in a city costs one question and a search for a museum named
/// in another country costs three.
pub async fn places(args: &PlacesArgs) -> CommandResult<PlacesReport> {
    let center = locate(&args.near, None).await?;
    let units = Units::customary_in(&center.country_code);
    let areas = SEARCH_RADII_KM.iter().map(|km| Area::Within(*km)).chain([Area::Anywhere]);
    for area in areas {
        let url = maps::places_url(&args.query, center.point, area, args.limit)
            .map_err(CommandError::from)?;
        let body = nominatim(&url, "places").await?;
        let found = maps::parse_places(&body, center.point, units, args.limit)
            .map_err(|e| unreadable(GEOCODER, "places", &e))?;
        if !found.is_empty() {
            return Ok(PlacesReport { near: center.label, count: found.len(), places: found });
        }
    }
    Ok(PlacesReport { near: center.label, count: 0, places: Vec::new() })
}

/// One answer to `get_directions`: a route by road or path, or a choice of
/// trips by public transport. Untagged, because both already say which they
/// are in their own `mode`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Directions {
    Route(Route),
    Transit(Transit),
}

/// How to get from `args.from` to `args.to`.
///
/// The far end is looked up near the start, so "the library" is the one in
/// town. `zone` is the person's, for reading a stop's time when the stop
/// does not say its own; `now` decides which times need their date.
pub async fn directions(
    args: &DirectionsArgs,
    zone: &TimeZone,
    now: Timestamp,
) -> CommandResult<Directions> {
    let from = locate(&args.from, None).await?;
    let to = locate(&args.to, Some(from.point)).await?;
    let units = args.units.unwrap_or_else(|| Units::customary_in(&from.country_code));

    if args.mode == Mode::Transit {
        let url = maps::transit_url(from.point, to.point, args.when);
        let body = get(&url, TRANSIT, "the timetable").await?;
        let trip = maps::parse_transit(&body, args.when, &from, &to, units, zone, now)
            .map_err(|e| unreadable(TRANSIT, "the timetable", &e))?;
        return Ok(Directions::Transit(trip));
    }

    let url = maps::route_url(from.point, to.point, args.mode);
    let body = get(&url, ROUTER, "the route").await?;
    let mut route = maps::parse_route(&body, args.mode, &from, &to, units)
        .map_err(|e| unreadable(ROUTER, "the route", &e))?;
    // The planner knows roads, not today's traffic or tomorrow's: a time
    // asked for changes nothing about its answer, and saying so beats
    // letting "leave at five" read as accounted for.
    if args.when != When::Now && route.note.is_none() {
        route.note = Some(
            "This time is the same whenever they leave: it does not account for traffic or \
             the time of day."
                .into(),
        );
    }
    Ok(Directions::Route(route))
}

/// Where `place` is, by Nominatim, preferring answers around `near`.
async fn locate(place: &str, near: Option<Point>) -> CommandResult<Located> {
    let url = maps::geocode_url(place, near).map_err(CommandError::from)?;
    let body = nominatim(&url, "the place").await?;
    let found = maps::parse_geocode(&body).map_err(|e| unreadable(GEOCODER, "the place", &e))?;
    found.ok_or_else(|| {
        CommandError::new(
            codes::NOT_FOUND,
            format!(
                "{GEOCODER} knows no place called \u{201c}{}\u{201d}. Try a fuller address, or \
                 the place's name with its town.",
                place.trim()
            ),
        )
    })
}

/// GET one Nominatim address, in turn -- see the module doc's "One question
/// a second".
async fn nominatim(url: &str, what: &str) -> CommandResult<String> {
    let mut last = NOMINATIM_TURN.lock().await;
    if let Some(at) = *last {
        let since = at.elapsed();
        if since < NOMINATIM_GAP {
            tokio::time::sleep(NOMINATIM_GAP - since).await;
        }
    }
    let answer = get(url, GEOCODER, what).await;
    *last = Some(Instant::now());
    answer
}

/// GET one of the three hosts and hand the body back as text.
///
/// `who` names the provider and `what` the half of the job this was, for a
/// failure's sentence.
async fn get(url: &str, who: &str, what: &str) -> CommandResult<String> {
    let response = http::client()?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| describe(&e, who, what))?;

    let status = response.status();
    if !status.is_success() {
        // All three explain a question they will not answer in their own
        // body -- "Path distance exceeds the max distance limit", "unknown
        // feed id" -- and that sentence is better than any this file could
        // guess at. Read small, and only for that.
        let reason = if status.is_client_error() && status.as_u16() != 429 {
            http::read_capped(response, 16 * 1024, String::new)
                .await
                .ok()
                .and_then(|b| maps::error_reason(&String::from_utf8_lossy(&b)))
        } else {
            None
        };
        // A question refused with a reason is one to ask differently -- a
        // nearer place, another way of travelling -- not a network that is
        // down, and the model is told which.
        let code = if reason.is_some() { codes::INVALID } else { codes::NETWORK };
        return Err(CommandError::new(code, explain_status(status, who, reason)));
    }

    let body = http::read_capped(response, maps::MAX_RESPONSE_BYTES, || {
        format!("{who} sent more than this app will read for {what}")
    })
    .await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn explain_status(status: reqwest::StatusCode, who: &str, reason: Option<String>) -> String {
    match (status.as_u16(), reason) {
        (_, Some(reason)) => format!("{who} could not answer that: {reason}"),
        (401 | 403, _) => format!("{who} refused the request."),
        (404 | 410, _) => format!("{who} had nothing at that address."),
        (429, _) => format!(
            "{who} asked us to slow down -- it is a free service that limits how often it is \
             asked. Try again in a minute."
        ),
        (500..=599, _) => format!("{who} is having trouble ({status})."),
        _ => format!("{who} answered {status}."),
    }
}

fn describe(e: &reqwest::Error, who: &str, what: &str) -> CommandError {
    let message = if e.is_timeout() {
        format!("{who} did not answer in time.")
    } else if e.is_connect() {
        format!("could not reach {who}. Check the network.")
    } else {
        // Without the URL: it carries the places, and one of them is
        // usually where somebody lives.
        format!("looking up {what} failed: {}", http::strip_url(&e.to_string()))
    };
    CommandError::new(codes::NETWORK, message)
}

/// An answer the core could not read: logged in full, said in a sentence.
fn unreadable(who: &str, what: &str, e: &everyday_core::Error) -> CommandError {
    tracing::warn!(provider = who, error = %e, "could not read {what}");
    CommandError::new(
        codes::UNREADABLE,
        format!("{who} answered with {what} in a form this app could not read."),
    )
}

/// A failure as the model is told it: a place nobody has heard of, or a
/// question refused with a reason, is something to ask differently; the rest
/// is the network's.
fn tool_error(e: CommandError) -> ToolExecutionError {
    if e.code == codes::INVALID || e.code == codes::NOT_FOUND {
        ToolExecutionError::invalid_args(e.message)
    } else {
        ToolExecutionError::other(e.message)
    }
}

fn to_output(value: &impl Serialize) -> Result<ToolOutput, ToolExecutionError> {
    let json = serde_json::to_value(value).map_err(|e| ToolExecutionError::other(e.to_string()))?;
    Ok(ToolOutput::json(json))
}

/// `search_places`, as a tool.
///
/// `home` is the profile's location, read once by `agent::build` -- which
/// has already read the profile for the preamble -- as `get_weather`'s is.
pub(crate) fn search_places_tool(home: String) -> PortableDynamicTool {
    PortableDynamicTool::new(
        SEARCH_PLACES,
        "Find places on OpenStreetMap: somewhere by its name or address, or places of a kind \
         near somewhere -- cafes near the office, a pharmacy near home. `near` defaults to \
         where they live. For a kind of place, use OpenStreetMap's own plain word for it, \
         singular: cafe, restaurant, pharmacy, supermarket, bakery, bar, pub, park, library, \
         hospital, dentist, fuel, cinema. \u{201c}cafe\u{201d} finds coffee shops; \
         \u{201c}coffee\u{201d} finds only places with coffee in their names. If nothing comes \
         back, try another word or a nearby town. Results are nearest first, each with how \
         far it is, its address and, where the map has them, opening hours (in \
         OpenStreetMap's notation, \u{201c}Mo-Fr 07:00-18:00\u{201d}), phone, website and \
         cuisine. The map is kept by volunteers: when opening hours matter, say they may be \
         out of date. Each place's `url` is its page on openstreetmap.org.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A kind of place in OpenStreetMap's word for it \
                                    (\u{201c}cafe\u{201d}), or a name or an address.",
                },
                "near": {
                    "type": "string",
                    "description": "A place or address to search around. Leave it out for \
                                    where they live.",
                },
                "limit": {
                    "type": "integer",
                    "description": "How many places, up to 20. Default 8.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let home = home.clone();
            Box::pin(async move {
                let asked = maps::places_args(&arguments, &home)
                    .map_err(ToolExecutionError::invalid_args)?;
                let found = places(&asked).await.map_err(tool_error)?;
                to_output(&found)
            })
        },
    )
}

/// `get_directions`, as a tool.
///
/// `home` is the profile's location; `zone` is the person's own time zone,
/// which a time they give -- "arrive by nine" -- is read in. A zone this
/// build does not know falls back to the machine's.
pub(crate) fn get_directions_tool(home: String, zone: String) -> PortableDynamicTool {
    let zone = TimeZone::get(zone.trim()).unwrap_or_else(|_| TimeZone::system());
    PortableDynamicTool::new(
        GET_DIRECTIONS,
        "How to get from one place to another and how long it takes: driving, walking or \
         cycling, planned on OpenStreetMap, or public transport, from Transitous. It starts \
         from where they live unless `from` says otherwise, and finds both ends by name or \
         address -- a vague name (\u{201c}the library\u{201d}) is matched near the start. \
         Driving times assume clear roads: they do not account for traffic. For public \
         transport, give `depart_at` or `arrive_by` as a local date and time \
         (\u{201c}2026-10-20T09:00\u{201d}, or just \u{201c}09:00\u{201d} for the next \
         nine o'clock) or it plans leaving now; it returns up to four options, each with when it leaves and \
         arrives in the local time where that happens, and every leg -- the line, which way \
         it is headed, where to get on and off. Transitous only knows timetables agencies \
         publish as open data, so no option can mean a gap in its coverage. A route by road \
         comes with `map_url`, the same route on openstreetmap.org, for them to open.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Where they are going: a place's name or an address.",
                },
                "from": {
                    "type": "string",
                    "description": "Where they are starting. Leave it out for where they live.",
                },
                "mode": {
                    "type": "string",
                    "enum": Mode::NAMES,
                    "description": "How they are going. Default drive.",
                },
                "depart_at": {
                    "type": "string",
                    "description": "Public transport: when they want to leave, as a local \
                                    date and time. Default now.",
                },
                "arrive_by": {
                    "type": "string",
                    "description": "Public transport: when they need to be there, as a local \
                                    date and time. Give this or depart_at, not both.",
                },
                "units": {
                    "type": "string",
                    "enum": ["metric", "imperial"],
                    "description": "Default: whichever is customary where they start.",
                },
            },
            "required": ["to"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let home = home.clone();
            let zone = zone.clone();
            Box::pin(async move {
                let now = Timestamp::now();
                let asked = maps::directions_args(&arguments, &home, &zone, now)
                    .map_err(ToolExecutionError::invalid_args)?;
                let found = directions(&asked, &zone, now).await.map_err(tool_error)?;
                to_output(&found)
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_quotes_the_providers_own_reason() {
        let said = explain_status(
            reqwest::StatusCode::BAD_REQUEST,
            ROUTER,
            Some("Path distance exceeds the max distance limit: 1500000 meters".into()),
        );
        assert!(said.contains("max distance limit"), "got {said}");
        let busy = explain_status(reqwest::StatusCode::TOO_MANY_REQUESTS, GEOCODER, None);
        assert!(busy.contains("slow down"), "got {busy}");
        let down = explain_status(reqwest::StatusCode::BAD_GATEWAY, TRANSIT, None);
        assert!(down.contains("having trouble"), "got {down}");
    }

    #[test]
    fn a_place_nobody_has_heard_of_is_the_models_to_ask_again() {
        let missing = tool_error(CommandError::new(codes::NOT_FOUND, "no such place"));
        assert_eq!(missing.message(), "no such place");
        assert_eq!(missing.kind(), ToolExecutionError::invalid_args("x").kind());
        let down = tool_error(CommandError::new(codes::NETWORK, "could not reach"));
        assert_eq!(down.kind(), ToolExecutionError::other("x").kind());
    }

    #[test]
    fn both_kinds_of_answer_say_which_they_are_in_mode() {
        let ends = Located {
            point: Point { latitude: 1.0, longitude: 2.0 },
            label: "Here".into(),
            country_code: "GB".into(),
        };
        let trip = Directions::Transit(Transit {
            mode: Mode::Transit,
            from: ends.label.clone(),
            to: ends.label.clone(),
            options: Vec::new(),
            walking_all_the_way: None,
            note: None,
        });
        let sent = serde_json::to_value(&trip).unwrap();
        assert_eq!(sent["mode"], "transit", "untagged: no wrapper around it");
        assert!(sent.get("Transit").is_none());
    }
}
