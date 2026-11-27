//! The bot's room with its owner: finding the one it has, or making one.
//!
//! The owner may well have started the conversation themselves -- made a
//! direct chat in Element and invited the bot, which it accepted -- so the
//! bot looks before it builds. Only when there is no private *encrypted* room
//! with the owner in it, joined or waiting for them, does it create an
//! encrypted direct room, invite them, and say hello. A private room that is
//! not encrypted is not one the bot answers in -- what it said there would be
//! readable by the homeserver -- so it does not count, and is never where a
//! routine's result goes.

use super::errors::classify;
use super::rules::{self, RoomKind};
use crate::connector::{ChatError, ChatId};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::UserId;
use matrix_sdk::ruma::events::room::member::MembershipState;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::{Client, Room, RoomMemberships};
use std::time::Duration;

/// How many times to check that a room just made lists the owner before
/// giving up and speaking anyway.
const MEMBER_CHECKS: u32 = 4;

/// How long each of those checks lets a sync run, to carry the server's
/// account of the new room to the client.
const MEMBER_SYNC_PATIENCE: Duration = Duration::from_secs(2);

/// Who is in a room, as `classify_room` wants to be told.
pub(super) async fn kind_of(room: &Room, bot: &UserId, owner: &UserId) -> Option<RoomKind> {
    let members = match room.members(RoomMemberships::ACTIVE).await {
        Ok(members) => members,
        Err(error) => {
            tracing::warn!(room = %room.room_id(), "couldn't list a room's members: {error}");
            return None;
        }
    };
    let mut joined = Vec::new();
    let mut invited = Vec::new();
    for member in &members {
        match member.membership() {
            MembershipState::Join => joined.push(member.user_id().as_str()),
            MembershipState::Invite => invited.push(member.user_id().as_str()),
            _ => {}
        }
    }
    Some(rules::classify_room(bot.as_str(), owner.as_str(), &joined, &invited))
}

/// Whether `room` is encrypted, asking the server if the bot is not yet
/// sure. Fails closed: a room whose state cannot be found out is treated as
/// not encrypted, because what the bot writes in one that is not is readable
/// by the homeserver and by anybody it shows it to.
pub(super) async fn is_encrypted(room: &Room) -> bool {
    match room.latest_encryption_state().await {
        Ok(state) => state.is_encrypted(),
        Err(error) => {
            let room = room.room_id();
            tracing::warn!(%room, "couldn't tell whether a room is encrypted: {error}");
            false
        }
    }
}

/// The bot's private, encrypted room with its owner, once they are in it:
/// where a routine's message goes. The first such room, if somehow there are
/// two -- either one reaches only them.
pub(super) async fn home(client: &Client, bot: &UserId, owner: &UserId) -> Option<ChatId> {
    for room in client.joined_rooms() {
        if kind_of(&room, bot, owner).await == Some(RoomKind::Private) && is_encrypted(&room).await
        {
            return Some(ChatId(room.room_id().to_string()));
        }
    }
    None
}

/// What the bot has with its owner.
pub(super) struct Counterpart {
    /// Whether the owner has joined it. `false` means the bot invited them and
    /// is waiting.
    pub owner_joined: bool,
}

/// Find a private, encrypted room with the owner, or make one and greet them
/// in it.
///
/// `greet` is whether the greeting can be read by anybody: with no device of
/// the owner's trusted yet, the room's keys go to none, and a greeting in it
/// would be one nobody's app could open. It is left out then, the room made
/// all the same.
///
/// Call it after a sync, so that the rooms the client knows about are the
/// server's current ones.
pub(super) async fn ensure(
    client: &Client,
    bot: &UserId,
    owner: &UserId,
    assistant_name: &str,
    greet: bool,
) -> Result<Counterpart, ChatError> {
    let mut waiting = false;
    for room in client.joined_rooms() {
        match kind_of(&room, bot, owner).await {
            Some(RoomKind::Private) if is_encrypted(&room).await => {
                return Ok(Counterpart { owner_joined: true });
            }
            Some(RoomKind::Private) => {
                tracing::warn!(
                    room = %room.room_id(),
                    "a private room with the owner is not encrypted, so the bot will not use it"
                );
            }
            Some(RoomKind::AwaitingOwner) => waiting = true,
            Some(RoomKind::Shared) | None => {}
        }
    }
    if waiting {
        return Ok(Counterpart { owner_joined: false });
    }

    tracing::info!(owner = %owner, "making a private room for the owner");
    let room = client
        .create_dm(owner)
        .await
        .map_err(|e| classify(e.client_api_error_kind(), false, "make a room for the owner", &e))?;
    know_the_members(client, &room, owner).await;
    if !greet {
        tracing::info!("no device of the owner's is trusted yet, so the new room gets no greeting");
        return Ok(Counterpart { owner_joined: false });
    }
    // The greeting is a courtesy. The room is made and the owner invited
    // whether or not it goes out, and making a second room on the next start
    // because the first greeting failed would be worse than no greeting.
    if let Err(error) =
        room.send(RoomMessageEventContent::text_markdown(rules::greeting(assistant_name))).await
    {
        tracing::warn!("couldn't send the greeting: {error}");
    }
    Ok(Counterpart { owner_joined: false })
}

/// Make sure the client knows the owner is in `room` before anything is
/// encrypted for it.
///
/// A room made a moment ago exists on the server, with the owner invited, but
/// the client has not yet been told: its sync has not come round. Encrypting
/// then shares the message key with the members the client knows of -- nobody
/// -- and the owner is left with a greeting they can never read. Asking the
/// server for the member list puts the owner in the client's store; if the
/// server somehow does not list them yet, a short sync is given a few chances
/// to bring the room in. After that the bot speaks anyway: late and possibly
/// unreadable beats never.
async fn know_the_members(client: &Client, room: &Room, owner: &UserId) {
    for attempt in 1..=MEMBER_CHECKS {
        match room.members(RoomMemberships::ACTIVE).await {
            Ok(members) if members.iter().any(|m| m.user_id() == owner) => return,
            Ok(_) => tracing::debug!(attempt, "the new room does not list the owner yet"),
            Err(error) => tracing::debug!(attempt, "couldn't list the new room's members: {error}"),
        }
        if let Err(error) =
            client.sync_once(SyncSettings::new().timeout(MEMBER_SYNC_PATIENCE)).await
        {
            tracing::debug!(attempt, "sync while waiting for the new room: {error}");
        }
    }
    tracing::warn!("sending the greeting before the new room lists the owner");
}
