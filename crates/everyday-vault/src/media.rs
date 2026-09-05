//! Serving attachment bytes to a webview.
//!
//! The desktop shell answers `everyday://` requests with these, and a mobile
//! shell would answer identically. The logic lives here rather than in the
//! Tauri crate for two reasons: both front ends need exactly the same
//! behaviour, and this way it is testable on a machine that cannot build a
//! webview at all.

/// Upper bound on a single response body.
///
/// An open-ended `Range: bytes=N-` asks for "the rest of the file". Answering
/// a 400 MB request literally would defeat the point of ranges, so we answer
/// with a window and let the player come back for more, exactly as an
/// ordinary HTTP server does.
pub const MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// What to send for a media request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsePlan {
    /// First byte to read.
    pub start: u64,
    /// How many bytes to read.
    pub len: u64,
    /// Total size of the blob, for `Content-Range`.
    pub total: u64,
    /// Whether this is a `206 Partial Content` rather than a `200`.
    pub partial: bool,
}

impl ResponsePlan {
    /// The `Content-Range` header value, when partial.
    pub fn content_range(&self) -> String {
        format!(
            "bytes {}-{}/{}",
            self.start,
            self.start + self.len.saturating_sub(1),
            self.total
        )
    }
}

/// Decide what to serve for a blob of `total` bytes and an optional `Range`.
///
/// A malformed or unsatisfiable range is treated as absent, which is the
/// behaviour RFC 9110 permits and which keeps a bad header from turning into
/// an error the user sees as a broken image.
pub fn plan(total: u64, range_header: Option<&str>) -> ResponsePlan {
    let (start, end, asked) = match range_header.and_then(|v| parse_range(v, total)) {
        Some((s, e)) => (s, e, true),
        None => (0, total.saturating_sub(1).min(MAX_CHUNK - 1), false),
    };
    let len = end.saturating_sub(start).saturating_add(1).min(total.saturating_sub(start));
    ResponsePlan {
        start,
        len,
        total,
        // Report 206 whenever less than the whole blob is being sent, asked
        // for or not: claiming 200 for a truncated body would make a player
        // believe it already had the entire file.
        partial: asked || len < total,
    }
}

/// Parse a single-range `Range: bytes=…` header against a known total size.
///
/// Returns an inclusive `(start, end)`. Multi-range requests are refused;
/// serving the first range instead would be a silent lie about the response.
pub fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (start, end) = match (from.trim(), to.trim()) {
        // `bytes=-N`: the final N bytes.
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (total.saturating_sub(n), total - 1)
        }
        // `bytes=N-`: from N to the end, bounded by the chunk limit.
        (s, "") => {
            let s: u64 = s.parse().ok()?;
            (s, s.saturating_add(MAX_CHUNK - 1).min(total - 1))
        }
        (s, e) => {
            let s: u64 = s.parse().ok()?;
            let e: u64 = e.parse().ok()?;
            (s, e.min(total - 1).min(s.saturating_add(MAX_CHUNK - 1)))
        }
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

/// Identify a media type from its leading bytes.
///
/// Sniffed rather than read from the document, so a hand-edited or imported
/// entry cannot talk the webview into interpreting a payload as something it
/// is not. Anything unrecognised is served as an opaque download.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    fn at(b: &[u8], off: usize, pat: &[u8]) -> bool {
        b.len() >= off + pat.len() && &b[off..off + pat.len()] == pat
    }

    match bytes {
        b if at(b, 0, b"\x89PNG\r\n\x1a\n") => "image/png",
        b if at(b, 0, &[0xFF, 0xD8, 0xFF]) => "image/jpeg",
        b if at(b, 0, b"GIF87a") || at(b, 0, b"GIF89a") => "image/gif",
        b if at(b, 0, b"RIFF") && at(b, 8, b"WEBP") => "image/webp",
        b if at(b, 4, b"ftypavif") => "image/avif",
        b if at(b, 4, b"ftypheic") || at(b, 4, b"ftypmif1") => "image/heic",
        b if at(b, 0, b"<svg") || at(b, 0, b"<?xml") => "image/svg+xml",
        b if at(b, 0, b"BM") => "image/bmp",

        b if at(b, 4, b"ftypqt  ") => "video/quicktime",
        b if at(b, 4, b"ftypM4A") => "audio/mp4",
        b if at(b, 4, b"ftyp") => "video/mp4",
        b if at(b, 0, &[0x1A, 0x45, 0xDF, 0xA3]) => "video/webm",
        b if at(b, 0, b"RIFF") && at(b, 8, b"AVI ") => "video/x-msvideo",

        b if at(b, 0, b"ID3") || at(b, 0, &[0xFF, 0xFB]) => "audio/mpeg",
        b if at(b, 0, b"RIFF") && at(b, 8, b"WAVE") => "audio/wav",
        b if at(b, 0, b"OggS") => "audio/ogg",
        b if at(b, 0, b"fLaC") => "audio/flac",

        b if at(b, 0, b"%PDF-") => "application/pdf",

        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_common_range_forms() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-", 1000), Some((100, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("  bytes=0-0  ", 1000), Some((0, 0)));
        assert_eq!(parse_range("bytes=999-999", 1000), Some((999, 999)));
    }

    #[test]
    fn clamps_ranges_to_the_file_and_to_the_chunk_limit() {
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
        let big = 64 * 1024 * 1024;
        assert_eq!(parse_range("bytes=0-", big), Some((0, MAX_CHUNK - 1)));
        assert_eq!(parse_range("bytes=0-99999999", big), Some((0, MAX_CHUNK - 1)));
    }

    #[test]
    fn rejects_nonsense_ranges_rather_than_erroring() {
        for bad in [
            "", "items=0-9", "bytes=abc-def", "bytes=500-100", "bytes=",
            "bytes=0-1,5-6", "bytes=-0", "bytes=9999-", "bytes=1000-1001",
        ] {
            assert_eq!(parse_range(bad, 1000), None, "accepted {bad:?}");
        }
        assert_eq!(parse_range("bytes=0-10", 0), None, "an empty blob has no ranges");
    }

    #[test]
    fn huge_range_bounds_do_not_overflow() {
        // `start + MAX_CHUNK` would wrap without saturating arithmetic.
        assert_eq!(parse_range(&format!("bytes={}-", u64::MAX), 1000), None);
        assert_eq!(parse_range(&format!("bytes=0-{}", u64::MAX), 1000), Some((0, 999)));
        assert_eq!(parse_range(&format!("bytes=-{}", u64::MAX), 1000), Some((0, 999)));
    }

    #[test]
    fn a_small_blob_is_served_whole_as_a_200() {
        let p = plan(500, None);
        assert_eq!(p, ResponsePlan { start: 0, len: 500, total: 500, partial: false });
    }

    #[test]
    fn an_empty_blob_is_handled_without_underflow() {
        let p = plan(0, None);
        assert_eq!(p.start, 0);
        assert_eq!(p.len, 0);
        assert_eq!(p.total, 0);
    }

    #[test]
    fn a_large_blob_without_a_range_is_still_bounded_and_marked_partial() {
        let total = 64 * 1024 * 1024;
        let p = plan(total, None);
        assert_eq!(p.len, MAX_CHUNK);
        assert!(p.partial, "a truncated body must not claim to be complete");
        assert_eq!(p.content_range(), format!("bytes 0-{}/{}", MAX_CHUNK - 1, total));
    }

    #[test]
    fn a_seek_into_the_middle_reads_only_that_window() {
        let total = 64 * 1024 * 1024;
        let p = plan(total, Some("bytes=33554432-33555431"));
        assert_eq!(p.start, 33_554_432);
        assert_eq!(p.len, 1000);
        assert!(p.partial);
        assert_eq!(p.content_range(), format!("bytes 33554432-33555431/{total}"));
    }

    #[test]
    fn a_bad_range_header_falls_back_to_the_start_of_the_file() {
        let p = plan(1000, Some("bytes=nonsense"));
        assert_eq!(p.start, 0);
        assert_eq!(p.len, 1000);
    }

    #[test]
    fn content_range_is_correct_for_a_single_byte() {
        let p = plan(1000, Some("bytes=42-42"));
        assert_eq!(p.content_range(), "bytes 42-42/1000");
    }

    #[test]
    fn sniffs_the_formats_a_journal_actually_holds() {
        assert_eq!(sniff_mime(b"\x89PNG\r\n\x1a\n....."), "image/png");
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_mime(b"GIF89a......."), "image/gif");
        assert_eq!(sniff_mime(b"RIFF____WEBPVP8 "), "image/webp");
        assert_eq!(sniff_mime(b"____ftypavif........"), "image/avif");
        assert_eq!(sniff_mime(b"____ftypisom........"), "video/mp4");
        assert_eq!(sniff_mime(b"____ftypqt  ........"), "video/quicktime");
        assert_eq!(sniff_mime(b"____ftypM4A ........"), "audio/mp4");
        assert_eq!(sniff_mime(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]), "video/webm");
        assert_eq!(sniff_mime(b"RIFF____WAVEfmt "), "audio/wav");
        assert_eq!(sniff_mime(b"OggS...."), "audio/ogg");
        assert_eq!(sniff_mime(b"%PDF-1.7"), "application/pdf");
    }

    #[test]
    fn unknown_content_is_not_guessed_into_something_executable() {
        assert_eq!(sniff_mime(b"<html><script>alert(1)</script>"), "application/octet-stream");
        assert_eq!(sniff_mime(b""), "application/octet-stream");
        assert_eq!(sniff_mime(b"\x00"), "application/octet-stream");
        assert_eq!(sniff_mime(b"MZ"), "application/octet-stream", "a PE binary is not media");
    }

    #[test]
    fn sniffing_short_buffers_never_panics() {
        for n in 0..24 {
            let _ = sniff_mime(&vec![0x66u8; n]);
            let _ = sniff_mime(&b"RIFF"[..n.min(4)]);
        }
    }
}
