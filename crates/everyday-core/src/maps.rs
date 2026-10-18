//! Places, and the ways between them, except the socket.
//!
//! The assistant's `search_places` and `get_directions` tools ask three
//! questions -- where is this, what is near it, and how do I get there -- and
//! this module is everything about them that does not need a network: the
//! addresses they are asked at, what the answers mean, how far 0.349 miles is
//! when somebody reads it aloud, and which of eight buses leaving after nine
//! are worth mentioning. `everyday_service::maps` opens the connections and
//! hands the bytes back here. It is the split [`crate::weather`] and
//! [`crate::websearch`] make, for their reason: "the bus said it left at four
//! in the afternoon to somebody in Seattle" is a test in this file rather
//! than a network capture.
//!
//! # Why these providers
//!
//! Every one of them answers without a key or an account, the rule each
//! outward lookup in this application keeps -- see [`crate::websearch`]'s own
//! "Which sources, and why these". The obvious names are missing for the
//! reason they are missing there: Google's and Mapbox's are a billing account
//! away, and a directions feature that is off until somebody pastes a key into
//! a settings box should not have shipped.
//!
//! - **Nominatim**, OpenStreetMap's own geocoder, finds a place from its name
//!   or address and searches for places of a kind near a point. Its usage
//!   policy asks for no more than one request a second and an honest user
//!   agent; the service keeps to the first across the whole process (one
//!   directions call looks up both its ends), and its shared client sends the
//!   second.
//! - **Valhalla**, the route planner FOSSGIS runs for openstreetmap.org's own
//!   Directions panel, plans driving, walking and cycling routes and writes
//!   each turn out in English, so this file never has to compose "turn left
//!   onto" from a bearing. It is offered on fair-use terms: personal use, one
//!   route at a time, a user agent that says who is asking.
//! - **Transitous**, a community-run journey planner built on the timetables
//!   transport agencies publish as open data, plans public transport. Its
//!   coverage is wherever those timetables exist, which is most of Europe and
//!   North America and patchy elsewhere -- so "no route" from it can mean a
//!   gap in what it knows rather than no bus, and [`parse_transit`] says so.
//!   It too asks for fair use and an identifying user agent.
//!
//! Driving times are the planner's estimate for a clear road. None of these
//! knows today's traffic, and the tool's description says so rather than
//! letting a time be read as a promise.
//!
//! # What leaves the machine
//!
//! The words of a place or an address, to Nominatim; then two pairs of
//! coordinates -- rounded to what Nominatim itself answered -- to the route
//! planner or to Transitous, with the time somebody asked to leave or arrive
//! by for a public transport trip. Where somebody lives is sent only when a
//! question starts there, which is the default, and only as the words they
//! typed into Settings → About You. No identifier and no history go anywhere.

use jiff::Timestamp;
use jiff::civil::{DateTime, Time};
use jiff::tz::TimeZone;
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::weather::Units;
use crate::websearch::encode;

/// Largest answer any of the three is trusted to send. A journey plan with
/// its line geometry is a quarter of a megabyte; four is somebody else's
/// problem.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Who finds places, for the sentences a failure turns into.
pub const GEOCODER: &str = "OpenStreetMap";

/// Who plans a route on foot, by bicycle or by car.
pub const ROUTER: &str = "OpenStreetMap's route planner";

/// Who plans a journey by public transport.
pub const TRANSIT: &str = "Transitous";

/// How many places a search answers with when nobody said.
pub const DEFAULT_PLACES: u32 = 8;

/// The most places one search answers with. A list longer than this is not
/// read, it is scrolled past.
pub const MAX_PLACES: u32 = 20;

/// The most turns one route lists. A drive across a country is hundreds of
/// them, and the map link has the rest.
pub const MAX_STEPS: usize = 40;

/// The most public transport options one answer offers. Transitous plans
/// eight; the four that fit somebody's question are the ones read.
pub const MAX_OPTIONS: usize = 4;

/// How far around a point a search for places of a kind looks first, then
/// second, in kilometres. A walk, and then a short drive; past that the
/// search stops being bounded at all and simply prefers what is near -- see
/// [`Area`].
pub const SEARCH_RADII_KM: [f64; 2] = [3.0, 15.0];

/// How far around the start the far end of a route is preferred, in
/// kilometres: "Trader Joe's" is the one in town, not the first of five
/// hundred.
const BIAS_RADIUS_KM: f64 = 25.0;

// ── Points and places ────────────────────────────────────────────────────

/// A latitude and a longitude, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub latitude: f64,
    pub longitude: f64,
}

impl Point {
    /// The great-circle distance to `other`, in kilometres. Haversine: good
    /// to a few metres over any distance a search for a cafe covers, which is
    /// all it is asked to be.
    pub fn distance_km(self, other: Point) -> f64 {
        const EARTH_RADIUS_KM: f64 = 6371.0088;
        let (lat1, lat2) = (self.latitude.to_radians(), other.latitude.to_radians());
        let dlat = lat2 - lat1;
        let dlon = (other.longitude - self.longitude).to_radians();
        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_KM * a.sqrt().min(1.0).asin()
    }

    /// Nominatim's `viewbox` for a square `radius_km` either side of this
    /// point: west, north, east, south.
    fn viewbox(self, radius_km: f64) -> String {
        let dlat = radius_km / 110.574;
        // A degree of longitude shrinks towards the poles; the floor keeps a
        // box near one from spanning the planet.
        let dlon = radius_km / (111.320 * self.latitude.to_radians().cos()).max(1.0);
        format!(
            "{:.5},{:.5},{:.5},{:.5}",
            self.longitude - dlon,
            self.latitude + dlat,
            self.longitude + dlon,
            self.latitude - dlat
        )
    }

    /// `47.620500,-122.349300`: how Transitous takes a place.
    fn pair(self) -> String {
        format!("{:.6},{:.6}", self.latitude, self.longitude)
    }
}

/// Somewhere the geocoder found for what somebody typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    pub point: Point,
    /// What to call it back to them: "Pike Place Market, Seattle".
    pub label: String,
    /// ISO 3166-1 alpha-2, upper case, and empty when the geocoder did not
    /// say. Decides the customary units -- see [`Units::customary_in`].
    pub country_code: String,
}

/// The geocoder's address for `place`: one answer, with its address broken
/// into parts. With `near`, answers around it are preferred but not required,
/// so a chain's name finds the branch in town and a city's still finds the
/// city.
pub fn geocode_url(place: &str, near: Option<Point>) -> Result<String> {
    let place = collapse(place);
    if place.is_empty() {
        return Err(Error::Invalid("no place was given to look up".into()));
    }
    let mut url = format!(
        "https://nominatim.openstreetmap.org/search?q={}&format=jsonv2&limit=1\
         &addressdetails=1&accept-language=en",
        encode(&place)
    );
    if let Some(near) = near {
        url.push_str(&format!("&viewbox={}&bounded=0", near.viewbox(BIAS_RADIUS_KM)));
    }
    Ok(url)
}

/// Read the geocoder's answer to [`geocode_url`]. `None` when it has never
/// heard of the place -- an empty list, not an error.
pub fn parse_geocode(body: &str) -> Result<Option<Located>> {
    Ok(nominatim_rows(body)?.iter().find_map(located))
}

fn located(row: &Value) -> Option<Located> {
    let point = point_of(row)?;
    let address = row.get("address");
    let head = place_name(row);
    let mut label = head.clone();
    if let Some(town) = locality(address, &head) {
        label = if label.is_empty() { town } else { format!("{label}, {town}") };
    }
    let country_code = address
        .and_then(|a| a.get("country_code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    Some(Located { point, label, country_code })
}

/// Where a search for places looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Area {
    /// Only within this many kilometres of the point.
    Within(f64),
    /// Anywhere, nearer first -- the last try, so a place somebody named
    /// that is further away than a short drive is still found.
    Anywhere,
}

/// The address of a search for `query` around `around`.
///
/// Asks for twice the places wanted, up to Nominatim's own ceiling of forty:
/// it ranks by how well a name matches rather than by distance, and the
/// nearest are what [`parse_places`] keeps.
pub fn places_url(query: &str, around: Point, area: Area, limit: u32) -> Result<String> {
    let query = collapse(query);
    if query.is_empty() {
        return Err(Error::Invalid("a search for places needs something to search for".into()));
    }
    let wanted = limit.clamp(1, MAX_PLACES);
    let asked = (wanted * 2).min(40);
    let (radius, bounded) = match area {
        Area::Within(km) => (km, 1),
        Area::Anywhere => (SEARCH_RADII_KM[SEARCH_RADII_KM.len() - 1], 0),
    };
    Ok(format!(
        "https://nominatim.openstreetmap.org/search?q={}&format=jsonv2&limit={asked}\
         &addressdetails=1&extratags=1&accept-language=en&viewbox={}&bounded={bounded}",
        encode(&query),
        around.viewbox(radius)
    ))
}

/// One place a search found, as the assistant is handed it.
///
/// Keys in the model's own snake_case, like every other tool's answer; an
/// empty field is left out rather than sent as `""` for the model to puzzle
/// over.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FoundPlace {
    pub name: String,
    /// What OpenStreetMap says it is: "Cafe", "Fast food", "Pharmacy".
    #[serde(rename = "type", skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// "407 Cedar Street, Seattle".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub address: String,
    /// How far from where the search was centred, in the place's customary
    /// units: "0.2 mi", "460 m".
    pub distance: String,
    pub latitude: f64,
    pub longitude: f64,
    /// In OpenStreetMap's own notation: "Mo-Fr 07:00-18:00; Sa 08:00-14:00".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub opening_hours: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub phone: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub website: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cuisine: String,
    /// The place on openstreetmap.org.
    pub url: String,
    /// The same distance as a number, for sorting. Not sent.
    #[serde(skip)]
    pub distance_km: f64,
}

/// What `search_places` answers with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlacesReport {
    /// Where the search was centred, as [`Located::label`] names it.
    pub near: String,
    pub count: usize,
    pub places: Vec<FoundPlace>,
}

/// Read a search's answer: the `limit` places nearest `around`, nearest
/// first, each once.
pub fn parse_places(
    body: &str,
    around: Point,
    units: Units,
    limit: u32,
) -> Result<Vec<FoundPlace>> {
    let mut out: Vec<FoundPlace> = Vec::new();
    for row in nominatim_rows(body)? {
        let Some(point) = point_of(&row) else { continue };
        let name = place_name(&row);
        if name.is_empty() {
            continue;
        }
        let url = osm_url(&row);
        let address = address_line(&row, &name);
        // The same shop mapped as a point and as its building, or a search
        // that matched it twice: one entry is enough.
        if out
            .iter()
            .any(|p| (!url.is_empty() && p.url == url) || (p.name == name && p.address == address))
        {
            continue;
        }
        let tag = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| {
                    row.pointer(&format!("/extratags/{k}"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                })
                .unwrap_or_default()
                .to_string()
        };
        let distance_km = around.distance_km(point);
        out.push(FoundPlace {
            kind: tidy_type(row.get("type").and_then(Value::as_str).unwrap_or_default()),
            address,
            distance: format_distance(distance_km, units),
            latitude: point.latitude,
            longitude: point.longitude,
            opening_hours: tag(&["opening_hours"]),
            phone: tag(&["phone", "contact:phone"]),
            website: web_address(&tag(&["website", "contact:website", "url"])),
            cuisine: tag(&["cuisine"]).replace(';', ", ").replace('_', " "),
            url,
            name,
            distance_km,
        });
    }
    out.sort_by(|a, b| a.distance_km.total_cmp(&b.distance_km));
    out.truncate(limit.clamp(1, MAX_PLACES) as usize);
    Ok(out)
}

// ── Getting there ────────────────────────────────────────────────────────

/// How somebody is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Drive,
    Walk,
    Cycle,
    Transit,
}

impl Mode {
    /// The spellings the tool's schema offers.
    pub const NAMES: [&'static str; 4] = ["drive", "walk", "cycle", "transit"];

    /// Read a model's spelling of it, which is not always the schema's.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "drive" | "driving" | "car" | "auto" => Some(Self::Drive),
            "walk" | "walking" | "foot" | "pedestrian" => Some(Self::Walk),
            "cycle" | "cycling" | "bike" | "bicycle" | "biking" => Some(Self::Cycle),
            "transit" | "public transport" | "public transit" | "bus" | "train" | "subway" => {
                Some(Self::Transit)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// Valhalla's name for it. Public transport is never asked of Valhalla;
    /// walking is the nearest thing, and the answer to a question nobody
    /// should be asking it.
    fn costing(self) -> &'static str {
        match self {
            Self::Drive => "auto",
            Self::Walk | Self::Transit => "pedestrian",
            Self::Cycle => "bicycle",
        }
    }

    /// The engine openstreetmap.org's Directions panel draws the same route
    /// with. None for public transport, which it does not plan.
    fn osm_engine(self) -> Option<&'static str> {
        match self {
            Self::Drive => Some("fossgis_valhalla_car"),
            Self::Walk => Some("fossgis_valhalla_foot"),
            Self::Cycle => Some("fossgis_valhalla_bicycle"),
            Self::Transit => None,
        }
    }
}

/// A link to the same route on openstreetmap.org, for somebody to open and
/// see on a map -- this application draws none of its own. `None` for public
/// transport, which openstreetmap.org does not plan.
pub fn map_url(from: Point, to: Point, mode: Mode) -> Option<String> {
    let engine = mode.osm_engine()?;
    Some(format!(
        "https://www.openstreetmap.org/directions?engine={engine}&route={:.5}%2C{:.5}%3B{:.5}%2C{:.5}",
        from.latitude, from.longitude, to.latitude, to.longitude
    ))
}

/// The route planner's address for a trip from `from` to `to`.
///
/// Distances are always asked for in kilometres and converted here, so that
/// one function decides how a distance reads in both systems -- see
/// [`format_distance`]. The planner's own instructions carry no distances, so
/// nothing it writes disagrees.
pub fn route_url(from: Point, to: Point, mode: Mode) -> String {
    let request = serde_json::json!({
        "locations": [
            { "lat": from.latitude, "lon": from.longitude },
            { "lat": to.latitude, "lon": to.longitude },
        ],
        "costing": mode.costing(),
        "units": "kilometers",
        "directions_options": { "units": "kilometers", "language": "en-US" },
    });
    format!("https://valhalla1.openstreetmap.de/route?json={}", encode(&request.to_string()))
}

/// A trip by road or path, as `get_directions` hands it over.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Route {
    pub mode: Mode,
    pub from: String,
    pub to: String,
    /// "1.2 mi".
    pub distance: String,
    /// "1 h 5 min".
    pub duration: String,
    pub duration_minutes: u64,
    /// Each turn in the planner's own words, with how far to go before the
    /// next: "Turn right onto Broad Street. (0.3 mi)".
    pub steps: Vec<String>,
    /// Anything the reader should know about the answer: that the list was
    /// cut short, or that a time of day was asked for and does not change it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub map_url: String,
}

/// Read the route planner's answer to [`route_url`].
pub fn parse_route(
    body: &str,
    mode: Mode,
    from: &Located,
    to: &Located,
    units: Units,
) -> Result<Route> {
    let root: Value = serde_json::from_str(body).map_err(|e| {
        Error::Invalid(format!("the route planner's answer could not be read: {e}"))
    })?;
    let Some(trip) = root.get("trip") else {
        return Err(Error::Invalid(
            error_reason(body).unwrap_or_else(|| "the route planner sent no route".into()),
        ));
    };
    let seconds = number(trip.pointer("/summary/time")).unwrap_or_default();
    let km = number(trip.pointer("/summary/length")).unwrap_or_default();

    let mut steps = Vec::new();
    let legs = trip.get("legs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for maneuver in
        legs.iter().filter_map(|l| l.get("maneuvers")).filter_map(Value::as_array).flatten()
    {
        let instruction =
            collapse(maneuver.get("instruction").and_then(Value::as_str).unwrap_or_default());
        if instruction.is_empty() {
            continue;
        }
        let length = number(maneuver.get("length")).unwrap_or_default();
        // 4, 5 and 6 are arriving -- straight ahead, on the right, on the
        // left -- and nothing follows them to be a distance away.
        let arriving = matches!(maneuver.get("type").and_then(Value::as_u64), Some(4..=6));
        if length > 0.0 && !arriving {
            steps.push(format!("{instruction} ({})", format_distance(length, units)));
        } else {
            steps.push(instruction);
        }
    }
    let mut note = None;
    if steps.len() > MAX_STEPS {
        let more = steps.len() - MAX_STEPS;
        steps.truncate(MAX_STEPS);
        note = Some(format!("{more} more steps are not listed; map_url shows the whole route."));
    }
    Ok(Route {
        mode,
        from: from.label.clone(),
        to: to.label.clone(),
        distance: format_distance(km, units),
        duration: format_duration(seconds),
        duration_minutes: minutes_of(seconds),
        steps,
        note,
        map_url: map_url(from.point, to.point, mode).unwrap_or_default(),
    })
}

/// When somebody wants to travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Now,
    DepartAt(Timestamp),
    ArriveBy(Timestamp),
}

/// Transitous's address for a trip from `from` to `to`.
///
/// `detailedTransfers=false` leaves out a description of every footpath
/// between platforms, which is most of the bytes and none of the answer.
pub fn transit_url(from: Point, to: Point, when: When) -> String {
    let mut url = format!(
        "https://api.transitous.org/api/v1/plan?fromPlace={}&toPlace={}\
         &numItineraries={MAX_OPTIONS}&detailedTransfers=false",
        from.pair(),
        to.pair()
    );
    match when {
        When::Now => {}
        When::DepartAt(at) => url.push_str(&format!("&time={}", encode(&at.to_string()))),
        When::ArriveBy(by) => {
            url.push_str(&format!("&time={}&arriveBy=true", encode(&by.to_string())));
        }
    }
    url
}

/// A trip by public transport, as `get_directions` hands it over.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transit {
    /// Always [`Mode::Transit`]: said, so the two kinds of answer can be told
    /// apart by the same key.
    pub mode: Mode,
    pub from: String,
    pub to: String,
    /// Soonest first. Empty when Transitous found nothing, and then `note`
    /// says why that might be.
    pub options: Vec<TransitOption>,
    /// How long walking the whole way takes, when Transitous worked that out
    /// too: sometimes the answer to "which bus" is none of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub walking_all_the_way: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One way of making the trip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransitOption {
    /// Leaving, in the local time where the trip starts: "09:00", or
    /// "Tue 20 Oct, 09:00" when it is not today there.
    pub depart: String,
    /// Arriving, likewise, in the local time where it ends.
    pub arrive: String,
    pub duration: String,
    pub duration_minutes: u64,
    pub transfers: u64,
    /// Each leg as a sentence: "Walk 8 min (560 m) to Wall St & 5th Ave",
    /// "Bus 131 towards Burien, 09:08 from Wall St & 5th Ave to 3rd Ave &
    /// Virginia St (arrive 09:11)".
    pub steps: Vec<String>,
}

/// Read Transitous's answer to [`transit_url`].
///
/// Transitous plans more than are useful. Leaving at a time, the soonest
/// [`MAX_OPTIONS`] are kept; arriving by one, the latest that still make it,
/// because "be there by nine" means "leave as late as I can". `zone` is
/// whose clock to read a time in when a stop does not say its own, and `now`
/// decides which times need their date written beside them.
pub fn parse_transit(
    body: &str,
    when: When,
    from: &Located,
    to: &Located,
    units: Units,
    zone: &TimeZone,
    now: Timestamp,
) -> Result<Transit> {
    let root: Value = serde_json::from_str(body).map_err(|e| {
        Error::Invalid(format!("the journey planner's answer could not be read: {e}"))
    })?;
    let names = Ends { from: &from.label, to: &to.label };

    let mut plans: Vec<(Timestamp, Timestamp, &Value)> = root
        .get("itineraries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|it| Some((timestamp(it.get("startTime"))?, timestamp(it.get("endTime"))?, it)))
        .collect();
    plans.sort_by_key(|(start, _, _)| *start);
    let chosen: Vec<(Timestamp, Timestamp, &Value)> = match when {
        When::ArriveBy(by) => {
            plans.retain(|(_, end, _)| *end <= by);
            plans.sort_by_key(|(_, end, _)| *end);
            let skip = plans.len().saturating_sub(MAX_OPTIONS);
            let mut latest: Vec<_> = plans.into_iter().skip(skip).collect();
            latest.sort_by_key(|(start, _, _)| *start);
            latest
        }
        When::Now | When::DepartAt(_) => plans.into_iter().take(MAX_OPTIONS).collect(),
    };

    let options: Vec<TransitOption> = chosen
        .into_iter()
        .map(|(start, end, plan)| {
            let legs =
                plan.get("legs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            let first_tz =
                legs.first().map_or_else(|| zone.clone(), |l| stop_zone(l.get("from"), zone));
            let last_tz =
                legs.last().map_or_else(|| zone.clone(), |l| stop_zone(l.get("to"), zone));
            let seconds = number(plan.get("duration"))
                .unwrap_or_else(|| (end.as_second() - start.as_second()) as f64);
            TransitOption {
                depart: clock(start, &first_tz, now),
                arrive: clock(end, &last_tz, now),
                duration: format_duration(seconds),
                duration_minutes: minutes_of(seconds),
                transfers: plan.get("transfers").and_then(Value::as_u64).unwrap_or_default(),
                steps: legs
                    .iter()
                    .filter_map(|leg| leg_line(leg, &names, units, zone, now))
                    .collect(),
            }
        })
        .collect();

    // `direct` is the trip with no transport at all. Only a walk is worth
    // offering as an alternative: Transitous can also answer with a drive.
    let walking_all_the_way = root
        .pointer("/direct/0")
        .filter(|d| {
            d.get("legs").and_then(Value::as_array).is_some_and(|legs| {
                !legs.is_empty() && legs.iter().all(|l| text(l, "mode") == "WALK")
            })
        })
        .and_then(|d| number(d.get("duration")))
        .map(format_duration);

    let note = options.is_empty().then(|| {
        "Transitous found no public transport between these places at that time. It only \
         knows the timetables transport agencies publish as open data, so this may be a gap \
         in what it knows rather than no service: say so, and suggest checking the local \
         operator."
            .to_string()
    });
    Ok(Transit {
        mode: Mode::Transit,
        from: from.label.clone(),
        to: to.label.clone(),
        options,
        walking_all_the_way,
        note,
    })
}

/// What a journey's first and last stops are called in its own legs, which
/// Transitous names `START` and `END`.
struct Ends<'a> {
    from: &'a str,
    to: &'a str,
}

fn leg_line(
    leg: &Value,
    ends: &Ends<'_>,
    units: Units,
    zone: &TimeZone,
    now: Timestamp,
) -> Option<String> {
    let mode = text(leg, "mode");
    let from = stop_name(leg.get("from"), ends);
    let to = stop_name(leg.get("to"), ends);
    let seconds = number(leg.get("duration")).unwrap_or_default();
    let verb = match mode.as_str() {
        "WALK" => Some("Walk"),
        "BIKE" | "BIKE_RENTAL" | "SCOOTER_RENTAL" => Some("Cycle"),
        "CAR" | "CAR_PARKING" | "CAR_DROPOFF" | "CAR_RENTAL" => Some("Drive"),
        _ => None,
    };
    if let Some(verb) = verb {
        let distance = number(leg.get("distance"))
            .filter(|m| *m > 0.0)
            .map(|m| format!(" ({})", format_distance(m / 1000.0, units)))
            .unwrap_or_default();
        return Some(format!("{verb} {}{distance} to {to}", format_duration(seconds)));
    }
    if mode.is_empty() {
        return None;
    }

    let mut line = vehicle(&mode);
    let name = ["displayName", "routeShortName", "routeLongName", "tripShortName"]
        .iter()
        .map(|k| text(leg, k))
        .find(|n| !n.is_empty())
        .unwrap_or_default();
    if !name.is_empty() {
        line.push(' ');
        line.push_str(&name);
    }
    let headsign = text(leg, "headsign");
    if !headsign.is_empty() {
        line.push_str(&format!(" towards {headsign}"));
    }
    let leaves = timestamp(leg.get("startTime"))
        .map(|t| format!(", {}", clock(t, &stop_zone(leg.get("from"), zone), now)))
        .unwrap_or_default();
    line.push_str(&format!("{leaves} from {from} to {to}"));
    if let Some(arrives) = timestamp(leg.get("endTime")) {
        line.push_str(&format!(
            " (arrive {})",
            clock(arrives, &stop_zone(leg.get("to"), zone), now)
        ));
    }
    if leg.get("cancelled").and_then(Value::as_bool) == Some(true) {
        line.push_str(" -- cancelled");
    }
    Some(line)
}

/// A stop's name, with Transitous's placeholders for the ends replaced by
/// what the person called them.
fn stop_name(stop: Option<&Value>, ends: &Ends<'_>) -> String {
    let name = stop.map(|s| text(s, "name")).unwrap_or_default();
    match name.as_str() {
        "START" | "" => ends.from.to_string(),
        "END" => ends.to.to_string(),
        _ => name,
    }
}

/// A stop's own time zone, which is the one its timetable is in -- or the
/// person's, when it does not say.
fn stop_zone(stop: Option<&Value>, zone: &TimeZone) -> TimeZone {
    stop.map(|s| text(s, "tz"))
        .filter(|tz| !tz.is_empty())
        .and_then(|tz| TimeZone::get(&tz).ok())
        .unwrap_or_else(|| zone.clone())
}

/// What a vehicle is called, from Transitous's mode.
fn vehicle(mode: &str) -> String {
    let word = match mode {
        "BUS" => "Bus",
        "COACH" => "Coach",
        "TRAM" => "Tram",
        "SUBWAY" | "METRO" => "Metro",
        "RAIL" | "REGIONAL_RAIL" | "REGIONAL_FAST_RAIL" | "HIGHSPEED_RAIL" | "LONG_DISTANCE"
        | "NIGHT_RAIL" | "SUBURBAN" => "Train",
        "FERRY" => "Ferry",
        "AIRPLANE" => "Flight",
        "CABLE_CAR" => "Cable car",
        "FUNICULAR" => "Funicular",
        "AERIAL_LIFT" => "Gondola",
        "ODM" => "On-demand service",
        other => return tidy_type(&other.to_ascii_lowercase()),
    };
    word.to_string()
}

/// A time as somebody would read it where it happens: "09:08", or "Tue 20
/// Oct, 09:08" when that is not today there.
pub fn clock(at: Timestamp, zone: &TimeZone, now: Timestamp) -> String {
    let local = at.to_zoned(zone.clone());
    if local.date() == now.to_zoned(zone.clone()).date() {
        local.strftime("%H:%M").to_string()
    } else {
        local.strftime("%a %-d %b, %H:%M").to_string()
    }
}

/// Read a time somebody gave: a full timestamp with its offset, a local date
/// and time ("2026-10-20T09:00", "2026-10-20 09:00") in `zone`, or a time
/// alone ("09:00"), which is the next one -- see [`NEXT_CLOCK_SLACK_MINUTES`].
/// The error is a sentence for a model.
pub fn parse_when(
    text: &str,
    zone: &TimeZone,
    now: Timestamp,
) -> std::result::Result<Timestamp, String> {
    let text = text.trim();
    let wrong = || format!("`{text}` is not a date and time; write it like 2026-10-20T09:00");
    if let Ok(at) = text.parse::<Timestamp>() {
        return Ok(at);
    }
    let local = if let Ok(datetime) = text.parse::<DateTime>() {
        datetime
    } else if let Ok(time) = text.parse::<Time>() {
        let today = now.to_zoned(zone.clone()).date();
        let at = today.to_datetime(time).to_zoned(zone.clone()).map_err(|_| wrong())?;
        let slack = jiff::SignedDuration::from_mins(NEXT_CLOCK_SLACK_MINUTES);
        if at.timestamp() >= now - slack {
            return Ok(at.timestamp());
        }
        let tomorrow = today.tomorrow().map_err(|_| wrong())?;
        tomorrow.to_datetime(time)
    } else {
        return Err(wrong());
    };
    local.to_zoned(zone.clone()).map(|z| z.timestamp()).map_err(|_| wrong())
}

/// How long ago a time alone may be and still mean today's.
///
/// "09:00" asked at eight in the evening is tomorrow morning: nobody plans a
/// bus they have already missed by eleven hours, and a planner asked for one
/// answers with timetables from the past. Asked at ten past nine it is still
/// this morning's -- somebody running late, whose answer is the next bus.
const NEXT_CLOCK_SLACK_MINUTES: i64 = 60;

// ── The tools' arguments ─────────────────────────────────────────────────

/// `search_places`' arguments, read and defaulted.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacesArgs {
    pub query: String,
    /// Where to search around: what they named, or where they live.
    pub near: String,
    pub limit: u32,
}

/// Read `search_places`' arguments, with the profile's location as the
/// `near` nobody named. The error is a sentence for the model.
pub fn places_args(arguments: &Value, home: &str) -> std::result::Result<PlacesArgs, String> {
    let query = collapse(arguments.get("query").and_then(Value::as_str).unwrap_or_default());
    if query.is_empty() {
        return Err("`query` is required: a kind of place, a name or an address".into());
    }
    let named = collapse(arguments.get("near").and_then(Value::as_str).unwrap_or_default());
    let near = if named.is_empty() { collapse(home) } else { named };
    if near.is_empty() {
        return Err(no_home("No `near` was given", "where to look"));
    }
    let limit = whole(arguments.get("limit")).unwrap_or(DEFAULT_PLACES).clamp(1, MAX_PLACES);
    Ok(PlacesArgs { query, near, limit })
}

/// `get_directions`' arguments, read and defaulted.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectionsArgs {
    pub from: String,
    pub to: String,
    pub mode: Mode,
    pub when: When,
    /// `None` is "customary where the trip starts", decided once that is
    /// found.
    pub units: Option<Units>,
}

/// Read `get_directions`' arguments, with the profile's location as the
/// `from` nobody named. Times are read in `zone`, the person's own. The error
/// is a sentence for the model.
pub fn directions_args(
    arguments: &Value,
    home: &str,
    zone: &TimeZone,
    now: Timestamp,
) -> std::result::Result<DirectionsArgs, String> {
    let to = collapse(arguments.get("to").and_then(Value::as_str).unwrap_or_default());
    if to.is_empty() {
        return Err("`to` is required: where they are going, by name or address".into());
    }
    let named = collapse(arguments.get("from").and_then(Value::as_str).unwrap_or_default());
    let from = if named.is_empty() { collapse(home) } else { named };
    if from.is_empty() {
        return Err(no_home("No `from` was given", "where they are starting from"));
    }

    let mode = match arguments.get("mode") {
        None | Some(Value::Null) => Mode::Drive,
        Some(v) => v
            .as_str()
            .and_then(Mode::parse)
            .ok_or_else(|| format!("`mode` must be one of: {}", Mode::NAMES.join(", ")))?,
    };

    let time_of = |key: &str| -> std::result::Result<Option<Timestamp>, String> {
        match arguments.get(key).and_then(Value::as_str).map(str::trim) {
            None | Some("") => Ok(None),
            Some(text) => {
                parse_when(text, zone, now).map(Some).map_err(|e| format!("`{key}`: {e}"))
            }
        }
    };
    let when = match (time_of("depart_at")?, time_of("arrive_by")?) {
        (Some(_), Some(_)) => return Err("give `depart_at` or `arrive_by`, not both".into()),
        (Some(at), None) => When::DepartAt(at),
        (None, Some(by)) => When::ArriveBy(by),
        (None, None) => When::Now,
    };

    let units = match arguments.get("units") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .and_then(Units::parse)
                .ok_or("`units` must be \"metric\" or \"imperial\"")?,
        ),
    };
    Ok(DirectionsArgs { from, to, mode, when, units })
}

/// What either tool says when it needs a place and has none.
fn no_home(what: &str, ask: &str) -> String {
    format!(
        "{what} and their profile has no location. Ask them {ask}, or suggest adding where \
         they live in Settings \u{2192} About You."
    )
}

// ── Reading it out ───────────────────────────────────────────────────────

/// A distance as somebody would say it.
///
/// Metres to the nearest ten under a kilometre and tenths of one under ten,
/// then whole kilometres; in miles, the same, with feet under a tenth of a
/// mile -- nobody in Ohio walks "0.03 miles".
pub fn format_distance(km: f64, units: Units) -> String {
    let km = if km.is_finite() { km.max(0.0) } else { 0.0 };
    let round_to = |value: f64, step: f64| ((value / step).round() * step).max(step) as u64;
    match units {
        Units::Metric if km < 1.0 => format!("{} m", round_to(km * 1000.0, 10.0)),
        Units::Metric if km < 10.0 => format!("{km:.1} km"),
        Units::Metric => format!("{} km", km.round() as u64),
        Units::Imperial => {
            let miles = km / 1.609_344;
            if miles < 0.1 {
                format!("{} ft", round_to(km * 3280.84, 10.0))
            } else if miles < 10.0 {
                format!("{miles:.1} mi")
            } else {
                format!("{} mi", miles.round() as u64)
            }
        }
    }
}

/// A length of time as somebody would say it: "5 min", "1 h 5 min", "2 h".
pub fn format_duration(seconds: f64) -> String {
    let minutes = minutes_of(seconds);
    if minutes < 60 {
        return format!("{minutes} min");
    }
    match (minutes / 60, minutes % 60) {
        (hours, 0) => format!("{hours} h"),
        (hours, rest) => format!("{hours} h {rest} min"),
    }
}

/// Whole minutes, rounded, and never "0 min" for a trip that takes any time
/// at all.
pub fn minutes_of(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    ((seconds / 60.0).round() as u64).max(1)
}

/// What a provider said was wrong with a question, from its own error body:
/// Valhalla's and Transitous's `{"error": "…"}`, Nominatim's `{"error":
/// {"message": "…"}}`. Better than any sentence this file could guess.
pub fn error_reason(body: &str) -> Option<String> {
    let root: Value = serde_json::from_str(body).ok()?;
    error_of(&root)
}

fn error_of(root: &Value) -> Option<String> {
    let error = root.get("error")?;
    let said =
        error.as_str().or_else(|| error.get("message").and_then(Value::as_str)).map(collapse)?;
    (!said.is_empty()).then_some(said)
}

// ── Nominatim's rows ─────────────────────────────────────────────────────

/// A Nominatim answer as its rows. An object with an `error` is the
/// geocoder refusing the question, said in its own words.
fn nominatim_rows(body: &str) -> Result<Vec<Value>> {
    let root: Value = serde_json::from_str(body)
        .map_err(|e| Error::Invalid(format!("{GEOCODER}'s answer could not be read: {e}")))?;
    match root {
        Value::Array(rows) => Ok(rows),
        other => match error_of(&other) {
            Some(reason) => Err(Error::Invalid(reason)),
            None => Ok(Vec::new()),
        },
    }
}

fn point_of(row: &Value) -> Option<Point> {
    let latitude = number(row.get("lat"))?;
    let longitude = number(row.get("lon"))?;
    ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude))
        .then_some(Point { latitude, longitude })
}

/// The thing's own name, or -- for an address, which has none -- its first
/// line.
fn place_name(row: &Value) -> String {
    let name = text(row, "name");
    if !name.is_empty() {
        return name;
    }
    let street = street_line(row.get("address"));
    if !street.is_empty() {
        return street;
    }
    let display = text(row, "display_name");
    collapse(display.split(',').next().unwrap_or_default())
}

/// "407 Cedar Street": the number and the street, whichever there are.
fn street_line(address: Option<&Value>) -> String {
    let Some(address) = address else { return String::new() };
    let part = |key: &str| text(address, key);
    let street = ["road", "pedestrian", "footway", "path", "square", "place"]
        .iter()
        .map(|k| part(k))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    match (part("house_number"), street) {
        (_, s) if s.is_empty() => String::new(),
        (n, s) if n.is_empty() => s,
        (n, s) => format!("{n} {s}"),
    }
}

/// The town a place is in, for a label: the first of city, town, village and
/// so on that is not the place's own name -- falling back to the state and
/// the country, so that "Seattle" is labelled "Seattle, Washington" rather
/// than "Seattle, Seattle".
fn locality(address: Option<&Value>, head: &str) -> Option<String> {
    let address = address?;
    ["city", "town", "village", "hamlet", "municipality", "state", "country"]
        .iter()
        .map(|k| text(address, k))
        .find(|town| !town.is_empty() && !town.eq_ignore_ascii_case(head))
}

/// A place's address as one line: its street and its town, or what the
/// geocoder's long form says after the name when there is no street.
fn address_line(row: &Value, name: &str) -> String {
    let address = row.get("address");
    let street = street_line(address);
    let town = locality(address, name).unwrap_or_default();
    if !street.is_empty() && street != name {
        return [street, town].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    }
    // "Mr Waffle, 1526, 1st Avenue, West Edge, …": everything after the
    // name, three parts of it.
    let display = text(row, "display_name");
    let rest: Vec<&str> =
        display.split(',').map(str::trim).skip(1).filter(|p| !p.is_empty()).take(3).collect();
    rest.join(", ")
}

/// The place on openstreetmap.org: `https://www.openstreetmap.org/node/123`.
fn osm_url(row: &Value) -> String {
    let kind = text(row, "osm_type");
    let id = row.get("osm_id").and_then(Value::as_u64);
    match (kind.as_str(), id) {
        ("node" | "way" | "relation", Some(id)) => {
            format!("https://www.openstreetmap.org/{kind}/{id}")
        }
        _ => String::new(),
    }
}

/// OpenStreetMap's word for what a place is, as a person would write it:
/// `fast_food` is "Fast food". "yes" means "one of these, of no stated
/// sort", which is no type at all.
fn tidy_type(osm_type: &str) -> String {
    let words = osm_type.trim().replace('_', " ");
    if words.is_empty() || words == "yes" {
        return String::new();
    }
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect::<String>())
        .unwrap_or_default()
}

/// A website tag as an address that can be opened. Mappers write
/// `example.com` as often as `https://example.com`; anything that is neither
/// is left out rather than offered as a link.
fn web_address(tagged: &str) -> String {
    let tagged = tagged.trim();
    let lower = tagged.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        tagged.to_string()
    } else if !tagged.is_empty() && tagged.contains('.') && !tagged.contains(char::is_whitespace) {
        format!("https://{tagged}")
    } else {
        String::new()
    }
}

// ── Small readers ────────────────────────────────────────────────────────

fn text(value: &Value, key: &str) -> String {
    collapse(value.get(key).and_then(Value::as_str).unwrap_or_default())
}

/// A number, whether it arrived as one or -- as Nominatim's coordinates do
/// -- as a string.
fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

/// A whole number a model sent, as a number or a string of one.
fn whole(value: Option<&Value>) -> Option<u32> {
    number(value).filter(|n| *n >= 0.0).map(|n| n.round().min(f64::from(u32::MAX)) as u32)
}

fn timestamp(value: Option<&Value>) -> Option<Timestamp> {
    value?.as_str()?.trim().parse().ok()
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Space Needle, which every fixture below is measured from.
    const NEEDLE: Point = Point { latitude: 47.6205, longitude: -122.3493 };

    fn seattle() -> TimeZone {
        TimeZone::get("America/Los_Angeles").unwrap()
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn place_at(label: &str, point: Point) -> Located {
        Located { point, label: label.into(), country_code: "US".into() }
    }

    /// Percent-decoding, for reading back what [`route_url`] encoded.
    fn unescape(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 3 <= bytes.len()
                && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).unwrap()
    }

    /// Three cafes as Nominatim sent them, out of distance order, the
    /// bookkeeping fields cut.
    const CAFES: &str = r#"[{"osm_type":"node","osm_id":2397493590,"lat":"47.6096516","lon":"-122.3404808","category":"amenity","type":"cafe","addresstype":"amenity","name":"Mr Waffle","display_name":"Mr Waffle, 1526, 1st Avenue, West Edge, Belltown, Seattle, King County, Washington, 98101, United States","address":{"amenity":"Mr Waffle","house_number":"1526","road":"1st Avenue","neighbourhood":"West Edge","suburb":"Belltown","city":"Seattle","county":"King County","state":"Washington","ISO3166-2-lvl4":"US-WA","postcode":"98101","country":"United States","country_code":"us"},"extratags":{"check_date":"2025-10-07"}},
        {"osm_type":"node","osm_id":2327596943,"lat":"47.6179341","lon":"-122.3475939","category":"amenity","type":"cafe","addresstype":"amenity","name":"Tilikum Place Cafe","display_name":"Tilikum Place Cafe, 407, Cedar Street, Belltown, Seattle, King County, Washington, 98121, United States","address":{"amenity":"Tilikum Place Cafe","house_number":"407","road":"Cedar Street","suburb":"Belltown","city":"Seattle","county":"King County","state":"Washington","ISO3166-2-lvl4":"US-WA","postcode":"98121","country":"United States","country_code":"us"},"extratags":{"phone":"+1-206-282-4830","website":"https://www.tilikumplacecafe.com/","opening_hours":"Th-Fr 10:00-14:00,17:00-21:00; Sa 09:00-14:00,17:00-21:00; Su 09:00-14:00"}},
        {"osm_type":"node","osm_id":2327652050,"lat":"47.6167902","lon":"-122.3466978","category":"amenity","type":"cafe","addresstype":"amenity","name":"La Parisienne","display_name":"La Parisienne, 2507, 4th Avenue, Belltown, Seattle, King County, Washington, 98121, United States","address":{"amenity":"La Parisienne","house_number":"2507","road":"4th Avenue","suburb":"Belltown","city":"Seattle","county":"King County","state":"Washington","ISO3166-2-lvl4":"US-WA","postcode":"98121","country":"United States","country_code":"us"},"extratags":{"shop":"bakery","contact:phone":"+1-206-728-1874","cuisine":"french;coffee_shop","website":"laparisienneseattle.com","opening_hours":"Mo-Fr 07:00-18:00; Sa 07:00-17:00; Su,PH off"}},
        {"osm_type":"node","osm_id":2327652050,"lat":"47.6167902","lon":"-122.3466978","type":"cafe","name":"La Parisienne","display_name":"La Parisienne, 2507, 4th Avenue"}]"#;

    #[test]
    fn places_come_back_nearest_first_with_what_osm_knows_about_each() {
        let found = parse_places(CAFES, NEEDLE, Units::Metric, 8).unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Tilikum Place Cafe", "La Parisienne", "Mr Waffle"],
            "nearest first, and the second copy of the same node is dropped"
        );

        let tilikum = &found[0];
        assert_eq!(tilikum.kind, "Cafe");
        assert_eq!(tilikum.address, "407 Cedar Street, Seattle");
        assert_eq!(tilikum.distance, "310 m");
        assert_eq!(tilikum.phone, "+1-206-282-4830");
        assert_eq!(tilikum.website, "https://www.tilikumplacecafe.com/");
        assert!(tilikum.opening_hours.starts_with("Th-Fr 10:00-14:00"));
        assert_eq!(tilikum.url, "https://www.openstreetmap.org/node/2327596943");

        let parisienne = &found[1];
        assert_eq!(parisienne.phone, "+1-206-728-1874", "from contact:phone");
        assert_eq!(
            parisienne.website, "https://laparisienneseattle.com",
            "a bare host made openable"
        );
        assert_eq!(parisienne.cuisine, "french, coffee shop");
        assert_eq!(found[2].distance, "1.4 km");
    }

    #[test]
    fn a_places_distance_is_in_the_units_given_and_empty_fields_are_not_sent() {
        let found = parse_places(CAFES, NEEDLE, Units::Imperial, 2).unwrap();
        assert_eq!(found.len(), 2, "the limit is kept");
        assert_eq!(found[0].distance, "0.2 mi");
        assert_eq!(found[1].distance, "0.3 mi");

        let waffle = parse_places(CAFES, NEEDLE, Units::Imperial, 8).unwrap().remove(2);
        let sent = serde_json::to_value(&waffle).unwrap();
        assert_eq!(sent["type"], "Cafe");
        assert!(sent.get("phone").is_none() && sent.get("opening_hours").is_none(), "{sent}");
        assert!(sent.get("distance_km").is_none(), "the sort key stays here");
    }

    #[test]
    fn a_search_that_found_nothing_is_empty_and_a_refusal_is_an_error() {
        assert!(parse_places("[]", NEEDLE, Units::Metric, 8).unwrap().is_empty());
        let refused = r#"{"error":{"code":400,"message":"Nothing to search for."}}"#;
        let err = parse_places(refused, NEEDLE, Units::Metric, 8).unwrap_err();
        assert!(err.to_string().contains("Nothing to search for."), "{err}");
        assert!(parse_places("<html>", NEEDLE, Units::Metric, 8).is_err());
    }

    #[test]
    fn a_geocoded_place_is_labelled_with_its_town_and_carries_its_country() {
        let body = r#"[{"osm_type":"way","osm_id":363400641,"lat":"47.6093968","lon":"-122.3414102","category":"landuse","type":"retail","name":"Pike Place Market","display_name":"Pike Place Market, 85, Belltown, Seattle, King County, Washington, 98101, United States","address":{"retail":"Pike Place Market","house_number":"85","suburb":"Belltown","city":"Seattle","county":"King County","state":"Washington","postcode":"98101","country":"United States","country_code":"us"}}]"#;
        let found = parse_geocode(body).unwrap().unwrap();
        assert_eq!(found.label, "Pike Place Market, Seattle");
        assert_eq!(found.country_code, "US");
        assert!((found.point.latitude - 47.6093968).abs() < 1e-9);

        // A city is not "Seattle, Seattle".
        let city = r#"[{"lat":"47.6038321","lon":"-122.330062","name":"Seattle","display_name":"Seattle, King County, Washington, United States","address":{"city":"Seattle","county":"King County","state":"Washington","country":"United States","country_code":"us"}}]"#;
        assert_eq!(parse_geocode(city).unwrap().unwrap().label, "Seattle, Washington");

        // An address has no name of its own: its first line stands in.
        let street = r#"[{"lat":"51.5","lon":"-0.12","name":"","display_name":"10, Downing Street, London","address":{"house_number":"10","road":"Downing Street","city":"London","country_code":"gb"}}]"#;
        assert_eq!(parse_geocode(street).unwrap().unwrap().label, "10 Downing Street, London");

        assert_eq!(parse_geocode("[]").unwrap(), None);
    }

    #[test]
    fn the_addresses_carry_the_words_a_box_around_the_point_and_nothing_else() {
        let plain = geocode_url("  Pike   Place Market ", None).unwrap();
        assert!(
            plain
                .starts_with("https://nominatim.openstreetmap.org/search?q=Pike%20Place%20Market&")
        );
        assert!(!plain.contains("viewbox"));
        let biased = geocode_url("Trader Joe's", Some(NEEDLE)).unwrap();
        assert!(biased.contains("&viewbox=") && biased.ends_with("&bounded=0"), "{biased}");
        assert!(geocode_url("   ", None).is_err());

        let near = places_url("cafe", NEEDLE, Area::Within(3.0), 8).unwrap();
        assert!(
            near.contains("q=cafe&") && near.contains("&limit=16&") && near.ends_with("&bounded=1")
        );
        let anywhere = places_url("cafe", NEEDLE, Area::Anywhere, 30).unwrap();
        assert!(
            anywhere.contains(&format!("&limit={}&", MAX_PLACES * 2))
                && anywhere.ends_with("&bounded=0")
        );
    }

    /// A drive from the Space Needle to Pike Place Market, as Valhalla
    /// answered it, the route's shape cut.
    const DRIVE: &str = r#"{"trip":{"legs":[{"maneuvers":[{"type":1,"instruction":"Drive northeast.","time":62.236,"length":0.121,"travel_mode":"drive"},{"type":10,"instruction":"Turn right onto 5th Avenue North.","time":3.866,"length":0.019,"travel_mode":"drive"},{"type":10,"instruction":"Turn right onto Broad Street.","time":48.449,"length":0.46,"travel_mode":"drive"},{"type":15,"instruction":"Turn left onto 2nd Avenue.","time":119.488,"length":1.116,"travel_mode":"drive"},{"type":10,"instruction":"Turn right onto Stewart Street.","time":36.184,"length":0.194,"travel_mode":"drive"},{"type":10,"instruction":"Turn right onto Pike Place.","time":5.554,"length":0.009,"travel_mode":"drive"},{"type":4,"instruction":"You have arrived at your destination.","time":0.0,"length":0.0,"travel_mode":"drive"}],"summary":{"time":275.779,"length":1.919}}],"summary":{"time":275.779,"length":1.919},"status_message":"Found route between points","status":0,"units":"kilometers","language":"en-US"}}"#;

    const MARKET: Point = Point { latitude: 47.6097, longitude: -122.3422 };

    #[test]
    fn a_route_is_its_planners_own_turns_with_how_far_each_goes() {
        let from = place_at("Space Needle, Seattle", NEEDLE);
        let to = place_at("Pike Place Market, Seattle", MARKET);
        let route = parse_route(DRIVE, Mode::Drive, &from, &to, Units::Metric).unwrap();
        assert_eq!(route.distance, "1.9 km");
        assert_eq!(route.duration, "5 min");
        assert_eq!(route.duration_minutes, 5);
        assert_eq!(route.steps[0], "Drive northeast. (120 m)");
        assert_eq!(route.steps[3], "Turn left onto 2nd Avenue. (1.1 km)");
        assert_eq!(
            route.steps.last().unwrap(),
            "You have arrived at your destination.",
            "arriving is not a distance away from anything"
        );
        assert_eq!(route.note, None);
        assert_eq!(
            route.map_url,
            "https://www.openstreetmap.org/directions?engine=fossgis_valhalla_car\
             &route=47.62050%2C-122.34930%3B47.60970%2C-122.34220"
        );

        let imperial = parse_route(DRIVE, Mode::Drive, &from, &to, Units::Imperial).unwrap();
        assert_eq!(imperial.distance, "1.2 mi");
        assert_eq!(imperial.steps[0], "Drive northeast. (400 ft)");

        let sent = serde_json::to_value(&route).unwrap();
        assert_eq!(sent["mode"], "drive");
        assert!(sent.get("note").is_none());
    }

    #[test]
    fn a_long_route_is_cut_short_and_says_so() {
        let maneuver = json!({ "type": 10, "instruction": "Keep going.", "length": 0.5 });
        let body = json!({ "trip": {
            "legs": [{ "maneuvers": vec![maneuver; MAX_STEPS + 5] }],
            "summary": { "time": 36000.0, "length": 900.0 },
        }})
        .to_string();
        let from = place_at("A", NEEDLE);
        let to = place_at("B", MARKET);
        let route = parse_route(&body, Mode::Cycle, &from, &to, Units::Metric).unwrap();
        assert_eq!(route.steps.len(), MAX_STEPS);
        assert!(route.note.as_deref().unwrap().starts_with("5 more steps"));
        assert_eq!(route.duration, "10 h");
        assert_eq!(route.distance, "900 km");
        assert!(route.map_url.contains("fossgis_valhalla_bicycle"));
    }

    #[test]
    fn a_planner_that_could_not_route_says_why_in_its_own_words() {
        let body = r#"{"error_code":154,"error":"Path distance exceeds the max distance limit: 1500000 meters","status_code":400,"status":"Bad Request"}"#;
        assert_eq!(
            error_reason(body).as_deref(),
            Some("Path distance exceeds the max distance limit: 1500000 meters")
        );
        let from = place_at("A", NEEDLE);
        let err = parse_route(body, Mode::Drive, &from, &from, Units::Metric).unwrap_err();
        assert!(err.to_string().contains("max distance limit"), "{err}");
        assert_eq!(
            error_reason(r#"{"error":{"message":"Bad request."}}"#).as_deref(),
            Some("Bad request.")
        );
        assert_eq!(error_reason("not json"), None);
    }

    #[test]
    fn the_route_planner_is_asked_in_kilometres_for_the_right_kind_of_trip() {
        let url = route_url(NEEDLE, MARKET, Mode::Walk);
        assert!(url.starts_with("https://valhalla1.openstreetmap.de/route?json="));
        let asked: Value =
            serde_json::from_str(&unescape(url.split_once("json=").unwrap().1)).unwrap();
        assert_eq!(asked["costing"], "pedestrian");
        assert_eq!(asked["units"], "kilometers");
        assert_eq!(asked["locations"][1]["lat"], 47.6097);
    }

    /// Two ways by bus from the Space Needle to Pike Place Market, and the
    /// walk, as Transitous answered, each leg's line geometry cut.
    const BUSES: &str = r#"{"itineraries":[{"duration":1020,"startTime":"2026-10-20T16:03:00Z","endTime":"2026-10-20T16:20:00Z","transfers":0,"legs":[{"mode":"WALK","from":{"name":"START","lat":47.6205,"lon":-122.3493,"tz":"America/Los_Angeles","departure":"2026-10-20T16:03:00Z"},"to":{"name":"Denny Way & 2nd Ave N","lat":47.61851500000001,"lon":-122.351715,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:09:00Z"},"duration":360,"startTime":"2026-10-20T16:03:00Z","endTime":"2026-10-20T16:09:00Z","realTime":false,"distance":360.0},{"mode":"BUS","from":{"name":"Denny Way & 2nd Ave N","lat":47.61851500000001,"lon":-122.351715,"tz":"America/Los_Angeles","departure":"2026-10-20T16:09:00Z"},"to":{"name":"3rd Ave & Virginia St","lat":47.61244,"lon":-122.34111000000001,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:14:00Z"},"duration":300,"startTime":"2026-10-20T16:09:00Z","endTime":"2026-10-20T16:14:00Z","realTime":false,"headsign":"Downtown Seattle","agencyName":"Metro Transit","routeShortName":"1","routeLongName":"","displayName":"1","cancelled":false},{"mode":"WALK","from":{"name":"3rd Ave & Virginia St","lat":47.61244,"lon":-122.34111000000001,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:14:00Z","departure":"2026-10-20T16:14:00Z"},"to":{"name":"END","lat":47.6097,"lon":-122.3422,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:20:00Z"},"duration":360,"startTime":"2026-10-20T16:14:00Z","endTime":"2026-10-20T16:20:00Z","realTime":false,"distance":416.0}]},
        {"duration":1020,"startTime":"2026-10-20T16:00:00Z","endTime":"2026-10-20T16:17:00Z","transfers":0,"legs":[{"mode":"WALK","from":{"name":"START","lat":47.6205,"lon":-122.3493,"tz":"America/Los_Angeles","departure":"2026-10-20T16:00:00Z"},"to":{"name":"Wall St & 5th Ave","lat":47.617504,"lon":-122.34527,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:08:00Z"},"duration":480,"startTime":"2026-10-20T16:00:00Z","endTime":"2026-10-20T16:08:00Z","realTime":false,"distance":562.0},{"mode":"BUS","from":{"name":"Wall St & 5th Ave","lat":47.617504,"lon":-122.34527,"tz":"America/Los_Angeles","departure":"2026-10-20T16:08:00Z"},"to":{"name":"3rd Ave & Virginia St","lat":47.61244,"lon":-122.34111000000001,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:11:00Z"},"duration":180,"startTime":"2026-10-20T16:08:00Z","endTime":"2026-10-20T16:11:00Z","realTime":false,"headsign":"Burien Transit Center Highland Park","agencyName":"Metro Transit","routeShortName":"131","routeLongName":"","displayName":"131","cancelled":false},{"mode":"WALK","from":{"name":"3rd Ave & Virginia St","lat":47.61244,"lon":-122.34111000000001,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:11:00Z","departure":"2026-10-20T16:11:00Z"},"to":{"name":"END","lat":47.6097,"lon":-122.3422,"tz":"America/Los_Angeles","arrival":"2026-10-20T16:17:00Z"},"duration":360,"startTime":"2026-10-20T16:11:00Z","endTime":"2026-10-20T16:17:00Z","realTime":false,"distance":416.0}]}],
        "direct":[{"duration":1404,"startTime":"2026-10-20T16:00:00Z","endTime":"2026-10-20T16:23:00Z","transfers":0,"legs":[{"mode":"WALK","from":{"name":"START","lat":47.6205,"lon":-122.3493,"departure":"2026-10-20T16:00:00Z"},"to":{"name":"END","lat":47.6097,"lon":-122.3422,"arrival":"2026-10-20T16:23:00Z"},"duration":1404,"startTime":"2026-10-20T16:00:00Z","endTime":"2026-10-20T16:23:00Z","realTime":false,"distance":1662.0}]}]}"#;

    fn bus_ends() -> (Located, Located) {
        (place_at("Space Needle, Seattle", NEEDLE), place_at("Pike Place Market, Seattle", MARKET))
    }

    #[test]
    fn transit_options_are_soonest_first_in_the_stops_own_local_time() {
        let (from, to) = bus_ends();
        // The same morning in Seattle, so no date is written.
        let now = at("2026-10-20T15:00:00Z");
        let trip =
            parse_transit(BUSES, When::Now, &from, &to, Units::Metric, &seattle(), now).unwrap();
        assert_eq!(trip.options.len(), 2);
        let first = &trip.options[0];
        assert_eq!((first.depart.as_str(), first.arrive.as_str()), ("09:00", "09:17"));
        assert_eq!(first.duration, "17 min");
        assert_eq!(first.duration_minutes, 17);
        assert_eq!(first.transfers, 0);
        assert_eq!(
            first.steps,
            [
                "Walk 8 min (560 m) to Wall St & 5th Ave",
                "Bus 131 towards Burien Transit Center Highland Park, 09:08 from Wall St & 5th Ave \
                 to 3rd Ave & Virginia St (arrive 09:11)",
                "Walk 6 min (420 m) to Pike Place Market, Seattle",
            ]
        );
        assert_eq!(trip.walking_all_the_way.as_deref(), Some("23 min"));
        assert_eq!(trip.note, None);
        assert_eq!(serde_json::to_value(&trip).unwrap()["mode"], "transit");
    }

    #[test]
    fn a_time_on_another_day_carries_its_date() {
        let (from, to) = bus_ends();
        let now = at("2026-10-18T15:00:00Z");
        let trip =
            parse_transit(BUSES, When::Now, &from, &to, Units::Imperial, &seattle(), now).unwrap();
        assert_eq!(trip.options[0].depart, "Tue 20 Oct, 09:00");
        assert_eq!(trip.options[0].steps[0], "Walk 8 min (0.3 mi) to Wall St & 5th Ave");
    }

    #[test]
    fn arriving_by_a_time_keeps_the_latest_options_that_make_it() {
        let (from, to) = bus_ends();
        let now = at("2026-10-20T15:00:00Z");
        let by = When::ArriveBy(at("2026-10-20T16:18:00Z"));
        let trip = parse_transit(BUSES, by, &from, &to, Units::Metric, &seattle(), now).unwrap();
        assert_eq!(trip.options.len(), 1, "the one arriving 09:20 is too late");
        assert_eq!(trip.options[0].arrive, "09:17");
    }

    #[test]
    fn no_public_transport_is_said_plainly_rather_than_as_a_failure() {
        let (from, to) = bus_ends();
        let now = at("2026-10-20T15:00:00Z");
        let empty = r#"{"itineraries":[],"direct":[]}"#;
        let trip =
            parse_transit(empty, When::Now, &from, &to, Units::Metric, &seattle(), now).unwrap();
        assert!(trip.options.is_empty());
        assert!(trip.note.as_deref().unwrap().contains("open data"));
        assert_eq!(trip.walking_all_the_way, None);
    }

    #[test]
    fn transitous_is_asked_for_the_time_and_which_end_of_it_matters() {
        let now = transit_url(NEEDLE, MARKET, When::Now);
        assert!(now.starts_with(
            "https://api.transitous.org/api/v1/plan?fromPlace=47.620500,-122.349300\
             &toPlace=47.609700,-122.342200&"
        ));
        assert!(!now.contains("time="));
        let by = transit_url(NEEDLE, MARKET, When::ArriveBy(at("2026-10-20T16:00:00Z")));
        assert!(by.ends_with("&time=2026-10-20T16%3A00%3A00Z&arriveBy=true"), "{by}");
        let leave = transit_url(NEEDLE, MARKET, When::DepartAt(at("2026-10-20T16:00:00Z")));
        assert!(leave.contains("&time=") && !leave.contains("arriveBy"));
    }

    #[test]
    fn a_time_given_is_read_in_their_own_zone() {
        let now = at("2026-10-20T15:00:00Z");
        let zone = seattle();
        assert_eq!(parse_when("2026-10-20T09:00", &zone, now), Ok(at("2026-10-20T16:00:00Z")));
        assert_eq!(parse_when("2026-10-20 09:00", &zone, now), Ok(at("2026-10-20T16:00:00Z")));
        assert_eq!(parse_when("09:30", &zone, now), Ok(at("2026-10-20T16:30:00Z")), "today");
        // It is eight in the morning there. Half an hour ago is still this
        // morning -- somebody running late -- and two hours ago is tomorrow's.
        assert_eq!(parse_when("07:30", &zone, now), Ok(at("2026-10-20T14:30:00Z")));
        assert_eq!(parse_when("06:00", &zone, now), Ok(at("2026-10-21T13:00:00Z")), "tomorrow");
        assert_eq!(parse_when("2026-10-20T09:00:00Z", &zone, now), Ok(at("2026-10-20T09:00:00Z")));
        assert!(parse_when("next tuesday", &zone, now).unwrap_err().contains("2026-10-20T09:00"));
    }

    #[test]
    fn directions_start_at_home_by_car_unless_told_otherwise() {
        let now = at("2026-10-20T15:00:00Z");
        let zone = seattle();
        let asked =
            directions_args(&json!({ "to": " Pike  Place Market " }), "Seattle, WA", &zone, now)
                .unwrap();
        assert_eq!(asked.from, "Seattle, WA");
        assert_eq!(asked.to, "Pike Place Market");
        assert_eq!(asked.mode, Mode::Drive);
        assert_eq!(asked.when, When::Now);
        assert_eq!(asked.units, None);

        let bus = directions_args(
            &json!({ "to": "x", "from": "y", "mode": "bus", "arrive_by": "09:00", "units": "metric" }),
            "",
            &zone,
            now,
        )
        .unwrap();
        assert_eq!(bus.mode, Mode::Transit);
        assert_eq!(bus.when, When::ArriveBy(at("2026-10-20T16:00:00Z")));
        assert_eq!(bus.units, Some(Units::Metric));
    }

    #[test]
    fn directions_arguments_that_cannot_be_used_say_what_to_do_instead() {
        let now = at("2026-10-20T15:00:00Z");
        let zone = seattle();
        let err = |args: Value, home: &str| directions_args(&args, home, &zone, now).unwrap_err();
        assert!(err(json!({}), "Seattle").contains("`to` is required"));
        assert!(err(json!({ "to": "x" }), "  ").contains("Settings \u{2192} About You"));
        assert!(
            err(json!({ "to": "x", "mode": "teleport" }), "home")
                .contains("drive, walk, cycle, transit")
        );
        assert!(
            err(json!({ "to": "x", "depart_at": "09:00", "arrive_by": "10:00" }), "home")
                .contains("not both")
        );
        assert!(err(json!({ "to": "x", "depart_at": "soon" }), "home").starts_with("`depart_at`"));
        assert!(err(json!({ "to": "x", "units": "furlongs" }), "home").contains("metric"));
    }

    #[test]
    fn a_search_for_places_looks_around_home_unless_told_otherwise() {
        let asked = places_args(&json!({ "query": "cafe" }), "Seattle, WA").unwrap();
        assert_eq!(
            asked,
            PlacesArgs { query: "cafe".into(), near: "Seattle, WA".into(), limit: 8 }
        );
        let named =
            places_args(&json!({ "query": "cafe", "near": "Ballard", "limit": 500 }), "x").unwrap();
        assert_eq!((named.near.as_str(), named.limit), ("Ballard", MAX_PLACES));
        assert!(
            places_args(&json!({ "query": " " }), "x").unwrap_err().contains("`query` is required")
        );
        assert!(places_args(&json!({ "query": "cafe" }), "").unwrap_err().contains("About You"));
    }

    #[test]
    fn distances_and_times_read_the_way_people_say_them() {
        assert_eq!(format_distance(0.0123, Units::Metric), "10 m");
        assert_eq!(format_distance(0.456, Units::Metric), "460 m");
        assert_eq!(format_distance(2.44, Units::Metric), "2.4 km");
        assert_eq!(format_distance(123.6, Units::Metric), "124 km");
        assert_eq!(format_distance(0.019, Units::Imperial), "60 ft");
        assert_eq!(format_distance(1.919, Units::Imperial), "1.2 mi");
        assert_eq!(format_distance(100.0, Units::Imperial), "62 mi");
        assert_eq!(format_distance(f64::NAN, Units::Metric), "10 m");

        assert_eq!(format_duration(20.0), "1 min", "never 0 min for a trip that takes any time");
        assert_eq!(format_duration(275.779), "5 min");
        assert_eq!(format_duration(3900.0), "1 h 5 min");
        assert_eq!(format_duration(7200.0), "2 h");
        assert_eq!(minutes_of(0.0), 0);
    }

    #[test]
    fn a_distance_between_two_points_is_measured_over_the_ground() {
        let km = NEEDLE.distance_km(Point { latitude: 47.6179341, longitude: -122.3475939 });
        assert!((km - 0.3127).abs() < 0.001, "{km}");
        assert_eq!(NEEDLE.distance_km(NEEDLE), 0.0);
    }
}
