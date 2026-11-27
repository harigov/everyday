//! What is said: the wording of replies, questions and notices, as small pure functions.

use super::*;

// ---- what is said -----------------------------------------------------------

/// The one message a finished turn ends with, if it should send one.
///
/// What the assistant said; failing that, everything it streamed; failing
/// that, "Stopped." for a turn that was stopped, and "(no reply)" for one that
/// simply said nothing. A turn that failed says so, in the model's or the
/// harness's own words -- including a failure before it started, such as the
/// assistant being switched off.
///
/// `owner_said_stopped` is the one case that sends nothing: the owner typed
/// `!stop`, was told "Stopped." the moment they did, and a second one would be
/// noise. If the turn had something to say before it was stopped, that is still
/// sent.
pub(crate) fn final_reply(
    result: &CommandResult<Turned>,
    told: &str,
    owner_said_stopped: bool,
) -> Option<String> {
    let turned = match result {
        Ok(turned) => turned,
        Err(e) => return Some(format!("Something went wrong: {}", e.message)),
    };
    let text = turned.text.trim();
    if !text.is_empty() {
        return Some(text.to_string());
    }
    let told = told.trim();
    if !told.is_empty() {
        return Some(told.to_string());
    }
    if turned.stopped {
        return (!owner_said_stopped).then(|| STOPPED.to_string());
    }
    Some(NO_REPLY.to_string())
}

/// What the bot says about itself: what it is, what to type.
pub(crate) fn help_text(assistant_name: &str) -> String {
    let name = assistant_name.trim();
    let who = if name.is_empty() {
        "your assistant".to_string()
    } else {
        format!("{}, your assistant", escape_markdown(name))
    };
    format!(
        "I'm {who} from Jatayu \u{2014} the same one that lives on your computer, answering \
         from this chat. Ask for what you would ask for there: what's on today, a reply to \
         someone, a task added.\n\n\
         - `!new` starts a new conversation\n\
         - `!stop` stops what I'm doing\n\
         - `!help` shows this message\n\n\
         Before I delete anything, or send something to someone else, I'll ask you here first. \
         If I ask before opening a web page, answer \"always\" and I'll open pages on that \
         website without asking from then on. \
         Routines you have sent here post what they did, and may ask you something while they \
         work \u{2014} answer them here too."
    )
}

/// The question put to the owner for a call that needs their say.
///
/// `subject` is what the assistant's tools name the thing as -- the record's
/// own title for a delete, "to alice@example.com \u{2014} Re: dinner \u{2014} \u{2026}" for a send,
/// the query for a search, the address for a fetch -- so each kind has its own
/// words around it, the same distinction the desktop's card draws. The choices
/// are Yes and No, Later where the call can be saved as a proposal, and Always
/// where `site` names the website that would be allowed from now on.
pub(crate) fn confirmation_question(
    kind: &str,
    subject: &str,
    can_park: bool,
    site: Option<&str>,
) -> Question {
    let subject = subject.trim();
    let site = site.map(one_line).filter(|site| !site.is_empty());
    let mut markdown = match kind {
        "destructive" if subject.is_empty() => {
            "**I want to delete something, but I can't say what.** That can't be undone."
                .to_string()
        }
        "destructive" => {
            format!("**Delete {}?** That can't be undone.", escape_markdown(&one_line(subject)))
        }
        "outward" => with_quote("**This reaches someone outside your vault.**", subject),
        "search" => with_quote(
            "**Search the web for this?** I've read your mail in this conversation, so I'm \
             checking before words from it leave your machine.",
            subject,
        ),
        "fetch" => {
            // As code, so the address is shown rather than turned into a link
            // that one tap would follow.
            let head = "**Open this address?** I put it together myself rather than finding it, \
                        and whatever is in it reaches that site.";
            if subject.is_empty() {
                head.to_string()
            } else {
                format!("{head}\n\n`{}`", one_line(subject).replace('`', "'"))
            }
        }
        // Typing into a page in the assistant's browser: the text, the field
        // and the site, which the site can read the moment it is typed.
        "type" => with_quote(
            "**Type this into a web page?** The site can read it as soon as it is typed.",
            subject,
        ),
        "click" => with_quote(
            "**Click this in the browser?** I've read your mail in this conversation, so I'm \
             checking that the mail isn't choosing where I go.",
            subject,
        ),
        // A routine whose instructions name an address nobody gave: as code,
        // as `fetch` shows one, so it is read rather than tapped.
        "routine" => {
            let head = "**Save a routine that opens this?** Neither you nor a search gave me \
                        this address, and the routine's runs will open it without asking.";
            if subject.is_empty() {
                head.to_string()
            } else {
                format!("{head}\n\n`{}`", one_line(subject).replace('`', "'"))
            }
        }
        // A money rule, or a change to many transactions, after somebody
        // else's words -- a transfer's memo, a mail -- were read.
        "rewrite" => with_quote(
            "**Change these transactions?** I've read words from someone else in this \
             conversation, so I'm checking before I rewrite many of them at once.",
            subject,
        ),
        // Something to be kept for good, read back as the owner's own wishes
        // -- a memory, a skill, a routine, a task for the assistant -- after
        // somebody else's words were read. The subject's first line says
        // what, and the lines after it are the text that would be saved, in
        // full: that text is what is being agreed to.
        "keep" => with_quote(
            "**Keep this?** I've read words from someone else in this conversation, so I'm \
             checking before I save something that I would read back later as what you want.",
            subject,
        ),
        // A tool of a connected service, which acts there as the owner: the
        // subject's first line names the tool and the connection, and the
        // lines after it are the arguments.
        "connection" => {
            with_quote("**Use this in a connected service?** It acts there as you.", subject)
        }
        _ => with_quote("**Go ahead with this?**", subject),
    };
    markdown.push_str("\n\n");

    let mut choices = vec![Choice::Yes, Choice::No];
    if can_park {
        choices.push(Choice::Later);
    }
    if site.is_some() {
        choices.push(Choice::Always);
    }
    let later = "later saves it for you to decide in the app";
    // As code, like the address above, so a host is read and not made a link.
    let always = site.map(|site| {
        format!(
            "always lets me open pages on `{}` from now on without asking",
            site.replace('`', "'")
        )
    });
    markdown.push_str(&match (can_park, always) {
        (false, None) => "Reply **yes** or **no**.".to_string(),
        (true, None) => format!("Reply **yes**, **no** or **later** ({later})."),
        (false, Some(always)) => format!("Reply **yes**, **no** or **always** ({always})."),
        (true, Some(always)) => {
            format!("Reply **yes**, **no**, **later** or **always** ({later}; {always}).")
        }
    });
    Question { markdown, choices }
}

/// The line a routine's message to the owner starts with, so that a question
/// arriving out of nowhere says whose it is.
pub(super) fn from_routine(routine: &str) -> String {
    format!("_From your routine **{}**:_\n\n", escape_markdown(&one_line(routine)))
}

/// A routine's own question, as the owner sees it: whose it is, the question
/// in the model's words, and the choices numbered if there are any.
///
/// Said to be the assistant's own words, and set apart as a quotation: a run
/// that has read somebody's mail or a page can be talked into putting a
/// paragraph of it in a question, and a message that arrives in the owner's
/// own chat from a routine they set up reads as the routine's -- so the
/// owner is told that the assistant wrote it, itself, while the routine ran.
/// Escaped as every other name and quotation shown in a chat is, so a link in
/// it is shown as written and not made one to tap.
pub(crate) fn free_question(routine: &str, question: &str, choices: &[String]) -> String {
    let mut out = format!(
        "_Your routine **{}** is running, and the assistant wrote this question itself:_\n\n",
        escape_markdown(&one_line(routine))
    );
    let quoted: Vec<String> = question
        .trim()
        .lines()
        .map(|line| format!("> {}", escape_markdown(line.trim_end())))
        .collect();
    out.push_str(&quoted.join("\n"));
    if choices.is_empty() {
        out.push_str("\n\nReply here.");
    } else {
        out.push_str("\n\n");
        for (n, choice) in choices.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", n + 1, escape_markdown(&one_line(choice))));
        }
        out.push_str("\nReply with a number, or in your own words.");
    }
    out
}

/// What the owner said, with a bare number read as the choice it numbers.
pub(crate) fn pick_choice(said: &str, choices: &[String]) -> String {
    let trimmed = said.trim().trim_end_matches('.');
    match trimmed.parse::<usize>() {
        Ok(n) if (1..=choices.len()).contains(&n) => choices[n - 1].clone(),
        _ => said.trim().to_string(),
    }
}

/// `head`, then `subject` as a quotation, if there is one.
fn with_quote(head: &str, subject: &str) -> String {
    if subject.is_empty() {
        return head.to_string();
    }
    let quoted: Vec<String> =
        subject.lines().map(|line| format!("> {}", escape_markdown(line.trim_end()))).collect();
    format!("{head}\n\n{}", quoted.join("\n"))
}

/// `text` on one line, whitespace runs folded to a space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What an answer means to the turn that asked.
///
/// "Later" without a proposal form to park into is not on offer, so it is
/// read as the one safe thing: no. "Always" on a question that named no site
/// is not on offer either, and is read the same way: a delete, a send or a
/// keep was never put to them as something to say yes to by that word, and a
/// reaction or a phrase that meant nothing before this answer existed must not
/// start doing the thing.
pub(crate) fn answer_for(choice: Choice, can_park: bool, can_trust: bool) -> ConfirmAnswer {
    match choice {
        Choice::Yes => ConfirmAnswer::Confirm,
        Choice::No => ConfirmAnswer::Decline,
        Choice::Later if can_park => ConfirmAnswer::Later,
        Choice::Later => ConfirmAnswer::Decline,
        Choice::Always if can_trust => ConfirmAnswer::TrustSite,
        Choice::Always => ConfirmAnswer::Decline,
    }
}

/// What silence means: save it for later if it can be saved, otherwise no.
/// Never yes.
pub(crate) fn timeout_answer(can_park: bool) -> ConfirmAnswer {
    if can_park { ConfirmAnswer::Later } else { ConfirmAnswer::Decline }
}

/// What the owner is told when a question timed out, so that a missing answer
/// does not look like nothing having happened.
pub(crate) fn timeout_notice(answer: ConfirmAnswer) -> &'static str {
    match answer {
        ConfirmAnswer::Later => {
            "No answer, so I've saved that for later. You can decide in the Assistant app."
        }
        _ => "No answer, so I haven't done it. Ask me again if you still want it.",
    }
}
