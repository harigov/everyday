use super::*;

/// The most messages of a thread replayed to the model. See [`window`].
pub(super) const HISTORY_MESSAGES: usize = 80;

/// The most characters of a thread replayed to the model -- roughly fifteen
/// thousand tokens, which leaves a small model's context room for the
/// preamble, the tools and the work. See [`window`].
pub(super) const HISTORY_CHARS: usize = 60_000;

/// How far the start of a trimmed thread moves at a time, in characters'
/// worth -- a fifth of [`HISTORY_CHARS`], or sixteen short messages. See
/// [`window`].
pub(super) const HISTORY_STEP: usize = 12_000;

/// How many of the person's most recent messages are shown to the model
/// with their pictures, this turn's own included. Older pictures are named
/// in a line instead -- see [`replay`].
///
/// Every picture shown is uploaded again, and costs a thousand tokens or so
/// again, on every turn it is shown in; the rail is one conversation that
/// goes on for weeks. Three is the picture itself and the two follow-ups
/// that are usually about it ("and the one on the left?").
///
/// The message a picture came with changes when the picture stops being
/// shown, so a provider's prompt cache finds the thread new from there on,
/// once, on that turn. That is the cheaper side: shown for every turn
/// after, it would cost its thousand tokens again on each of them.
pub(super) const PICTURE_TURNS: usize = 3;

/// What one picture shown again counts for against [`HISTORY_CHARS`]: about
/// the thousand tokens a model spends looking at one.
pub(super) const PICTURE_CHARS: usize = 4_000;

/// The thread so far, in rig's shape, and whether the start of it was left
/// out.
///
/// Only what a model needs to continue: the prose either party produced, in
/// order -- and the pictures the person sent with their last few messages
/// (see [`PICTURE_TURNS`]), each read through `picture`. A picture older
/// than that, or one `picture` cannot give back, is named in a line after
/// the words it came with instead, so the model knows the conversation was
/// about something it can no longer see, and does not pretend otherwise. Tool calls and their results are deliberately *not* replayed --
/// they are kept in the vault so the panel can draw what happened, but
/// feeding a model back its own calls from a previous turn invites it to
/// treat them as still pending. What it needs from a finished turn is the
/// sentence that summarised it, which is the assistant message beside them.
///
/// Every kept message that carried files is followed by `files`' line naming
/// them (see [`files_line`]), however old it is: a line is a few words, and
/// the id in it is what lets the model read or link to a file the person
/// handed over three turns ago.
///
/// And only the recent part of it: see [`window`].
///
/// `current` is the message this turn is answering, already saved by
/// [`run_turn`] and passed to the model separately, so it must not also
/// appear in the history. Named rather than assumed to be the newest user or
/// assistant line, because it need not be one: the follow-up to a finished
/// job is saved as a [`Role::System`] line, which is not replayed, and
/// dropping "the newest message" there would drop the reply before it.
///
/// Other system lines -- a finished job's card, say -- are not replayed
/// either. They are the application talking to the person's screen, not to
/// the model, and what matters of them reaches it as the follow-up's own
/// line for the one turn it is about, and as the answer that turn gave.
pub(super) fn replay(
    thread: &[VaultMessage],
    current: MessageId,
    picture: impl Fn(&Attachment) -> Option<rig_agent::completion::message::UserContent>,
    files: impl Fn(&[FileId]) -> Option<String>,
) -> (Vec<rig_agent::completion::Message>, bool) {
    let spoken: Vec<&VaultMessage> = thread
        .iter()
        .filter(|m| m.id != current)
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .filter(|m| {
            !m.content.trim().is_empty() || !m.attachments.is_empty() || !m.files.is_empty()
        })
        .collect();
    // Where the messages whose pictures are shown again begin: the person's
    // last few before this one, which is the first of `PICTURE_TURNS`.
    let theirs: Vec<usize> =
        spoken.iter().enumerate().filter(|(_, m)| m.role == Role::User).map(|(i, _)| i).collect();
    let earlier = PICTURE_TURNS.saturating_sub(1);
    let shown_from = if earlier == 0 {
        spoken.len()
    } else {
        theirs.len().checked_sub(earlier).map_or(0, |i| theirs[i])
    };

    // Each message's pictures, read now so that one the vault cannot give
    // back is named in the text instead -- and the text's length, with what
    // the pictures shown will cost, is what the window is measured in.
    let mut pictures: Vec<Vec<rig_agent::completion::message::UserContent>> = Vec::new();
    let mut lines: Vec<(Role, String, usize)> = Vec::new();
    for (i, m) in spoken.iter().enumerate() {
        let (shown, unseen): (Vec<_>, Vec<_>) = if i >= shown_from {
            let read: Vec<_> = m.attachments.iter().map(|a| (a, picture(a))).collect();
            let unseen = read.iter().filter(|(_, c)| c.is_none()).map(|(a, _)| *a).collect();
            (read.into_iter().filter_map(|(_, c)| c).collect(), unseen)
        } else {
            (Vec::new(), m.attachments.iter().collect())
        };
        let text = with_unseen(&m.content, &unseen);
        let text = with_files_line(text, (!m.files.is_empty()).then(|| files(&m.files)).flatten());
        lines.push((m.role, text, shown.len() * PICTURE_CHARS));
        pictures.push(shown);
    }
    let borrowed: Vec<(Role, &str, usize)> =
        lines.iter().map(|(role, text, weight)| (*role, text.as_str(), *weight)).collect();
    let (kept, trimmed) = window(&borrowed, HISTORY_MESSAGES, HISTORY_CHARS, HISTORY_STEP);
    // What `window` keeps is always the end of what it was given.
    let first = spoken.len() - kept.len();
    let history = kept
        .into_iter()
        .zip(pictures.drain(first..))
        .map(|((role, text), shown)| match role {
            Role::User => user_message(text, shown),
            _ => rig_agent::completion::Message::assistant(text),
        })
        .collect();
    (history, trimmed)
}

/// `picture` as the model is sent it, or `None` if the vault cannot give it
/// back or it is not a type a provider takes -- which nothing stored through
/// [`crate::pictures`] can be.
pub(super) fn shown_picture(
    vault: &Vault,
    picture: &Attachment,
) -> Option<rig_agent::completion::message::UserContent> {
    use base64::Engine as _;
    use rig_agent::completion::message::{ImageMediaType, UserContent};
    let media_type = match picture.mime.as_str() {
        "image/jpeg" => ImageMediaType::JPEG,
        "image/png" => ImageMediaType::PNG,
        _ => return None,
    };
    let bytes = vault.blob(picture.blob).ok()?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(UserContent::image_base64(data, Some(media_type), None))
}

/// Whether `message` shows the model a picture.
pub(super) fn has_picture(message: &rig_agent::completion::Message) -> bool {
    use rig_agent::completion::message::UserContent;
    matches!(message, rig_agent::completion::Message::User { content }
        if content.iter().any(|c| matches!(c, UserContent::Image(_))))
}

/// The person's message as the model is sent it: their words, then each
/// picture shown -- and a line naming any that could not be.
pub(super) fn with_pictures(
    text: String,
    unseen: &[&Attachment],
    shown: Vec<rig_agent::completion::message::UserContent>,
) -> rig_agent::completion::Message {
    user_message(with_unseen(&text, unseen), shown)
}

/// A user message of `text` and `pictures`. Words first, and left out when
/// there are none: a picture sent with nothing said is still a message.
pub(super) fn user_message(
    text: String,
    pictures: Vec<rig_agent::completion::message::UserContent>,
) -> rig_agent::completion::Message {
    use rig_agent::completion::message::UserContent;
    if pictures.is_empty() {
        return rig_agent::completion::Message::user(text);
    }
    let mut content = Vec::with_capacity(pictures.len() + 1);
    if !text.trim().is_empty() {
        content.push(UserContent::text(text));
    }
    content.extend(pictures);
    rig_agent::completion::Message::User { content }
}

/// `text`, with a line after it naming pictures that came with it and that
/// the model is not shown. The names are the person's own, so the model can
/// say which one it means when it asks for it again.
pub(super) fn with_unseen(text: &str, unseen: &[&Attachment]) -> String {
    if unseen.is_empty() {
        return text.to_string();
    }
    let names: Vec<&str> = unseen.iter().map(|a| a.filename.as_str()).collect();
    let what = if unseen.len() == 1 { "a picture" } else { "pictures" };
    let note = format!(
        "[They sent {what} with this message ({}), which you are no longer shown. \
         Ask them to send it again if you need to look at it.]",
        names.join(", ")
    );
    if text.trim().is_empty() { note } else { format!("{text}\n\n{note}") }
}

/// `text`, with `line` after it when there is one. A message that is only
/// files is just the line.
pub(super) fn with_files_line(text: String, line: Option<String>) -> String {
    match line {
        Some(line) if text.trim().is_empty() => line,
        Some(line) => format!("{text}\n\n{line}"),
        None => text,
    }
}

/// A line telling the model which files a message carried: each one's name,
/// what sort of file it is, how big, and the id to read or link it by -- or
/// `None` when none of them can be named any more (all since deleted).
///
/// Names, not contents. A picture among the files is shown to the model
/// beside the message already (see [`crate::pictures::of_file`]); a text file
/// is never put into the message, because it can be any length and
/// `read_file` reads as much of it as is wanted. The names are somebody's --
/// a download's, a phone's -- so each is quoted, flattened onto one line and
/// capped, as every other name put in front of the model is.
pub(super) fn files_line(vault: &Vault, ids: &[FileId]) -> Option<String> {
    let named: Vec<String> = ids
        .iter()
        .filter_map(|id| vault.file(*id).ok())
        .map(|file| {
            format!(
                "{} ({}, {}, files:{})",
                quoted_name(&file.name),
                file_kind_word(file.kind),
                file_size_text(file.size),
                file.id
            )
        })
        .collect();
    if named.is_empty() {
        return None;
    }
    let what = if named.len() == 1 { "a file" } else { "files" };
    Some(format!(
        "[They attached {what} to this message: {}. A picture is shown to you with the \
         message; to read a text file, call read_file with its id. When you mention a file in \
         your reply, write it as a Markdown link whose target is its files: address, like \
         [name](files:id).]",
        named.join(", ")
    ))
}

/// A file's name for the model's context: on one line, at most 120
/// characters, and quoted with JSON's escaping so that whatever it says
/// stays inside the quotation marks.
pub(super) fn quoted_name(name: &str) -> String {
    let flat = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = if flat.chars().count() > 120 {
        format!("{}\u{2026}", flat.chars().take(120).collect::<String>())
    } else {
        flat
    };
    serde_json::to_string(&capped).unwrap_or_default()
}

/// What sort of file this is, in the words a person uses.
pub(super) fn file_kind_word(kind: karmayoga_core::FileKind) -> &'static str {
    use karmayoga_core::FileKind;
    match kind {
        FileKind::Image => "picture",
        FileKind::Video => "video",
        FileKind::Audio => "audio",
        FileKind::Document => "document",
        FileKind::Other => "file",
    }
}

/// A size as a file manager says it: `4.1 MB`, decimal.
pub(super) fn file_size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

/// The most recent part of a thread that fits in `max_messages` and
/// `max_chars`, and whether anything was left out to make it fit.
///
/// The rail is one conversation that goes on for weeks, and replaying all of
/// it with every turn would first make each turn slower and dearer and then
/// stop working altogether, at whatever length the model's context ran out.
/// So it is counted from the newest message backwards, and stops at the
/// first message that would break either budget -- except the newest, which
/// is always kept, and cut short rather than dropped if it is on its own
/// longer than the whole budget: the thing just said is the one thing the
/// next answer cannot do without. A window that begins partway through
/// then drops any replies at its start, so it opens on something the person
/// said -- unless that would leave nothing at all.
///
/// A message is measured by its characters and the third of its numbers:
/// what the pictures shown with it cost, in characters' worth (see
/// [`PICTURE_CHARS`]). What is kept is always the end of `said`, which is
/// how [`replay`] finds each kept message's pictures again.
///
/// Once the thread no longer fits, where it starts moves in steps of `step`
/// rather than one message every turn. A provider's prompt cache serves a
/// request only up to the first thing that differs from one it has seen,
/// and a window whose oldest message changed every turn made the whole
/// replayed thread new on every turn of a long conversation. This way it is
/// new once a step, and in between the window only grows at its end.
///
/// The steps are measured from the start of the thread in what each message
/// weighs -- its characters, but never less than `max_chars / max_messages`,
/// so that both budgets are counted in the one unit -- and the window
/// starts at the first message at or past the next step after the first
/// message that fits. What that costs is some of the oldest messages that
/// would have fitted, never one that would not: at most `step`'s worth and
/// the one message that straddles it -- a few long ones where they are
/// long, or sixteen short ones, out of the eighty.
///
/// What falls out of the window is not lost: it stays in the vault and on
/// screen, and [`Guidance`] tells the model that durable facts belong in
/// memory rather than in a thread it will not always be shown in full.
pub(super) fn window(
    said: &[(Role, &str, usize)],
    max_messages: usize,
    max_chars: usize,
    step: usize,
) -> (Vec<(Role, String)>, bool) {
    let mut chars = 0usize;
    let mut start = said.len();
    for (i, (_, text, pictures)) in said.iter().enumerate().rev() {
        let n = text.chars().count() + pictures;
        let kept = said.len() - start;
        if kept > 0 && (kept >= max_messages || chars + n > max_chars) {
            break;
        }
        chars += n;
        start = i;
    }
    if start > 0 {
        start = on_the_step(said, start, max_chars / max_messages.max(1), step.max(1));
    }
    let mut trimmed = start > 0;
    let mut kept: Vec<(Role, String)> =
        said[start..].iter().map(|(role, text, _)| (*role, text.to_string())).collect();

    let cut = kept.last().and_then(|(_, text)| text.char_indices().nth(max_chars)).map(|(i, _)| i);
    if let (Some(cut), Some((_, text))) = (cut, kept.last_mut()) {
        text.truncate(cut);
        text.push_str("\n\n[\u{2026} the rest of this message is not shown]");
        trimmed = true;
    }

    if trimmed {
        let replies = kept.iter().take_while(|(role, _)| *role == Role::Assistant).count();
        if replies < kept.len() {
            kept.drain(..replies);
        }
    }
    (kept, trimmed)
}

/// The first message at or after `start` that begins on or past a step --
/// see [`window`] -- and never past the newest.
pub(super) fn on_the_step(said: &[(Role, &str, usize)], start: usize, least: usize, step: usize) -> usize {
    let weights: Vec<usize> = said
        .iter()
        .map(|(_, text, pictures)| (text.chars().count() + pictures).max(least))
        .collect();
    let mut at: usize = weights[..start].iter().sum();
    let mark = at.div_ceil(step) * step;
    let mut i = start;
    while at < mark && i + 1 < said.len() {
        at += weights[i];
        i += 1;
    }
    i
}
