//! Authorization-code OAuth with PKCE, and the loopback that catches the
//! browser's answer -- the two halves of "sign in", with nothing about a
//! vault, an `Account` record, or where a refresh token is kept.
//!
//! # Why this crate, not `everyday-core`
//!
//! Every other module in `everyday-core` is synchronous: it can be called
//! from a test in a few microseconds and never opens a socket. OAuth cannot
//! be that -- exchanging a code and refreshing a token are HTTP requests --
//! so it lives beside [`crate::imap`] and [`crate::smtp`], the other two
//! places this tree talks to a server. What it does *not* do is decide where
//! a token is kept once it has one: [`Tokens`] is a value, handed back to
//! whoever asked, and the accounts domain's `save_account` is what claims it
//! into the vault's `SecretStore`. See `docs/plans/mail.md`'s "Phase 1".
//!
//! # The reqwest decision
//!
//! `oauth2` 5.x's own `reqwest` feature is convenient and wrong for this
//! tree: it pulls in reqwest 0.12, and the workspace is on 0.13 everywhere
//! else. Two copies of an HTTP stack in one binary is not a style
//! preference -- it is twice the TLS code to audit, twice the connection
//! pool, and a `cargo tree -d` that stops being a useful question to ask.
//!
//! The plan names two ways to avoid it: implement `oauth2`'s
//! [`AsyncHttpClient`](oauth2::AsyncHttpClient) trait over the workspace's
//! reqwest directly, or take `oauth2-reqwest`, the alpha adapter that exists
//! for exactly this version gap. This uses the adapter. Its entire public
//! surface is two newtypes -- `ReqwestClient` and, behind a feature this
//! crate does not enable, `ReqwestBlockingClient` -- wrapping a
//! `reqwest::Client` and forwarding to it; reading it start to finish took
//! less time than writing an equivalent impl would have, and every line of
//! it is one this crate would otherwise own and keep in step with `oauth2`'s
//! next breaking change itself. Being alpha is the honest cost, and it is
//! why the version below is pinned exactly rather than left to a caret: an
//! upgrade is a one-line review of a ninety-line crate, not a surprise.
//!
//! `oauth2`'s own `reqwest` (and `reqwest-blocking`, `native-tls`) features
//! are therefore switched off with `default-features = false`, so the only
//! `reqwest` in `Cargo.lock` is the workspace's 0.13 -- checked with
//! `cargo tree -d | grep reqwest` after every dependency change to this
//! crate, the same way a duplicate `serde` would be caught.
//!
//! # What never appears in a log
//!
//! An access token, a refresh token, a client secret and a PKCE verifier are
//! all bearer secrets: whoever holds the bytes can act as the account until
//! it is revoked, with no second factor to stop them. None of the four ever
//! reaches a `tracing` call in this module, and [`Tokens`] and
//! [`OAuthClient`] hand-write their own [`std::fmt::Debug`] rather than
//! derive it, so a stray `{:?}` in a log line prints `"[redacted]"` instead
//! of the thing that would let somebody read a mailbox. `oauth2`'s own
//! secret-carrying types (`AccessToken`, `RefreshToken`, `ClientSecret`,
//! `PkceCodeVerifier`) already redact themselves the same way, which is one
//! more reason to keep values in their typed form for as long as possible
//! rather than unwrapping to `String` early. `tests::debug_never_prints_a_secret`
//! is what keeps this true rather than merely believed.

mod client;
pub mod loopback;

pub use client::{Authorization, OAuthClient, OAuthError, Tokens, xoauth2_sasl};
pub use loopback::{Loopback, LoopbackError};
