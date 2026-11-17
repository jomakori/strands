//! KML export of resolved places.
//!
//! Rendering is a pure function over the rows: no database, no clock, so the
//! well-formedness test is a unit test. Google Earth is unforgiving about
//! unescaped `&`/`<`, and place names ("Katz's Deli & Bar") are user text, so
//! every value goes through [`escape`].

/// One mapped point. Only rows with real coordinates get here — an unresolved
/// place has nothing to plot and is counted separately.
#[derive(Debug, Clone, PartialEq)]
pub struct KmlPlace {
    pub name: String,
    pub address: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub place_id: Option<String>,
    pub primary_type: Option<String>,
    pub notes: Option<String>,
}

/// Render a KML document with one `Placemark` per place.
///
/// `unresolved` is carried into the document's description so a download that
/// silently lacks a saved place explains itself inside Google Earth instead of
/// looking complete.
pub fn render(places: &[KmlPlace], unresolved: usize) -> String {
    let mut kml = String::with_capacity(256 + places.len() * 320);
    kml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    kml.push_str("<kml xmlns=\"http://www.opengis.net/kml/2.2\">\n");
    kml.push_str("  <Document>\n");
    kml.push_str("    <name>Strands places</name>\n");
    kml.push_str(&format!(
        "    <description>{}</description>\n",
        escape(&summary(places.len(), unresolved))
    ));
    kml.push_str("    <Style id=\"strands-place\">\n");
    kml.push_str("      <IconStyle><scale>0.9</scale></IconStyle>\n");
    kml.push_str("    </Style>\n");

    for place in places {
        kml.push_str("    <Placemark>\n");
        kml.push_str(&format!("      <name>{}</name>\n", escape(&place.name)));
        if let Some(address) = place.address.as_deref().filter(|value| !value.is_empty()) {
            kml.push_str(&format!("      <address>{}</address>\n", escape(address)));
        }
        if let Some(notes) = place.notes.as_deref().filter(|value| !value.is_empty()) {
            kml.push_str(&format!(
                "      <description>{}</description>\n",
                escape(notes)
            ));
        }
        kml.push_str("      <styleUrl>#strands-place</styleUrl>\n");
        kml.push_str(&format!(
            "      <Point><coordinates>{:.6},{:.6},0</coordinates></Point>\n",
            place.lng, place.lat
        ));
        kml.push_str("      <ExtendedData>\n");
        if let Some(place_id) = place.place_id.as_deref().filter(|value| !value.is_empty()) {
            kml.push_str(&data("google_place_id", place_id));
        }
        if let Some(primary_type) = place
            .primary_type
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            kml.push_str(&data("primary_type", primary_type));
        }
        kml.push_str("      </ExtendedData>\n");
        kml.push_str("    </Placemark>\n");
    }

    kml.push_str("  </Document>\n");
    kml.push_str("</kml>\n");
    kml
}

fn summary(placed: usize, unresolved: usize) -> String {
    let mut text = format!("{placed} place{} with coordinates", plural(placed));
    if unresolved > 0 {
        text.push_str(&format!(
            "; {unresolved} place{} without coordinates excluded",
            plural(unresolved)
        ));
    }
    text
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn data(name: &str, value: &str) -> String {
    format!(
        "        <Data name=\"{}\"><value>{}</value></Data>\n",
        escape(name),
        escape(value)
    )
}

/// Escape text for XML character data and attribute values.
pub fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            // XML 1.0 forbids most control characters outright; a stray one in
            // a note would make the whole document unparseable.
            character if character.is_control() && character != '\n' && character != '\t' => {}
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::Reader;
    use quick_xml::events::Event;

    fn place(name: &str) -> KmlPlace {
        KmlPlace {
            name: name.to_owned(),
            address: Some("1 Ferry Building, San Francisco".to_owned()),
            lat: 37.7955,
            lng: -122.3937,
            place_id: Some("ChIJplace".to_owned()),
            primary_type: Some("cafe".to_owned()),
            notes: Some("go early".to_owned()),
        }
    }

    /// Elements of interest, in document order.
    fn elements(xml: &str) -> Vec<String> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut names = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(start)) => {
                    names.push(String::from_utf8_lossy(start.name().as_ref()).into_owned())
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => panic!("KML is not well-formed XML: {error}"),
            }
        }
        names
    }

    #[test]
    fn one_placemark_per_place_inside_a_document() {
        let places = [place("Blue Bottle"), place("Katz's Delicatessen")];
        let xml = render(&places, 0);
        let names = elements(&xml);
        assert_eq!(names.first().map(String::as_str), Some("kml"));
        assert_eq!(names.iter().filter(|name| *name == "Document").count(), 1);
        assert_eq!(names.iter().filter(|name| *name == "Placemark").count(), 2);
    }

    #[test]
    fn user_text_is_escaped_so_the_document_stays_parseable() {
        let xml = render(&[place("Katz's Deli & \"Bar\" <best>")], 0);
        assert!(xml.contains("Katz&apos;s Deli &amp; &quot;Bar&quot; &lt;best&gt;"));
        assert_eq!(
            elements(&xml)
                .iter()
                .filter(|name| *name == "Placemark")
                .count(),
            1
        );
    }

    #[test]
    fn coordinates_are_lng_lat_in_kml_order() {
        let xml = render(&[place("Blue Bottle")], 0);
        assert!(
            xml.contains("<coordinates>-122.393700,37.795500,0</coordinates>"),
            "{xml}"
        );
    }

    #[test]
    fn unresolved_places_are_counted_in_the_document_not_silently_missing() {
        let xml = render(&[place("Blue Bottle")], 3);
        assert!(xml.contains("3 places without coordinates excluded"));
        assert_eq!(
            elements(&xml)
                .iter()
                .filter(|name| *name == "Placemark")
                .count(),
            1
        );
    }

    #[test]
    fn an_empty_export_is_still_a_valid_document() {
        let xml = render(&[], 0);
        let names = elements(&xml);
        assert!(names.iter().any(|name| name == "Document"));
        assert!(!names.iter().any(|name| name == "Placemark"));
    }

    #[test]
    fn control_characters_are_dropped_rather_than_breaking_the_document() {
        assert_eq!(escape("a\u{0}b"), "ab");
        assert_eq!(escape("a\nb"), "a\nb");
    }
}
