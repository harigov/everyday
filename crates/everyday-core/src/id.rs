//! Newtype wrappers around UUIDs so that a `JournalId` can never be passed
//! where an `EntryId` is expected.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const KIND: &'static str = $kind;

            /// UUIDv7 — time-ordered, so ids sort chronologically and index
            /// well in a B-tree without a separate sort column.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            pub fn parse(s: &str) -> Result<Self, uuid::Error> {
                Ok(Self(Uuid::parse_str(s)?))
            }

            /// A short, human-quotable form: the last eight hex digits.
            ///
            /// The *leading* digits of a UUIDv7 are a millisecond timestamp,
            /// so every id minted in the same period shares them -- a prefix
            /// makes a useless label and, worse, a colliding file name. The
            /// trailing digits are random, so they actually distinguish.
            pub fn short(&self) -> String {
                let hex = self.0.simple().to_string();
                hex[hex.len() - 8..].to_string()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", $kind, self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }
    };
}

typed_id!(JournalId, "journal");
typed_id!(EntryId, "entry");

// The task domain. Separate newtypes for the same reason as above: a
// `TaskId` and a `ProjectId` are both UUIDs and are never interchangeable,
// and a `BlockId` addresses a span of time rather than a thing to do.
typed_id!(ProjectId, "project");
typed_id!(TaskId, "task");
typed_id!(BlockId, "block");

// The calendar domain. A `CalendarId` names a subscription -- a feed you
// added -- and an `EventId` one occurrence read out of it. Events are minted
// fresh on every sync and are never referred to by anything else, so the id
// is an addressing detail rather than a durable name; the durable name is
// the event's `uid`, which comes from the publisher.
typed_id!(CalendarId, "calendar");
typed_id!(EventId, "event");

// The library domain. A `KindId` names a shelf -- Books, Films, the one you
// invented for wines -- an `ItemId` one thing on it, and a `LogId` one
// occasion on which you did something about that thing. Three newtypes for
// the same reason as everywhere above: they are all UUIDs and none of them
// is interchangeable with another.
typed_id!(KindId, "kind");
typed_id!(ItemId, "item");
typed_id!(LogId, "log");

// The tracking domain. A `TrackerId` names a thing you decided to record --
// a habit, a supplement, a symptom -- and lives in the journal's settings; a
// `ReadingId` names one recorded value. The split matters more here than
// elsewhere: readings outlive the definition being renamed, re-coloured or
// archived, which is what makes a year of "how much did I sleep" comparable
// with itself.
typed_id!(TrackerId, "tracker");
typed_id!(ReadingId, "reading");

// The assistant's domain. A `ConversationId` names a thread, a `MessageId`
// one turn in it, and a `MemoryId` one fact the assistant was asked to keep
// across all of them. The third is separate from the first two for the
// reason readings are separate from trackers: a memory outlives the
// conversation that produced it, and deleting a thread must not quietly
// retract what it taught.
typed_id!(ConversationId, "conversation");
typed_id!(MessageId, "message");
typed_id!(MemoryId, "memory");

/// Content address of an attachment payload.
///
/// Blobs are content-addressed with BLAKE3 so that the same photo dropped into
/// two entries is stored once, and so that a corrupted blob is detectable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobId(#[serde(with = "hex32")] pub [u8; 32]);

impl BlobId {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    pub fn parse(s: &str) -> Result<Self, crate::Error> {
        let s = s.trim();
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(crate::Error::Invalid(format!("{s:?} is not a blob id")));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16).unwrap() as u8;
            let lo = (chunk[1] as char).to_digit(16).unwrap() as u8;
            out[i] = hi << 4 | lo;
        }
        Ok(Self(out))
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blob({})", &self.to_hex()[..12])
    }
}

impl FromStr for BlobId {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::BlobId(*v).to_hex())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::BlobId::parse(&s).map(|b| b.0).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_id_round_trips_through_hex() {
        let id = BlobId::of(b"a photo of a cat");
        assert_eq!(BlobId::parse(&id.to_hex()).unwrap(), id);
        assert_eq!(id.to_hex().len(), 64);
    }

    #[test]
    fn blob_id_rejects_malformed_hex() {
        assert!(BlobId::parse("nope").is_err());
        assert!(BlobId::parse(&"z".repeat(64)).is_err());
    }

    #[test]
    fn short_ids_distinguish_ids_minted_together() {
        // The whole point: `id.to_string()[..8]` would be identical here.
        let ids: Vec<EntryId> = (0..64).map(|_| EntryId::new()).collect();
        let shorts: std::collections::BTreeSet<String> = ids.iter().map(EntryId::short).collect();
        assert_eq!(shorts.len(), ids.len(), "short ids collided");
        assert!(shorts.iter().all(|s| s.len() == 8));

        let prefixes: std::collections::BTreeSet<String> =
            ids.iter().map(|i| i.to_string()[..8].to_string()).collect();
        assert!(prefixes.len() < ids.len(), "prefixes were expected to collide");
    }

    #[test]
    fn typed_ids_are_time_ordered() {
        let a = EntryId::new();
        let b = EntryId::new();
        assert!(a < b, "uuidv7 ids should sort by creation time");
    }
}
