//! The URL somebody carries from one screen to the other.
//!
//! Everything a client needs to connect, in a string short enough to read
//! aloud and simple enough to be a QR code:
//!
//! ```text
//! everyday://pair?host=100.64.0.12:7397&fp=<sha256 of the cert>&code=<one-time>&name=Vault
//! ```
//!
//! Four fields, and each is there for a reason. `host` is where to dial.
//! `fp` is the certificate to pin, and it is what makes a self-signed
//! certificate *stronger* than a public one here: a client trusts exactly this
//! certificate, so no authority anywhere can issue one it would accept. `code`
//! is a single-use, five-minute capability to become a paired device. `name` is
//! so the connect screen can say which vault before anything is committed.
//!
//! The QR code is a picture of the same string. It exists for the phone shell,
//! where typing a certificate fingerprint is not a thing anybody will do; on a
//! desktop, pasting the URL is the ordinary path.

use std::fmt::Write as _;

/// A pairing invitation.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    pub url: String,
    /// The code on its own, for somebody reading it out over the phone.
    pub code: String,
    pub host: String,
    pub fingerprint: String,
    /// The same URL as a QR code, in SVG.
    ///
    /// Drawn here rather than in the interface so the two cannot disagree about
    /// what was encoded, and served as SVG because the content security policy
    /// already allows a `data:` image and does not allow a canvas to be talked
    /// into anything.
    pub qr_svg: String,
}

pub fn invitation(host: &str, fingerprint: &str, code: &str, vault_name: &str) -> Invitation {
    let mut url = String::from("everyday://pair?host=");
    url.push_str(&encode(host));
    if !fingerprint.is_empty() {
        let _ = write!(url, "&fp={}", encode(fingerprint));
    }
    let _ = write!(url, "&code={}", encode(code));
    let _ = write!(url, "&name={}", encode(vault_name));

    Invitation {
        qr_svg: qr_svg(&url),
        url,
        code: code.to_string(),
        host: host.to_string(),
        fingerprint: fingerprint.to_string(),
    }
}

/// What a pairing URL says, once it has been read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub host: String,
    pub fingerprint: String,
    pub code: String,
    pub name: String,
}

/// Read a pairing URL, or say what is wrong with it.
///
/// Deliberately forgiving about the things a person does when copying: leading
/// and trailing space, a trailing full stop, the whole thing wrapped in angle
/// brackets by a mail client. Deliberately strict about the two that matter --
/// a host and a code -- because a URL missing either cannot pair and the honest
/// answer is to say so at the paste field.
pub fn parse(raw: &str) -> Result<Invite, String> {
    let raw = raw.trim().trim_matches(|c| c == '<' || c == '>').trim_end_matches('.');
    let rest = raw
        .strip_prefix("everyday://pair?")
        .ok_or_else(|| "that is not an Every Day pairing link".to_string())?;

    let mut invite = Invite {
        host: String::new(),
        fingerprint: String::new(),
        code: String::new(),
        name: String::new(),
    };
    for pair in rest.split('&') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        let value = decode(value);
        match key {
            "host" => invite.host = value,
            "fp" => invite.fingerprint = value,
            "code" => invite.code = value,
            "name" => invite.name = value,
            _ => {}
        }
    }
    if invite.host.is_empty() {
        return Err("that link does not say which computer to connect to".into());
    }
    if invite.code.is_empty() {
        return Err("that link has no pairing code in it".into());
    }
    Ok(invite)
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The URL as an SVG QR code.
///
/// A quiet zone of four modules, because a code drawn flush to the edge of a
/// dark panel is one a camera cannot find. Black on white regardless of theme,
/// for the same reason: a scanner wants contrast in the direction it expects.
fn qr_svg(url: &str) -> String {
    use qrcode::{EcLevel, QrCode};
    let Ok(code) = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M) else {
        return String::new();
    };
    let width = code.width();
    let quiet = 4;
    let side = width + quiet * 2;

    let mut squares = String::new();
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == qrcode::Color::Dark {
                let _ = write!(
                    squares,
                    r#"<rect x="{}" y="{}" width="1" height="1"/>"#,
                    x + quiet,
                    y + quiet
                );
            }
        }
    }
    // `r##"..."##`, not `r#"..."#`: a colour literal contains `"#`, which would
    // end a single-hash raw string in the middle of the tag.
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="{side}" height="{side}" fill="#fff"/><g fill="#000">{squares}</g></svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_round_trips() {
        let invitation = invitation("100.64.0.12:7397", "abc123", "PQRS2345", "Hari's journal");
        let invite = parse(&invitation.url).unwrap();
        assert_eq!(invite.host, "100.64.0.12:7397");
        assert_eq!(invite.fingerprint, "abc123");
        assert_eq!(invite.code, "PQRS2345");
        assert_eq!(invite.name, "Hari's journal");
    }

    #[test]
    fn a_url_survives_being_copied_by_a_person() {
        let invitation = invitation("10.0.0.4:7397", "ff00", "AAAA1111", "Vault");
        for mangled in [
            format!("  {}  ", invitation.url),
            format!("<{}>", invitation.url),
            format!("{}.", invitation.url),
        ] {
            assert_eq!(parse(&mangled).unwrap().code, "AAAA1111", "{mangled}");
        }
    }

    #[test]
    fn a_link_missing_what_it_needs_says_which() {
        assert!(parse("https://example.com").unwrap_err().contains("pairing link"));
        assert!(parse("everyday://pair?code=X").unwrap_err().contains("which computer"));
        assert!(parse("everyday://pair?host=a:1").unwrap_err().contains("pairing code"));
    }

    #[test]
    fn a_qr_code_is_drawn_for_the_url_that_is_shown() {
        let invitation = invitation("10.0.0.4:7397", "ff00", "AAAA1111", "Vault");
        assert!(invitation.qr_svg.starts_with("<svg"));
        assert!(invitation.qr_svg.contains("<rect"));
        // A quiet zone: the outermost module is never dark.
        assert!(invitation.qr_svg.contains(r#"<rect x="4" y="4""#));
    }
}
