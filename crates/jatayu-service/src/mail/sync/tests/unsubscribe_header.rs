use jatayu_core::mail::ListUnsubscribe;
use jiff::Timestamp;

use super::fixtures::*;
// ---------------------------------------------------------------------
// Keeping `List-Unsubscribe` at ingest -- docs/plans/unsubscribe.md's
// "Keeping the header"
// ---------------------------------------------------------------------

/// A message whose header block carries `extra` (each line ending `\r\n`)
/// between its `Date` and its body.
fn raw_with(message_id: &str, from: &str, extra: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{message_id}>\r\n\
         From: {from}\r\n\
         To: me@example.com\r\n\
         Subject: This week\r\n\
         Date: 01 Jan 2024 09:00:00 +0000\r\n\
         {extra}\
         Content-Type: text/plain\r\n\r\n\
         Hello.\r\n"
    )
    .into_bytes()
}

/// Both list headers, the first folded over two lines the way senders do.
const BOTH: &str = "List-Unsubscribe: <mailto:leave@lists.example?subject=unsubscribe>,\r\n \
                    <https://lists.example/u/abc>\r\n\
                    List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n";

fn stored(env: &TestEnv, message_id: &str) -> jatayu_core::mail::Message {
    env.vault
        .message_by_message_id_header(env.account_id, message_id)
        .unwrap()
        .unwrap_or_else(|| panic!("{message_id} stored"))
}

/// What the header pass reads of the list headers is kept on the message
/// -- every usable link, in the sender's order, and whether one click was
/// offered -- and is still there once the bodies pass has written the
/// message's raw bytes, which rewrites the row. A message with no header, or
/// one naming nothing usable, keeps nothing.
#[tokio::test]
async fn the_list_headers_are_kept_on_the_message_they_came_with() {
    let env = TestEnv::new();
    let server = plain_server();
    let page_only = "List-Unsubscribe: <https://shop.example/leave?u=1>\r\n";
    let useless = "List-Unsubscribe: <javascript:alert(1)>\r\n";
    {
        let mut s = server.lock().unwrap();
        for (message_id, from, extra) in [
            ("both@lists.example", "news@lists.example", BOTH),
            ("page@shop.example", "deals@shop.example", page_only),
            ("odd@odd.example", "odd@odd.example", useless),
            ("plain@example.com", "dana@example.com", ""),
        ] {
            s.append("INBOX", raw_with(message_id, from, extra), flags_seen(), None);
        }
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let both = stored(&env, "both@lists.example");
    assert!(
        !crate::mail::sync::ingest::is_pending(&both.pack),
        "the bodies pass has been, so the header outlived its rewrite of the row"
    );
    assert_eq!(
        both.unsubscribe,
        Some(ListUnsubscribe {
            links: vec![
                "mailto:leave@lists.example?subject=unsubscribe".into(),
                "https://lists.example/u/abc".into(),
            ],
            one_click: true,
            asked_at: None,
        })
    );

    let page = stored(&env, "page@shop.example").unsubscribe.expect("a page to open is kept");
    assert_eq!(page.links, vec!["https://shop.example/leave?u=1".to_string()]);
    assert!(!page.one_click, "no List-Unsubscribe-Post, so no one click");

    // Looked at and nothing offered is kept too, as an empty header: only
    // `None` sends a later look to the message's own bytes.
    let none = Some(ListUnsubscribe::default());
    assert_eq!(stored(&env, "odd@odd.example").unsubscribe, none, "nothing usable, kept as none");
    assert_eq!(stored(&env, "plain@example.com").unsubscribe, none, "no header at all, the same");
}

/// A message already stored is given its header when it is seen again --
/// here, filed in a second folder -- if, and only if, it kept none: a row
/// synced before headers were kept is filled in for free, and a kept one,
/// which may say when this person asked to leave the list, is never written
/// over by a fresh reading of the same header.
#[tokio::test]
async fn a_message_seen_again_is_given_its_header_only_when_it_kept_none() {
    let env = TestEnv::new();
    let server = plain_server();
    let raw = raw_with("again@lists.example", "news@lists.example", BOTH);
    {
        let mut s = server.lock().unwrap();
        s.append("INBOX", raw.clone(), flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let first = stored(&env, "again@lists.example");
    let read = first.unsubscribe.clone().expect("kept at ingest");

    // As it would be, synced by a build that kept no headers.
    env.vault.set_message_unsubscribe(first.id, None).unwrap();
    {
        let mut s = server.lock().unwrap();
        s.append("Archive", raw.clone(), flags_seen(), None);
    }
    env.sync(&mut session).await;
    let filled = stored(&env, "again@lists.example");
    assert_eq!(filled.id, first.id, "the same message, seen again rather than stored twice");
    assert_eq!(filled.unsubscribe.as_ref(), Some(&read), "filled in from the header seen again");

    // Asked, and the header it was asked from kept beside the moment.
    let asked = ListUnsubscribe {
        links: vec!["https://lists.example/u/older".into()],
        one_click: false,
        asked_at: Some(Timestamp::now()),
    };
    env.vault.set_message_unsubscribe(first.id, Some(asked.clone())).unwrap();
    {
        let mut s = server.lock().unwrap();
        s.append("Sent", raw, flags_seen(), None);
    }
    env.sync(&mut session).await;
    let kept = stored(&env, "again@lists.example");
    assert_eq!(kept.id, first.id);
    assert_eq!(kept.unsubscribe, Some(asked), "a kept header is never read over");
}
