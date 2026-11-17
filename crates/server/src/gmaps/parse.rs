//! Parsers for the two Google Takeout shapes this ticket imports.
//!
//! Every function here is a pure `&str` in / typed structs out: no filesystem,
//! no network, no database. That is deliberate — the shapes Google actually
//! ships are messy enough that they need unit tests with fixtures inline, and a
//! parser that needs a database to be exercised never gets those.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use strands_api_types::GmapsFormat;

/// One visit from `Timeline.json`, whichever shape the file used.
#[derive(Debug, Clone, PartialEq)]
pub struct Visit {
    /// Human label for the place. Takeout's `semanticSegments` stopped
    /// carrying business names, so this falls back to the semantic type
    /// (`Cafe`) or, failing that, the place id — see [`parse_timeline`].
    pub name: String,
    pub address: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub place_id: Option<String>,
    pub primary_type: Option<String>,
    pub types: Vec<String>,
    pub occurred_at: DateTime<Utc>,
}

/// One row of a Takeout saved-places export (CSV or JSON).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPlace {
    pub title: String,
    pub note: Option<String>,
    pub comment: Option<String>,
    pub url: Option<String>,
    /// Absent in Takeout's four-column CSV. A row without one cannot be
    /// geocoded and is stored unresolved rather than guessed at.
    pub address: Option<String>,
}

/// A `Timeline.json` upload.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineDocument {
    pub visits: Vec<Visit>,
    /// Entries that carried no timestamp or nothing to name them by. Counted
    /// so an import can report them instead of quietly shrinking.
    pub skipped: usize,
}

/// A saved-places upload. `name` is the list title when the file carries one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPlacesDocument {
    pub name: Option<String>,
    pub places: Vec<SavedPlace>,
    /// Which of the two identical-in-spirit exports this came from, so the
    /// import can report the format it detected.
    pub from_csv: bool,
}

/// What an upload turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Document {
    Timeline(TimelineDocument),
    SavedPlaces(SavedPlacesDocument),
}

impl Document {
    /// Wire name of the format, matching `api_types::GmapsFormat`.
    pub fn format(&self) -> GmapsFormat {
        match self {
            Self::Timeline(_) => GmapsFormat::Timeline,
            Self::SavedPlaces(places) if places.from_csv => GmapsFormat::SavedPlacesCsv,
            Self::SavedPlaces(_) => GmapsFormat::SavedPlacesJson,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("the upload is not valid JSON: {0}")]
    Json(String),
    #[error("the upload is not a Google Maps Takeout file this importer knows")]
    UnknownFormat,
    #[error("the CSV has no Title column: {0}")]
    MissingTitleColumn(String),
    #[error("the upload is empty")]
    Empty,
}

/// Detect and parse an uploaded file by its content.
///
/// Detection is on content, never on the file name: Takeout renames things, and
/// a mislabelled upload should still import.
pub fn parse_upload(input: &str) -> Result<Document, ParseError> {
    let text = input.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err(ParseError::Empty);
    }

    if text.starts_with('{') || text.starts_with('[') {
        return parse_json(text);
    }
    parse_saved_places_csv(text)
}

fn parse_json(text: &str) -> Result<Document, ParseError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| ParseError::Json(err.to_string()))?;

    // A Timeline file is an object carrying visit shapes; saved places are an
    // array, or an object wrapping one under a known key.
    if value.is_object() {
        let object = value.as_object().expect("checked is_object");
        if object.contains_key("semanticSegments") || object.contains_key("locations") {
            return Ok(Document::Timeline(parse_timeline_value(value)?));
        }
        let wrapped = ["items", "places", "savedPlaces"]
            .iter()
            .any(|key| object.contains_key(*key));
        if wrapped || object.contains_key("title") || object.contains_key("name") {
            return Ok(Document::SavedPlaces(parse_saved_places_json_value(value)?));
        }
    }
    if value.is_array() {
        return Ok(Document::SavedPlaces(parse_saved_places_json_value(value)?));
    }
    Err(ParseError::UnknownFormat)
}

/// Parse a `Timeline.json` document.
///
/// Two shapes are in the wild and both are accepted:
///
/// * `semanticSegments[]` (the 2024+ export): `visit.topCandidate` carries the
///   place id, a semantic type and `placeLocation.latLng`.
/// * `locations[]` (the older export): one entry per stop with `name`,
///   `address`, `latitudeE7`/`longitudeE7` and `placeId`.
///
/// A `semanticSegments` visit has no business name in the file. Rather than
/// invent one, the visit is titled by its semantic type (`Cafe`), then by its
/// place id, then by its coordinates — all of which come from the file.
pub fn parse_timeline(input: &str) -> Result<TimelineDocument, ParseError> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|err| ParseError::Json(err.to_string()))?;
    parse_timeline_value(value)
}

fn parse_timeline_value(value: serde_json::Value) -> Result<TimelineDocument, ParseError> {
    let file: TimelineFile =
        serde_json::from_value(value).map_err(|err| ParseError::Json(err.to_string()))?;

    let mut visits = Vec::new();
    let mut skipped = 0usize;

    for segment in file.semantic_segments {
        match segment.into_visit() {
            Some(visit) => visits.push(visit),
            None => skipped += 1,
        }
    }
    for location in file.locations {
        match location.into_visit() {
            Some(visit) => visits.push(visit),
            None => skipped += 1,
        }
    }

    // Old exports are unordered by accident, not by design; restore order so a
    // re-import writes the same note sequence.
    visits.sort_by_key(|visit| visit.occurred_at);
    Ok(TimelineDocument { visits, skipped })
}

#[derive(Debug, Deserialize)]
struct TimelineFile {
    #[serde(default, rename = "semanticSegments")]
    semantic_segments: Vec<SemanticSegment>,
    #[serde(default)]
    locations: Vec<LegacyLocation>,
}

#[derive(Debug, Deserialize)]
struct SemanticSegment {
    #[serde(default, rename = "startTime")]
    start_time: Option<String>,
    #[serde(default)]
    visit: Option<SegmentVisit>,
}

impl SemanticSegment {
    fn into_visit(self) -> Option<Visit> {
        let visit = self.visit?;
        let occurred_at = parse_timestamp(self.start_time.as_deref())?;
        let candidate = visit.top_candidate;

        let place_id = candidate
            .as_ref()
            .and_then(|candidate| candidate.place_id.clone())
            .filter(|id| !id.trim().is_empty());
        let (lat, lng) = candidate
            .as_ref()
            .and_then(|candidate| candidate.place_location.as_ref())
            .and_then(|location| location.lat_lng.as_deref())
            .and_then(parse_lat_lng)
            .map_or((None, None), |(lat, lng)| (Some(lat), Some(lng)));

        let semantic_type = candidate
            .as_ref()
            .and_then(|candidate| candidate.semantic_type.as_deref())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty() && value != "unknown");

        let inline_name = candidate
            .as_ref()
            .and_then(|candidate| candidate.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);

        let name = inline_name
            .or_else(|| semantic_type.as_deref().map(title_case))
            .or_else(|| place_id.clone())
            .or_else(|| match (lat, lng) {
                (Some(lat), Some(lng)) => Some(format!("{lat:.5}, {lng:.5}")),
                _ => None,
            })?;

        Some(Visit {
            name,
            address: None,
            lat,
            lng,
            place_id,
            primary_type: semantic_type.clone(),
            types: semantic_type.into_iter().collect(),
            occurred_at,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SegmentVisit {
    #[serde(default, rename = "topCandidate")]
    top_candidate: Option<TopCandidate>,
}

#[derive(Debug, Deserialize)]
struct TopCandidate {
    #[serde(default, rename = "placeId")]
    place_id: Option<String>,
    #[serde(default, rename = "semanticType")]
    semantic_type: Option<String>,
    #[serde(default, rename = "placeLocation")]
    place_location: Option<PlaceLocation>,
    /// Not in Google's own output, but tolerated: some exports and most
    /// fixtures carry a display name here.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaceLocation {
    #[serde(default, rename = "latLng")]
    lat_lng: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyLocation {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "latitudeE7")]
    latitude_e7: Option<i64>,
    #[serde(default, rename = "longitudeE7")]
    longitude_e7: Option<i64>,
    #[serde(default, rename = "placeId")]
    place_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default, rename = "semanticType")]
    semantic_type: Option<String>,
}

impl LegacyLocation {
    fn into_visit(self) -> Option<Visit> {
        let occurred_at = parse_timestamp(self.timestamp.as_deref())?;
        let name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)?;

        let lat = self.latitude_e7.map(|value| value as f64 / 1e7);
        let lng = self.longitude_e7.map(|value| value as f64 / 1e7);
        let primary_type = self
            .semantic_type
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty() && value != "unknown");

        Some(Visit {
            name,
            address: clean_optional(self.address),
            lat,
            lng,
            place_id: clean_optional(self.place_id),
            primary_type: primary_type.clone(),
            types: primary_type.into_iter().collect(),
            occurred_at,
        })
    }
}

/// Parse a Takeout saved-places CSV.
///
/// Google ships `Title,Note,URL,Comment`; an `Address` column appears in some
/// exports and is honoured when present (that is the only thing a row can be
/// geocoded from). Blank rows — Takeout pads its CSVs with them — are dropped
/// here rather than turned into nameless places.
pub fn parse_saved_places_csv(input: &str) -> Result<Document, ParseError> {
    let text = input.trim_start_matches('\u{feff}');
    let rows = tokenise_csv(text);
    let mut rows = rows.into_iter().filter(|row| !is_blank_row(row));

    let header = rows.next().ok_or(ParseError::Empty)?;
    let columns = Columns::from_header(&header)?;

    let mut places = Vec::new();
    for row in rows {
        let title = columns
            .get(&row, columns.title)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        if title.is_empty() {
            continue;
        }
        places.push(SavedPlace {
            title,
            note: clean_optional(columns.get(&row, columns.note).map(str::to_owned)),
            comment: clean_optional(columns.get(&row, columns.comment).map(str::to_owned)),
            url: clean_optional(columns.get(&row, columns.url).map(str::to_owned)),
            address: clean_optional(columns.get(&row, columns.address).map(str::to_owned)),
        });
    }

    Ok(Document::SavedPlaces(SavedPlacesDocument {
        name: None,
        places,
        from_csv: true,
    }))
}

/// Parse the JSON flavour of the same export: a bare array of place objects,
/// or an object wrapping one under `items`/`places`/`savedPlaces`.
pub fn parse_saved_places_json(input: &str) -> Result<SavedPlacesDocument, ParseError> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|err| ParseError::Json(err.to_string()))?;
    parse_saved_places_json_value(value)
}

fn parse_saved_places_json_value(
    value: serde_json::Value,
) -> Result<SavedPlacesDocument, ParseError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Bare(Vec<SavedPlaceJson>),
        Wrapped(SavedPlacesWrapper),
    }

    let (name, rows) = match serde_json::from_value::<Shape>(value)
        .map_err(|err| ParseError::Json(err.to_string()))?
    {
        Shape::Bare(rows) => (None, rows),
        Shape::Wrapped(wrapper) => (clean_optional(wrapper.name.or(wrapper.title)), {
            let mut rows = wrapper.items;
            rows.extend(wrapper.places);
            rows.extend(wrapper.saved_places);
            rows
        }),
    };

    let places = rows
        .into_iter()
        .filter_map(SavedPlaceJson::into_place)
        .collect();

    Ok(SavedPlacesDocument {
        name,
        places,
        from_csv: false,
    })
}

#[derive(Debug, Deserialize)]
struct SavedPlacesWrapper {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    items: Vec<SavedPlaceJson>,
    #[serde(default)]
    places: Vec<SavedPlaceJson>,
    #[serde(default, rename = "savedPlaces")]
    saved_places: Vec<SavedPlaceJson>,
}

#[derive(Debug, Deserialize)]
struct SavedPlaceJson {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    address: Option<String>,
}

impl SavedPlaceJson {
    fn into_place(self) -> Option<SavedPlace> {
        let title = clean_optional(self.title.or(self.name))?;
        Some(SavedPlace {
            title,
            note: clean_optional(self.note),
            comment: clean_optional(self.comment),
            url: clean_optional(self.url),
            address: clean_optional(self.address),
        })
    }
}

/// Indices of the columns this importer understands, resolved from the header
/// row by name so column order in the file does not matter.
struct Columns {
    title: Option<usize>,
    note: Option<usize>,
    url: Option<usize>,
    comment: Option<usize>,
    address: Option<usize>,
}

impl Columns {
    fn from_header(header: &[String]) -> Result<Self, ParseError> {
        let find = |wanted: &[&str]| {
            header.iter().position(|cell| {
                let cell = cell.trim().to_ascii_lowercase();
                wanted.contains(&cell.as_str())
            })
        };
        let title = find(&["title", "name"]);
        // Every Takeout CSV has a Title column; a file without one (a stations
        // list, a random spreadsheet) is not a saved-places export.
        let title = title.ok_or_else(|| {
            ParseError::MissingTitleColumn(
                header
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
            )
        })?;
        Ok(Self {
            title: Some(title),
            note: find(&["note", "notes"]),
            url: find(&["url", "link"]),
            comment: find(&["comment", "comments"]),
            address: find(&["address", "location"]),
        })
    }

    fn get<'a>(&self, row: &'a [String], index: Option<usize>) -> Option<&'a str> {
        index.and_then(|index| row.get(index)).map(String::as_str)
    }
}

/// Split CSV text into cells, honouring quotes, doubled quotes and CRLF.
fn tokenise_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' if cell.is_empty() => in_quotes = true,
            ',' if !in_quotes => row.push(std::mem::take(&mut cell)),
            '\r' if !in_quotes => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            _ => cell.push(character),
        }
    }

    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

fn is_blank_row(row: &[String]) -> bool {
    row.iter().all(|cell| cell.trim().is_empty())
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?.trim();
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// `"35.0050°, 135.7640°"` (and the plain comma form) into degrees.
fn parse_lat_lng(value: &str) -> Option<(f64, f64)> {
    let cleaned = value.replace(['°', '\u{a0}'], " ");
    let mut parts = cleaned.split(',');
    let lat = parts.next()?.trim().parse::<f64>().ok()?;
    let lng = parts.next()?.trim().parse::<f64>().ok()?;
    if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) {
        Some((lat, lng))
    } else {
        None
    }
}

pub(super) fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline_semantic(name: Option<&str>, semantic_type: Option<&str>) -> String {
        let name = name.map_or(String::new(), |name| format!(r#","name":"{name}""#));
        let semantic_type = semantic_type.map_or(String::new(), |value| {
            format!(r#","semanticType":"{value}""#)
        });
        format!(
            r#"{{"semanticSegments":[
                {{"startTime":"2024-03-01T10:00:00Z","visit":{{"topCandidate":{{
                    "placeId":"ChIJabc"{semantic_type}{name},
                    "placeLocation":{{"latLng":"35.0050°, 135.7640°"}}
                }}}}}}
            ]}}"#
        )
    }

    #[test]
    fn semantic_segments_visit_is_parsed_with_coordinates() {
        let document = parse_upload(&timeline_semantic(Some("Nishiki Market"), Some("MARKET")))
            .expect("timeline parses");
        let Document::Timeline(timeline) = document else {
            panic!("expected a timeline");
        };
        assert_eq!(timeline.skipped, 0);
        assert_eq!(timeline.visits.len(), 1);
        let visit = &timeline.visits[0];
        assert_eq!(visit.name, "Nishiki Market");
        assert_eq!(visit.place_id.as_deref(), Some("ChIJabc"));
        assert_eq!(visit.primary_type.as_deref(), Some("market"));
        assert_eq!(visit.lat, Some(35.005));
        assert_eq!(visit.lng, Some(135.764));
        assert_eq!(
            visit.occurred_at.to_rfc3339(),
            "2024-03-01T10:00:00+00:00".to_owned()
        );
    }

    #[test]
    fn a_nameless_visit_is_titled_by_its_semantic_type_never_a_made_up_business_name() {
        let document = parse_upload(&timeline_semantic(None, Some("CAFE"))).expect("parses");
        let Document::Timeline(timeline) = document else {
            panic!("expected a timeline");
        };
        assert_eq!(timeline.visits[0].name, "Cafe");
    }

    #[test]
    fn the_older_locations_array_shape_is_parsed() {
        let json = r#"{"locations":[
            {"timestamp":"2019-07-04T18:30:00.000Z","latitudeE7":488581000,
             "longitudeE7":22944000,"placeId":"ChIJlegacy","name":"Café Central",
             "address":"Herrengasse 14, 1010 Wien, Austria","semanticType":"CAFE"},
            {"timestamp":"2019-07-04T20:05:00Z","latitudeE7":488000000,
             "longitudeE7":22950000}
        ]}"#;
        let timeline = parse_timeline(json).expect("legacy timeline parses");
        assert_eq!(
            timeline.visits.len(),
            1,
            "a ping with no name is not a visit"
        );
        assert_eq!(timeline.skipped, 1, "and it is counted, not lost");
        let visit = &timeline.visits[0];
        assert_eq!(visit.name, "Café Central");
        assert_eq!(visit.lat, Some(48.8581));
        assert_eq!(visit.lng, Some(2.2944));
        assert_eq!(
            visit.address.as_deref(),
            Some("Herrengasse 14, 1010 Wien, Austria")
        );
    }

    #[test]
    fn saved_places_csv_skips_blank_rows_and_unquotes_commas() {
        let csv = "\u{feff}Title,Note,URL,Comment,Address\n\
                   \"Katz's, Delicatessen\",Pastrami,https://maps.app.goo.gl/x,\"worth it\",205 E Houston St\n\
                   ,,,,\n\
                   \n\
                   Blue Bottle,,https://maps.app.goo.gl/y,,\n";
        let document = parse_saved_places_csv(csv).expect("csv parses");
        let Document::SavedPlaces(saved) = document else {
            panic!("expected saved places");
        };
        assert!(saved.from_csv);
        assert_eq!(saved.places.len(), 2, "blank rows are not places");
        assert_eq!(saved.places[0].title, "Katz's, Delicatessen");
        assert_eq!(saved.places[0].note.as_deref(), Some("Pastrami"));
        assert_eq!(saved.places[0].comment.as_deref(), Some("worth it"));
        assert_eq!(saved.places[0].address.as_deref(), Some("205 E Houston St"));
        assert_eq!(saved.places[1].address, None);
    }

    #[test]
    fn the_four_column_takeout_header_parses_without_an_address_column() {
        let csv = "Title,Note,URL,Comment\nKyoto Station,,https://maps.app.goo.gl/z,\n";
        let document = parse_upload(csv).expect("csv parses");
        let Document::SavedPlaces(saved) = document else {
            panic!("expected saved places");
        };
        assert_eq!(saved.places.len(), 1);
        assert_eq!(saved.places[0].title, "Kyoto Station");
        assert_eq!(saved.places[0].address, None);
    }

    #[test]
    fn a_csv_without_a_title_column_is_rejected_rather_than_guessed() {
        let csv = "Stop,Arrival\nKyoto,10:00\n";
        let error = parse_upload(csv).expect_err("no title column");
        assert!(matches!(error, ParseError::MissingTitleColumn(_)));
    }

    #[test]
    fn the_takeout_json_list_variant_is_parsed() {
        let json = r#"[{"title":"Nishiki Market","note":"go early","url":"https://maps.app.goo.gl/a"},
                       {"name":"Fushimi Inari","address":"68 Fukakusa Yabunouchicho, Kyoto"}]"#;
        let saved = parse_saved_places_json(json).expect("json list parses");
        assert!(!saved.from_csv);
        assert_eq!(saved.places.len(), 2);
        assert_eq!(saved.places[1].title, "Fushimi Inari");
        assert_eq!(
            saved.places[1].address.as_deref(),
            Some("68 Fukakusa Yabunouchicho, Kyoto")
        );
    }

    #[test]
    fn a_wrapped_json_list_keeps_its_name() {
        let json = r#"{"name":"Favorites","items":[{"title":"Blue Bottle"}]}"#;
        let document = parse_upload(json).expect("wrapped list parses");
        assert_eq!(document.format(), GmapsFormat::SavedPlacesJson);
        let Document::SavedPlaces(saved) = document else {
            panic!("expected saved places");
        };
        assert_eq!(saved.name.as_deref(), Some("Favorites"));
        assert_eq!(saved.places.len(), 1);
    }

    #[test]
    fn an_unrecognised_upload_is_an_error_not_an_empty_import() {
        let error = parse_upload(r#"{"some":"other export"}"#).expect_err("unknown shape");
        assert!(matches!(error, ParseError::UnknownFormat));
        assert!(matches!(
            parse_upload("   ").expect_err("blank"),
            ParseError::Empty
        ));
    }

    #[test]
    fn malformed_coordinates_are_dropped_rather_than_clamped() {
        assert_eq!(parse_lat_lng("35.0°, 135.0°"), Some((35.0, 135.0)));
        assert_eq!(parse_lat_lng("999.0, 0.0"), None);
        assert_eq!(parse_lat_lng("garbage"), None);
    }
}
