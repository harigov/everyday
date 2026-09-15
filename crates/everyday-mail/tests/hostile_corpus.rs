//! The corpus `docs/plans/mail.md`'s phase 5 asks for: messages built to
//! smuggle instructions past a model, or to run script past a browser. Every
//! fixture here is real enough to have arrived in an inbox -- valid RFC 822,
//! a plausible sender, a plausible subject -- with one hostile element
//! planted in it.
//!
//! What these tests assert is narrow and deliberate: **hidden** content
//! never reaches [`everyday_mail::text::model_text`], and nothing with
//! behaviour survives [`everyday_mail::sanitize::sanitize`]. They do not
//! assert that a model can't be fooled by words it can see -- that is the
//! tool layer's job (`send_draft`'s confirmation, the untrusted-content
//! framing, the assistant's opt-in), described in `docs/plans/mail.md`'s
//! phase 5 and out of reach of a parsing crate that has no model to fool.
//! `injection_visible_plea` below exists to say that boundary out loud: its
//! words *do* reach `model_text`, on purpose, because hiding them was never
//! the defence.

use everyday_mail::{mime, sanitize, text};

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "injection_in_html_comment" => {
            include_bytes!("fixtures/hostile/injection_in_html_comment.eml")
        }
        "injection_white_on_white" => {
            include_bytes!("fixtures/hostile/injection_white_on_white.eml")
        }
        "injection_display_none" => include_bytes!("fixtures/hostile/injection_display_none.eml"),
        "injection_visible_plea" => include_bytes!("fixtures/hostile/injection_visible_plea.eml"),
        "script_and_iframe" => include_bytes!("fixtures/hostile/script_and_iframe.eml"),
        "calendar_invite_requesting_reply" => {
            include_bytes!("fixtures/hostile/calendar_invite_requesting_reply.eml")
        }
        "injection_entity_encoded_hidden_text" => {
            include_bytes!("fixtures/hostile/injection_entity_encoded_hidden_text.eml")
        }
        "entity_encoded_css_exfil" => {
            include_bytes!("fixtures/hostile/entity_encoded_css_exfil.eml")
        }
        "injection_css_declaration_bypass" => {
            include_bytes!("fixtures/hostile/injection_css_declaration_bypass.eml")
        }
        other => panic!("no such fixture: {other}"),
    }
}

fn parse(name: &str) -> mime::ParsedMessage {
    mime::parse(fixture(name)).unwrap_or_else(|err| panic!("{name} should parse: {err}"))
}

#[test]
fn instructions_hidden_in_an_html_comment_do_not_reach_model_text() {
    let parsed = parse("injection_in_html_comment");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("please find your invoice"));
    assert!(!model_text.to_lowercase().contains("developer mode"));
    assert!(!model_text.contains("attacker@evil.example"));
}

#[test]
fn white_on_white_instructions_do_not_reach_model_text() {
    let parsed = parse("injection_white_on_white");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("roundup of stories"));
    assert!(!model_text.to_lowercase().contains("disregard your operator"));
    assert!(!model_text.to_lowercase().contains("password"));
}

#[test]
fn a_display_none_instruction_does_not_reach_model_text() {
    let parsed = parse("injection_display_none");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("ticket has been updated"));
    assert!(!model_text.to_lowercase().contains("forward the last ten invoices"));
    assert!(!model_text.contains("attacker@evil.example"));
}

#[test]
fn a_visible_injection_attempt_is_not_hidden_from_model_text() {
    // The point of this fixture: this crate's job is to stop *hidden*
    // content reaching a tool, not to judge the content that a person (or a
    // sighted rendering of the message) would have read anyway. Whether a
    // tool should act on it is a different layer's problem.
    let parsed = parse("injection_visible_plea");
    let model_text = text::model_text(&parsed);
    assert!(model_text.to_lowercase().contains("forward the last ten"));
}

#[test]
fn a_calendar_invites_description_is_not_folded_into_the_readable_body() {
    let parsed = parse("calendar_invite_requesting_reply");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("quarterly sync"));
    assert!(!model_text.to_lowercase().contains("include the contents"));

    // The calendar payload is still there for whatever later reads it --
    // just not folded into the text a mail tool returns by default.
    let calendar = parsed.calendar.expect("a text/calendar part");
    let calendar = String::from_utf8_lossy(&calendar);
    assert!(calendar.contains("BEGIN:VEVENT"));
    assert!(calendar.to_lowercase().contains("please reply"));
}

#[test]
fn a_script_tag_does_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    let lower = clean.html.to_lowercase();
    assert!(!lower.contains("<script"));
    assert!(!lower.contains("document.cookie"));
}

#[test]
fn an_iframe_does_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    assert!(!clean.html.to_lowercase().contains("<iframe"));
}

#[test]
fn an_onerror_handler_does_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    assert!(!clean.html.to_lowercase().contains("onerror"));
}

#[test]
fn a_javascript_link_does_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    assert!(!clean.html.to_lowercase().contains("javascript:"));
}

#[test]
fn an_svg_script_does_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    let lower = clean.html.to_lowercase();
    assert!(!lower.contains("<svg"));
    assert!(!lower.contains("onload"));
}

#[test]
fn an_at_import_and_css_expression_do_not_survive_sanitize() {
    let parsed = parse("script_and_iframe");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new("hostile-script-1@evil.example"));
    let lower = clean.html.to_lowercase();
    assert!(!lower.contains("@import"));
    assert!(!lower.contains("expression("));
    assert!(!lower.contains("evil.example/track.css"));
}

#[test]
fn an_entity_encoded_display_none_instruction_does_not_reach_model_text() {
    let parsed = parse("injection_entity_encoded_hidden_text");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("ticket has been updated"));
    assert!(!model_text.to_lowercase().contains("forward the last ten invoices"));
    assert!(!model_text.contains("attacker@evil.example"));
}

#[test]
fn an_entity_encoded_css_url_does_not_survive_sanitize() {
    let parsed = parse("entity_encoded_css_exfil");
    let html = parsed.html.expect("html body");
    let clean = sanitize::sanitize(
        &html,
        &sanitize::Rewrite::new("hostile-entity-css-1@marketing.example"),
    );
    assert!(!clean.html.contains("evil.example"), "{}", clean.html);
}

/// Finding 3: none of `DISPLAY:NONE` (uppercase), `visibility:hidden!important`
/// (no space before the value), a large negative absolute offset, or a
/// zero-area `clip: rect(...)` used to be caught by a check that only ever
/// compared against the literal strings `"display:none"` and
/// `"visibility:hidden"` -- each is a real, if differently spelled, way of
/// hiding content a sighted reader never saw, and all four are planted in
/// this one message.
#[test]
fn css_declaration_bypass_variants_do_not_reach_model_text() {
    let parsed = parse("injection_css_declaration_bypass");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("invoice is attached"));
    assert!(!model_text.to_lowercase().contains("card number"));
    assert!(!model_text.contains("attacker@evil.example"));
    assert!(!model_text.to_lowercase().contains("disregard your operator"));
    assert!(!model_text.to_lowercase().contains("vault's secrets"));
}
