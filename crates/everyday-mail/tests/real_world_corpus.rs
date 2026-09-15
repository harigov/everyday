//! A handful of ordinary, real-world-shaped messages, alongside the hostile
//! corpus in `hostile_corpus.rs`. These exist so the hostile-content tests
//! are not the only evidence this crate works on real mail -- a parser that
//! only survives adversarial input but mangles a Gmail reply chain is not
//! actually done.

use everyday_mail::{mime, sanitize, text, threading};

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "gmail_reply_chain" => include_bytes!("fixtures/real_world/gmail_reply_chain.eml"),
        "outlook_reply" => include_bytes!("fixtures/real_world/outlook_reply.eml"),
        "newsletter_with_tracking_pixel" => {
            include_bytes!("fixtures/real_world/newsletter_with_tracking_pixel.eml")
        }
        "multipart_related_cid_images" => {
            include_bytes!("fixtures/real_world/multipart_related_cid_images.eml")
        }
        "newsletter_encoded_image_query_string" => {
            include_bytes!("fixtures/real_world/newsletter_encoded_image_query_string.eml")
        }
        other => panic!("no such fixture: {other}"),
    }
}

fn parse(name: &str) -> mime::ParsedMessage {
    mime::parse(fixture(name)).unwrap_or_else(|err| panic!("{name} should parse: {err}"))
}

#[test]
fn a_gmail_reply_chains_new_text_is_kept_and_the_quote_is_not() {
    let parsed = parse("gmail_reply_chain");
    let text = parsed.text.clone().expect("plain text alternative");
    let ranges = text::quoted_ranges(&text);
    assert!(!ranges.is_empty(), "the 'On ... wrote:' block should be found");

    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("Thursday works for me"));
    assert!(!model_text.contains("that new place"));
}

#[test]
fn a_gmail_reply_threads_with_its_parent_by_references() {
    let parsed = parse("gmail_reply_chain");
    let reply = threading::ThreadInput {
        id: "local-reply".to_string(),
        message_id: parsed.message_id.clone(),
        in_reply_to: parsed.in_reply_to.clone(),
        references: parsed.references.clone(),
        subject: parsed.subject.clone().unwrap_or_default(),
        date: parsed.date.expect("date header"),
    };

    let root = threading::Thread {
        id: "gmail-root-1@mail.gmail.com".to_string(),
        members: vec![threading::ThreadInput {
            id: "local-root".to_string(),
            message_id: Some("gmail-root-1@mail.gmail.com".to_string()),
            in_reply_to: None,
            references: Vec::new(),
            subject: "Lunch on Thursday?".to_string(),
            date: jiff::Timestamp::from_second(1_768_000_000).unwrap(),
        }],
    };

    assert_eq!(
        threading::place(&[root], &reply, &threading::Options::default()),
        threading::Placement::Join { thread_id: "gmail-root-1@mail.gmail.com".to_string() }
    );
}

#[test]
fn an_outlook_replys_original_message_block_is_recognised_as_quoted() {
    let parsed = parse("outlook_reply");
    let text = parsed.text.clone().expect("plain text body");
    let model_text = text::model_text(&parsed);
    assert!(model_text.contains("Approved, go ahead"));
    assert!(!model_text.contains("Total comes to $4,200"));
    assert!(!text::quoted_ranges(&text).is_empty());
}

#[test]
fn a_newsletters_tracking_pixel_is_dropped_and_its_links_survive() {
    let parsed = parse("newsletter_with_tracking_pixel");
    let html = parsed.html.expect("html body");
    let clean =
        sanitize::sanitize(&html, &sanitize::Rewrite::new(parsed.message_id.clone().unwrap()));

    assert!(clean.had_tracking_pixels);
    assert!(!clean.html.contains("track.dailytimes.example"));
    assert!(clean.html.contains("Read more"));
    assert!(clean.html.contains("local council"));

    assert_eq!(
        parsed.list_id.as_deref(),
        Some("Daily Times Newsletter <newsletter.dailytimes.example>")
    );
    assert!(parsed.list_unsubscribe.is_some());
    assert_eq!(parsed.precedence.as_deref(), Some("bulk"));
}

/// Finding 1: `lol_html::Element::get_attribute` hands back an attribute's
/// *source* text, entities and all, so a `src` written the standards-correct
/// way -- `&amp;` for a literal `&` in a URL's query string -- used to be
/// hashed and proxied with the `&amp;` still in it, and then fetched
/// literally, breaking the image for most commercial mail. It has to be
/// decoded once, before hashing, so the token and the eventual fetch agree
/// on the same URL the sender actually meant.
#[test]
fn an_image_urls_entities_are_decoded_before_it_is_proxied() {
    let parsed = parse("newsletter_encoded_image_query_string");
    let html = parsed.html.expect("html body");
    let clean =
        sanitize::sanitize(&html, &sanitize::Rewrite::new(parsed.message_id.clone().unwrap()));

    assert_eq!(clean.remote_images.len(), 1);
    assert_eq!(
        clean.remote_images[0].original_url,
        "https://cdn.retailer.example/banner.png?w=600&h=300&fit=crop"
    );
    assert!(clean.html.contains("Shop now"));
}

#[test]
fn multipart_related_cid_images_are_proxied_to_their_own_part() {
    let parsed = parse("multipart_related_cid_images");
    let html = parsed.html.expect("html body");
    let message_id = parsed.message_id.clone().unwrap();
    let clean = sanitize::sanitize(&html, &sanitize::Rewrite::new(message_id.clone()));

    assert!(clean.html.contains(&format!("everyday://mail/part/{message_id}/logo@corp.example")));
    assert!(
        clean.html.contains(&format!("everyday://mail/part/{message_id}/wordmark@corp.example"))
    );
    // cid: images are inline, never fetched, so they're not "remote".
    assert!(clean.remote_images.is_empty());

    let logo_part = parsed
        .parts
        .iter()
        .find(|part| part.content_id.as_deref() == Some("logo@corp.example"))
        .expect("logo part");
    let bytes =
        mime::part_bytes(fixture("multipart_related_cid_images"), &logo_part.part_id).unwrap();
    assert_eq!(bytes, b"hello logo");
}
