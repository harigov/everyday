//! A Server-Sent Events parser for Streamable HTTP replies.
//!
//! A reply to a POST may be a stream of events, and the answer to the
//! request is one of them. The bytes arrive in whatever pieces the network
//! cuts them into -- mid-line, mid-character -- so the parser is fed pieces
//! and hands back whole events.

/// One dispatched event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    /// The `event:` field; absent means the default, `message`.
    pub name: Option<String>,
    /// Every `data:` line, joined with `\n`.
    pub data: String,
    /// The `id:` field.
    pub id: Option<String>,
}

/// Incremental parser. Lines end at `\n` or `\r\n`; a blank line ends an
/// event; a line starting with `:` is a comment (servers send them to keep a
/// connection warm); fields other than `event`, `data` and `id` are ignored.
#[derive(Debug, Default)]
pub struct Parser {
    /// Bytes of a line whose end has not arrived.
    pending: Vec<u8>,
    name: Option<String>,
    data: Option<String>,
    id: Option<String>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next piece of the body; get back every event it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Event> {
        let mut buffer = std::mem::take(&mut self.pending);
        buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut start = 0;
        while let Some(offset) = buffer[start..].iter().position(|b| *b == b'\n') {
            let end = start + offset;
            if let Some(event) = self.line(&buffer[start..end]) {
                events.push(event);
            }
            start = end + 1;
        }
        self.pending = buffer.split_off(start);
        events
    }

    /// The body ended. A server that closes right after its last `data:`
    /// line, without the blank line, still meant that event.
    pub fn finish(&mut self) -> Option<Event> {
        let rest = std::mem::take(&mut self.pending);
        let from_line = if rest.is_empty() { None } else { self.line(&rest) };
        from_line.or_else(|| self.dispatch())
    }

    fn line(&mut self, line: &[u8]) -> Option<Event> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return self.dispatch();
        }
        if line[0] == b':' {
            return None;
        }
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (&*text, ""),
        };
        match field {
            "event" => self.name = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            "id" => self.id = Some(value.to_string()),
            _ => {}
        }
        None
    }

    /// A blank line: hand over the event being built, if it has any data.
    fn dispatch(&mut self) -> Option<Event> {
        let name = self.name.take();
        let id = self.id.take();
        self.data.take().map(|data| Event { name, data, id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(parts: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for part in parts {
            events.extend(parser.push(part));
        }
        events.extend(parser.finish());
        events
    }

    #[test]
    fn one_event() {
        let events = all(&[b"event: message\ndata: {\"a\":1}\n\n"]);
        assert_eq!(
            events,
            vec![Event { name: Some("message".into()), data: "{\"a\":1}".into(), id: None }]
        );
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        let events = all(&[b"data: one\ndata: two\ndata:three\n\n"]);
        assert_eq!(events[0].data, "one\ntwo\nthree");
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        let events = all(&[b": keep-alive\nretry: 100\n\ndata: x\n: another\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn pieces_may_split_anywhere() {
        let whole = "id: 7\nevent: message\ndata: h\u{e9}llo\r\n\r\ndata: second\n\n".as_bytes();
        let expected = all(&[whole]);
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].data, "h\u{e9}llo");
        assert_eq!(expected[0].id.as_deref(), Some("7"));
        for cut in 0..whole.len() {
            assert_eq!(all(&[&whole[..cut], &whole[cut..]]), expected, "cut at {cut}");
        }
    }

    #[test]
    fn a_last_event_without_its_blank_line_still_counts() {
        let events = all(&[b"data: a\n\ndata: b"]);
        assert_eq!(events.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn a_blank_line_with_no_data_dispatches_nothing() {
        assert!(all(&[b"event: ping\n\n\n"]).is_empty());
    }

    #[test]
    fn an_empty_data_line_is_an_event_with_empty_data() {
        let events = all(&[b"data:\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "");
    }
}
