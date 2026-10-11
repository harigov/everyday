//! Turning what Matrix says went wrong into the two answers the supervisor
//! understands.
//!
//! [`ChatError::NeedsSignIn`] means "stop trying until somebody enters a
//! password in Settings"; [`ChatError::Transient`] means "try again later".
//! Choosing wrongly is expensive both ways: calling a dropped connection a
//! sign-in problem leaves the bot silent until a person notices, and calling
//! a refused password transient has it hammering the server with the wrong
//! one forever. So the choice is made in one place, by the server's own
//! error code rather than by guessing from message text, and it is a pure
//! function so it can be tested without a server.

use crate::connector::ChatError;
use matrix_sdk::ClientBuildError;
use matrix_sdk::ruma::api::error::ErrorKind;
use std::fmt::Display;

/// Classify a failed request.
///
/// `kind` is the Matrix error code, if the server sent one
/// (`error.client_api_error_kind()` on both `matrix_sdk::Error` and
/// `matrix_sdk::HttpError`). `signing_in` says whether the request was the
/// password login itself, because `M_FORBIDDEN` means "wrong password" there
/// and "you may not do that" everywhere else -- a refused message send must
/// not be taken for a dead sign-in. `doing` finishes the sentence "Couldn't
/// ..." for the transient case, and `error` is appended to it.
pub(crate) fn classify(
    kind: Option<&ErrorKind>,
    signing_in: bool,
    doing: &str,
    error: &dyn Display,
) -> ChatError {
    match kind {
        Some(ErrorKind::Forbidden) if signing_in => ChatError::NeedsSignIn(
            "The server refused the bot's password. Enter the right one in Settings to sign it in."
                .into(),
        ),
        Some(ErrorKind::UnknownToken(_) | ErrorKind::MissingToken) => {
            ChatError::NeedsSignIn(rejected_sign_in_message())
        }
        Some(ErrorKind::UserDeactivated) => {
            ChatError::NeedsSignIn("The server says the bot's account has been deactivated.".into())
        }
        Some(ErrorKind::UserLocked | ErrorKind::UserSuspended) => {
            ChatError::NeedsSignIn("The server has locked or suspended the bot's account.".into())
        }
        _ => ChatError::Transient(format!("Couldn't {doing}: {error}")),
    }
}

/// What to tell a person whose saved sign-in the server no longer accepts.
fn rejected_sign_in_message() -> String {
    "The server no longer accepts the bot's sign-in. Enter the bot's password in Settings to sign \
     it in again."
        .into()
}

/// Classify a failure to build the client.
///
/// Nothing here has touched the network yet (the homeserver address is used
/// exactly as given, with no discovery), so what can go wrong is the address
/// itself, which only a change in Settings fixes, or the local store failing
/// to open, which is worth another try.
pub(crate) fn classify_build(error: ClientBuildError) -> ChatError {
    match error {
        ClientBuildError::MissingHomeserver
        | ClientBuildError::InvalidServerName
        | ClientBuildError::WellKnownLookupDisabled
        | ClientBuildError::Url(_) => ChatError::NeedsSignIn(format!(
            "The homeserver address isn't one the bot can use ({error}). Check it in Settings."
        )),
        other => ChatError::Transient(format!("Couldn't open the bot's connection: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::api::error::UnknownTokenErrorData;

    fn classify_plain(kind: Option<&ErrorKind>, signing_in: bool) -> ChatError {
        classify(kind, signing_in, "reach the server", &"connection refused")
    }

    #[test]
    fn a_refused_password_needs_a_new_one() {
        let error = classify_plain(Some(&ErrorKind::Forbidden), true);
        assert!(matches!(&error, ChatError::NeedsSignIn(m) if m.contains("password")), "{error:?}");
    }

    #[test]
    fn forbidden_outside_sign_in_is_not_a_sign_in_problem() {
        // Being barred from a room says nothing about the bot's password.
        let error = classify_plain(Some(&ErrorKind::Forbidden), false);
        assert!(matches!(error, ChatError::Transient(_)));
    }

    #[test]
    fn a_token_the_server_forgot_needs_signing_in_again_wherever_it_shows_up() {
        let unknown = ErrorKind::UnknownToken(UnknownTokenErrorData::new());
        for signing_in in [true, false] {
            assert!(matches!(
                classify_plain(Some(&unknown), signing_in),
                ChatError::NeedsSignIn(_)
            ));
            assert!(matches!(
                classify_plain(Some(&ErrorKind::MissingToken), signing_in),
                ChatError::NeedsSignIn(_)
            ));
        }
    }

    #[test]
    fn a_deactivated_or_locked_account_is_not_retried() {
        for kind in [ErrorKind::UserDeactivated, ErrorKind::UserLocked, ErrorKind::UserSuspended] {
            assert!(matches!(classify_plain(Some(&kind), false), ChatError::NeedsSignIn(_)));
        }
    }

    #[test]
    fn everything_else_is_worth_trying_again() {
        assert!(matches!(classify_plain(None, true), ChatError::Transient(_)));
        assert!(matches!(
            classify_plain(Some(&ErrorKind::NotFound), false),
            ChatError::Transient(_)
        ));
        let error = classify_plain(None, false);
        assert!(matches!(&error, ChatError::Transient(m) if m.contains("connection refused")));
    }

    #[test]
    fn a_bad_homeserver_address_is_for_settings_to_fix() {
        // `reqwest` re-exports `url::Url`; the error type itself never needs naming.
        let parse = matrix_sdk::reqwest::Url::parse("not a url").unwrap_err();
        assert!(matches!(classify_build(ClientBuildError::Url(parse)), ChatError::NeedsSignIn(_)));
        assert!(matches!(
            classify_build(ClientBuildError::MissingHomeserver),
            ChatError::NeedsSignIn(_)
        ));
    }
}
