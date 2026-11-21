//! QR rendering for the pairing code (T28).
//!
//! `whatsapp-rust` hands the session a pairing payload string, but the only form
//! a phone can act on is a QR image: a settings row that prints the payload as
//! text is a dead end for the user. A QR code is a grid of squares, so the SVG
//! is drawn here and nowhere else — no image crate, and the browser sizes it
//! with CSS like any other element.
//!
//! The payload is *not* logged or stored: it is a live pairing secret, and it
//! only ever travels to the client that is signed in as the linking user.

use qrcodegen::{QrCode, QrCodeEcc};

/// Quiet zone in modules: the spec's minimum, and what a scanner expects when
/// the code sits near a panel edge.
const BORDER: i32 = 4;

/// One pairing payload as an SVG document.
///
/// `None` when the payload does not fit a QR code (a pairing code never does
/// not); returning `None` instead of panicking keeps a broken bridge from taking
/// the settings screen down with it.
pub fn svg(payload: &str) -> Option<String> {
    let code = QrCode::encode_text(payload, QrCodeEcc::Medium).ok()?;
    let size = code.size() + BORDER * 2;

    // One path, one rectangle per run of dark modules: far smaller than a rect
    // per module and it renders crisply at any size.
    let mut path = String::new();
    for y in 0..code.size() {
        let mut x = 0;
        while x < code.size() {
            if !code.get_module(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < code.size() && code.get_module(x, y) {
                x += 1;
            }
            path.push_str(&format!(
                "M{} {}h{}v1h-{}z",
                start + BORDER,
                y + BORDER,
                x - start,
                x - start
            ));
        }
    }

    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" \
         shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"WhatsApp pairing QR code\">\
         <rect width=\"{size}\" height=\"{size}\" fill=\"#ffffff\"/>\
         <path d=\"{path}\" fill=\"#111111\"/></svg>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_payload_becomes_an_svg() {
        let svg = svg("2@AbCdEfGhIjKlMnOpQrStUvWxYz+/=,1").expect("encodes");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("<path"));
        // A QR of that payload is more than four modules wide, so the path has
        // content beyond the quiet zone.
        assert!(svg.contains("h1"));
    }

    #[test]
    fn different_payloads_differ() {
        let first = svg("first-payload").expect("encodes");
        let second = svg("second-payload").expect("encodes");
        assert_ne!(first, second);
    }

    #[test]
    fn an_unencodable_payload_is_none_not_a_panic() {
        assert!(svg(&"x".repeat(10_000)).is_none());
    }
}
