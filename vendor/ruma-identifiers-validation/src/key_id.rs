use std::num::NonZeroU8;

use crate::{Error, KeyName};

pub fn validate<K: KeyName + ?Sized>(s: &str) -> Result<NonZeroU8, Error> {
    let colon_idx = s.find(':').ok_or(Error::MissingColon)?;
    if colon_idx == 0 {
        return Err(Error::MissingColon);
    }

    // Backported from 0.14.0 (RUSTSEC-2026-0589): the index was cast to `u8` before slicing, so an
    // algorithm longer than 255 bytes wrapped it and the slice could split a character and panic.
    K::validate(&s[colon_idx + 1..])?;

    // 0.14 returns `()` instead; nothing in ruma 0.17 reads the index (`KeyId` finds the colon
    // again), so past 255 it saturates rather than refusing what 0.14 accepts.
    Ok(u8::try_from(colon_idx).ok().and_then(NonZeroU8::new).unwrap_or(NonZeroU8::MAX))
}
