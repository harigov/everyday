//! The volume, through `osascript`'s own `get volume settings`.
//!
//! `get volume settings` and `set volume` are StandardAdditions commands,
//! which `osascript` runs inside itself: unlike telling another application
//! what to do, they raise no Automation prompt, and they are what the
//! volume keys' own slider answers to. The tool says the output volume as a
//! whole number from 0 to 100 and whether the output is muted, and that is all
//! a Mac has: there is no loudness past 100 to be careful of.
//!
//! # Where the volume goes is worked out here
//!
//! As on Linux, a change is read, planned in [`plan`] and written as an
//! absolute percentage, and what is read back afterwards is what is
//! reported: never lower for being asked for louder, and a muted Mac asked to
//! turn up is unmuted, as its volume keys do. An output with no volume of its
//! own -- an HDMI monitor, a DAC that is turned by its own knob -- reports
//! `missing value`, and is said to be exactly that, in words.

use karmayoga_service::desk::{Volume, VolumeChange};

/// How far up and down go when the person did not say how far.
const STEP: u32 = 10;

/// The most the volume can be.
const CEILING: u32 = 100;

/// Change the volume and say where it is now. Blocking.
pub(in crate::desk) fn change(change: VolumeChange) -> Result<Volume, String> {
    let now = read()?;
    let plan = plan(now, change);
    if plan.mute.is_none() && plan.percent.is_none() {
        return Ok(now);
    }
    // The level first and the mute after: an output is unmuted at the volume
    // that was asked for, not for a moment at the one it was left at.
    if let Some(percent) = plan.percent {
        write(&format!("set volume output volume {percent}"))?;
    }
    if let Some(mute) = plan.mute {
        write(&format!("set volume output muted {mute}"))?;
    }
    read()
}

fn read() -> Result<Volume, String> {
    let ran = super::script("get volume settings").map_err(|e| {
        tracing::warn!(error = %e, "the volume could not be read");
        "The sound settings didn't answer.".to_string()
    })?;
    if !ran.success {
        tracing::warn!(stderr = %ran.stderr.trim(), "the volume could not be read");
        return Err("I couldn't tell what the volume is.".to_string());
    }
    parse(&ran.stdout).ok_or_else(|| {
        tracing::warn!(text = %ran.stdout.trim(), "this output has no volume to read");
        "The sound output the Mac is using has no volume of its own to change: its own \
         knob or buttons set the level."
            .to_string()
    })
}

fn write(line: &str) -> Result<(), String> {
    let ran = super::script(line).map_err(|e| {
        tracing::warn!(error = %e, "the volume could not be changed");
        "The sound settings didn't answer.".to_string()
    })?;
    if ran.success {
        return Ok(());
    }
    tracing::warn!(stderr = %ran.stderr.trim(), line, "the volume could not be changed");
    Err("The Mac wouldn't change the volume.".to_string())
}

/// What to do to the output, found from where it is and what was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    /// The percentage to set, if it is to change.
    percent: Option<u32>,
    /// Whether to mute or unmute, if that is to change.
    mute: Option<bool>,
}

/// What `change` asks, given `now`.
///
/// - Up goes by the step, to 100 at the most, and unmutes a muted output:
///   asking for more sound is asking for sound.
/// - Down goes by the step, to nothing at the least, and leaves the mute as
///   it was.
/// - Set is that percentage, up to 100, unmuting for anything above nothing.
/// - Mute and unmute change only the mute, and nothing is done to one that
///   already is.
fn plan(now: Volume, change: VolumeChange) -> Plan {
    let unmute = if now.muted { Some(false) } else { None };
    match change {
        VolumeChange::Up(by) => {
            let by = by.map_or(STEP, u32::from);
            let target = now.percent.saturating_add(by).min(CEILING);
            Plan { percent: (target > now.percent).then_some(target), mute: unmute }
        }
        VolumeChange::Down(by) => {
            let by = by.map_or(STEP, u32::from);
            let target = now.percent.saturating_sub(by);
            Plan { percent: (target < now.percent).then_some(target), mute: None }
        }
        VolumeChange::Set(to) => {
            let to = u32::from(to).min(CEILING);
            Plan { percent: Some(to), mute: if to > 0 { unmute } else { None } }
        }
        VolumeChange::Mute => Plan { percent: None, mute: (!now.muted).then_some(true) },
        VolumeChange::Unmute => Plan { percent: None, mute: now.muted.then_some(false) },
        VolumeChange::Status => Plan { percent: None, mute: None },
    }
}

/// `get volume settings`' answer: `output volume:81, input volume:69, alert
/// volume:100, output muted:false`. `None` for anything else, and for an
/// output volume that is `missing value`.
fn parse(text: &str) -> Option<Volume> {
    let field = |name: &str| {
        text.split(',').find_map(|part| part.trim().strip_prefix(name)?.strip_prefix(':'))
    };
    let percent: u32 = field("output volume")?.trim().parse().ok()?;
    let muted = match field("output muted")?.trim() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some(Volume { percent: percent.min(CEILING), muted })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(percent: u32, muted: bool) -> Volume {
        Volume { percent, muted }
    }

    #[test]
    fn osascript_says_the_level_and_whether_it_is_muted() {
        let said = "output volume:81, input volume:69, alert volume:100, output muted:false\n";
        assert_eq!(parse(said), Some(volume(81, false)));
        let muted = "output volume:0, input volume:69, alert volume:100, output muted:true";
        assert_eq!(parse(muted), Some(volume(0, true)));
    }

    #[test]
    fn an_output_with_no_volume_of_its_own_is_not_a_volume() {
        let hdmi = "output volume:missing value, input volume:69, alert volume:100, output muted:missing value";
        assert_eq!(parse(hdmi), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("execution error"), None);
    }

    #[test]
    fn up_and_down_go_by_a_step_and_stay_between_nothing_and_full() {
        let now = volume(50, false);
        assert_eq!(plan(now, VolumeChange::Up(None)).percent, Some(60));
        assert_eq!(plan(now, VolumeChange::Up(Some(20))).percent, Some(70));
        assert_eq!(plan(now, VolumeChange::Down(None)).percent, Some(40));
        assert_eq!(plan(volume(95, false), VolumeChange::Up(None)).percent, Some(100));
        assert_eq!(plan(volume(100, false), VolumeChange::Up(None)).percent, None);
        assert_eq!(plan(volume(5, false), VolumeChange::Down(None)).percent, Some(0));
        assert_eq!(plan(volume(0, false), VolumeChange::Down(None)).percent, None);
    }

    #[test]
    fn turning_up_unmutes_and_turning_down_does_not() {
        let muted = volume(30, true);
        assert_eq!(
            plan(muted, VolumeChange::Up(None)),
            Plan { percent: Some(40), mute: Some(false) }
        );
        assert_eq!(plan(muted, VolumeChange::Down(None)), Plan { percent: Some(20), mute: None });
        // Already at full and muted: only the mute changes.
        assert_eq!(
            plan(volume(100, true), VolumeChange::Up(None)),
            Plan { percent: None, mute: Some(false) }
        );
    }

    #[test]
    fn setting_a_level_unmutes_unless_it_is_nothing() {
        let muted = volume(30, true);
        assert_eq!(
            plan(muted, VolumeChange::Set(60)),
            Plan { percent: Some(60), mute: Some(false) }
        );
        assert_eq!(plan(muted, VolumeChange::Set(0)), Plan { percent: Some(0), mute: None });
        assert_eq!(plan(volume(30, false), VolumeChange::Set(255)).percent, Some(100));
    }

    #[test]
    fn mute_and_unmute_touch_only_the_mute_and_only_when_it_changes() {
        assert_eq!(
            plan(volume(30, false), VolumeChange::Mute),
            Plan { percent: None, mute: Some(true) }
        );
        assert_eq!(plan(volume(30, true), VolumeChange::Mute), Plan { percent: None, mute: None });
        assert_eq!(
            plan(volume(30, true), VolumeChange::Unmute),
            Plan { percent: None, mute: Some(false) }
        );
        assert_eq!(
            plan(volume(30, false), VolumeChange::Unmute),
            Plan { percent: None, mute: None }
        );
        assert_eq!(
            plan(volume(30, true), VolumeChange::Status),
            Plan { percent: None, mute: None }
        );
    }

    /// The real osascript on this Mac, changing nothing anybody would hear:
    /// the level is read, then set to what it already was.
    #[test]
    #[ignore = "uses this Mac's own volume; run by hand with --ignored"]
    fn live_the_volume_is_read_and_set_where_it_was() {
        let before = change(VolumeChange::Status).expect("the volume was read");
        let level = u8::try_from(before.percent.min(100)).unwrap();
        let after = change(VolumeChange::Set(level)).expect("the volume was set");
        assert_eq!(after.percent, before.percent);
    }
}
