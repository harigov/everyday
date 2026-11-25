//! The volume, through PipeWire's `wpctl` or PulseAudio's `pactl`.
//!
//! Both are command-line tools the sound server ships, and what the
//! desktop's own volume slider is a front end to. Calling them is no more
//! than what a person typing `wpctl set-volume` would do, it needs no
//! library and no portal, and it works under every desktop that has either.
//! `wpctl` is preferred -- WirePlumber is what a current distribution runs --
//! and `pactl` is the answer on one that still runs PulseAudio, or on
//! PipeWire without WirePlumber's tool installed: `pipewire-pulse` speaks
//! it too.
//!
//! # Where the volume goes is worked out here, not by the tool
//!
//! A change is read, planned in [`plan`] and written as an absolute
//! percentage, never as the tools' own `10%+`. The tools disagree about
//! how a step past the top behaves (`wpctl` has a limit option, `pactl` has
//! nothing), and the plan is what has to hold on both: never past 100 --
//! these are somebody's speakers, not an amplifier -- never lower for being
//! asked for louder, and a muted computer asked to turn up is unmuted, as
//! its volume keys do. The volume read back afterwards is what is reported:
//! the model says what is so, not what it asked for.

use std::ffi::OsStr;

use karmayoga_service::desk::{Volume, VolumeChange};

/// How far up and down go when the person did not say how far.
const STEP: u32 = 10;

/// The most the assistant will set the volume to.
const CEILING: u32 = 100;

/// Which tool to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Wpctl,
    Pactl,
}

impl Tool {
    /// The first of the two that is on `search` (a `PATH`).
    fn found(search: &OsStr) -> Option<Tool> {
        if super::on_path("wpctl", search) {
            Some(Tool::Wpctl)
        } else if super::on_path("pactl", search) {
            Some(Tool::Pactl)
        } else {
            None
        }
    }

    fn program(self) -> &'static str {
        match self {
            Tool::Wpctl => "wpctl",
            Tool::Pactl => "pactl",
        }
    }
}

/// Is there a tool to change the volume with?
pub(in crate::desk) fn available() -> bool {
    std::env::var_os("PATH").is_some_and(|search| Tool::found(&search).is_some())
}

/// Change the volume and say where it is now. Blocking.
pub(in crate::desk) fn change(change: VolumeChange) -> Result<Volume, String> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    let tool = Tool::found(&search)
        .ok_or_else(|| "I can't change the volume on this computer.".to_string())?;
    let now = read(tool)?;
    let plan = plan(now, change);
    if plan.mute.is_none() && plan.percent.is_none() {
        return Ok(now);
    }
    // The level first and the mute after: an output is unmuted at the
    // volume that was asked for, not for a moment at the one it was left at.
    if let Some(percent) = plan.percent {
        write_percent(tool, percent)?;
    }
    if let Some(mute) = plan.mute {
        write_mute(tool, mute)?;
    }
    read(tool)
}

fn read(tool: Tool) -> Result<Volume, String> {
    match tool {
        Tool::Wpctl => {
            let ran = ask(tool, &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
            parse_wpctl(&ran).ok_or_else(|| unreadable(tool, &ran))
        }
        Tool::Pactl => {
            let volume = ask(tool, &["get-sink-volume", "@DEFAULT_SINK@"])?;
            let mute = ask(tool, &["get-sink-mute", "@DEFAULT_SINK@"])?;
            parse_pactl(&volume, &mute).ok_or_else(|| unreadable(tool, &volume))
        }
    }
}

/// Set the level to `percent`, as [`plan`] decided it: never raised past the
/// ceiling, but a level the person had set past it and a step down keeps
/// past it is written as it is, not pulled down to the ceiling.
fn write_percent(tool: Tool, percent: u32) -> Result<(), String> {
    let level = format!("{percent}%");
    match tool {
        // `-l 1.0` is wpctl's own ceiling, a belt for the plan's braces --
        // except for a level already past it, which it would cut.
        Tool::Wpctl if percent <= CEILING => {
            ask(tool, &["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &level]).map(drop)
        }
        Tool::Wpctl => ask(tool, &["set-volume", "@DEFAULT_AUDIO_SINK@", &level]).map(drop),
        Tool::Pactl => ask(tool, &["set-sink-volume", "@DEFAULT_SINK@", &level]).map(drop),
    }
}

fn write_mute(tool: Tool, mute: bool) -> Result<(), String> {
    let flag = if mute { "1" } else { "0" };
    match tool {
        Tool::Wpctl => ask(tool, &["set-mute", "@DEFAULT_AUDIO_SINK@", flag]).map(drop),
        Tool::Pactl => ask(tool, &["set-sink-mute", "@DEFAULT_SINK@", flag]).map(drop),
    }
}

/// Run the tool and give back what it printed, or the words to say that it
/// would not.
fn ask(tool: Tool, args: &[&str]) -> Result<String, String> {
    let ran = super::run(tool.program(), args).map_err(|e| {
        tracing::warn!(error = %e, "the volume could not be changed");
        "The sound system didn't answer.".to_string()
    })?;
    if ran.success {
        return Ok(ran.stdout);
    }
    tracing::warn!(tool = tool.program(), ?args, stderr = %ran.stderr.trim(), "volume tool failed");
    // `wpctl` says "Object ... not found" and `pactl` "No such entity" when
    // there is no output device to have a volume: headphones that are not
    // plugged into a machine with no speakers.
    Err("There's no sound output on this computer to change the volume of.".to_string())
}

fn unreadable(tool: Tool, text: &str) -> String {
    tracing::warn!(tool = tool.program(), text, "could not read the volume");
    "I couldn't tell what the volume is.".to_string()
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
/// - Up goes by the step, up to the ceiling, and never *down* -- an output
///   already past it (some are, by choice) is left where it is, with nothing
///   written. Muted, it is unmuted: asking for more sound is asking for
///   sound.
/// - Down goes by the step, to nothing at the least, and leaves the mute as
///   it was -- from past the ceiling too, by the step and not to it.
/// - Set is that percentage, up to the ceiling, unmuting for anything above
///   nothing.
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

/// `wpctl get-volume`'s answer: `Volume: 0.45` and, when muted, `Volume:
/// 0.45 [MUTED]`. A fraction of the nominal volume, so 1.0 is 100%.
fn parse_wpctl(text: &str) -> Option<Volume> {
    let line = text.lines().find(|line| line.trim_start().starts_with("Volume:"))?;
    let rest = line.trim_start().strip_prefix("Volume:")?;
    let fraction: f64 = rest.split_whitespace().next()?.parse().ok()?;
    if !fraction.is_finite() || fraction < 0.0 {
        return None;
    }
    Some(Volume { percent: (fraction * 100.0).round() as u32, muted: rest.contains("[MUTED]") })
}

/// `pactl get-sink-volume`'s answer and `pactl get-sink-mute`'s: `Volume:
/// front-left: 29491 /  45% / -20.00 dB, front-right: ...` and `Mute: no`.
/// The first channel's percentage is the volume: the balance between
/// channels is the person's own business, and a stereo pair at different
/// levels has no single one to report.
fn parse_pactl(volume: &str, mute: &str) -> Option<Volume> {
    let percent = volume
        .split(|c: char| c.is_whitespace() || c == '/' || c == ',')
        .find_map(|word| word.strip_suffix('%')?.parse::<u32>().ok())?;
    let muted = mute.lines().find_map(|line| line.trim().strip_prefix("Mute:"))?.trim() == "yes";
    Some(Volume { percent, muted })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(percent: u32, muted: bool) -> Volume {
        Volume { percent, muted }
    }

    #[test]
    fn wpctl_says_a_fraction_and_whether_it_is_muted() {
        assert_eq!(parse_wpctl("Volume: 0.45\n"), Some(volume(45, false)));
        assert_eq!(parse_wpctl("Volume: 0.45 [MUTED]\n"), Some(volume(45, true)));
        assert_eq!(parse_wpctl("Volume: 1.00"), Some(volume(100, false)));
        assert_eq!(parse_wpctl("Volume: 0.00 [MUTED]"), Some(volume(0, true)));
        // Past the nominal volume, where some people keep it.
        assert_eq!(parse_wpctl("Volume: 1.20"), Some(volume(120, false)));
        // 0.455 is 46, as the slider rounds.
        assert_eq!(parse_wpctl("Volume: 0.455"), Some(volume(46, false)));
    }

    #[test]
    fn wpctl_with_nothing_to_say_is_not_a_volume() {
        assert_eq!(parse_wpctl(""), None);
        assert_eq!(parse_wpctl("Object '@DEFAULT_AUDIO_SINK@' not found"), None);
        assert_eq!(parse_wpctl("Volume: loud"), None);
        assert_eq!(parse_wpctl("Volume: -0.5"), None);
    }

    #[test]
    fn pactl_says_a_percentage_for_each_channel() {
        let stereo = "Volume: front-left: 29491 /  45% / -20.00 dB,   front-right: 29491 /  45% / -20.00 dB\n        balance 0.00\n";
        assert_eq!(parse_pactl(stereo, "Mute: no\n"), Some(volume(45, false)));
        assert_eq!(parse_pactl(stereo, "Mute: yes\n"), Some(volume(45, true)));
        let mono = "Volume: mono: 65536 / 100% / 0.00 dB\n        balance 0.00\n";
        assert_eq!(parse_pactl(mono, "Mute: no"), Some(volume(100, false)));
        let over =
            "Volume: front-left: 78643 / 120% / 4.66 dB,   front-right: 78643 / 120% / 4.66 dB";
        assert_eq!(parse_pactl(over, "Mute: no"), Some(volume(120, false)));
    }

    #[test]
    fn pactl_with_nothing_to_say_is_not_a_volume() {
        assert_eq!(parse_pactl("", "Mute: no"), None);
        assert_eq!(parse_pactl("Volume: mono: 65536 / 100% / 0.00 dB", ""), None);
        assert_eq!(parse_pactl("No such entity", "No such entity"), None);
    }

    #[test]
    fn up_and_down_go_by_the_step_or_by_what_was_said() {
        let now = volume(45, false);
        assert_eq!(plan(now, VolumeChange::Up(None)).percent, Some(55));
        assert_eq!(plan(now, VolumeChange::Up(Some(20))).percent, Some(65));
        assert_eq!(plan(now, VolumeChange::Down(None)).percent, Some(35));
        assert_eq!(plan(now, VolumeChange::Down(Some(5))).percent, Some(40));
        assert_eq!(plan(now, VolumeChange::Up(None)).mute, None);
    }

    #[test]
    fn the_volume_stays_between_nothing_and_the_ceiling() {
        assert_eq!(plan(volume(95, false), VolumeChange::Up(None)).percent, Some(100));
        assert_eq!(plan(volume(100, false), VolumeChange::Up(Some(100))).percent, None);
        assert_eq!(plan(volume(5, false), VolumeChange::Down(None)).percent, Some(0));
        assert_eq!(plan(volume(0, false), VolumeChange::Down(Some(100))).percent, None);
        assert_eq!(plan(volume(50, false), VolumeChange::Set(100)).percent, Some(100));
        assert_eq!(plan(volume(50, false), VolumeChange::Set(200)).percent, Some(100));
        assert_eq!(plan(volume(50, false), VolumeChange::Set(0)).percent, Some(0));
    }

    /// An output the person has gone past 100 on is not turned *down* by
    /// being asked to turn up.
    #[test]
    fn up_never_lowers_what_is_already_past_the_ceiling() {
        assert_eq!(
            plan(volume(120, false), VolumeChange::Up(None)).percent,
            None,
            "louder never quieter"
        );
        assert_eq!(plan(volume(120, false), VolumeChange::Down(None)).percent, Some(110));
    }

    #[test]
    fn asking_for_more_sound_unmutes() {
        let muted = volume(40, true);
        assert_eq!(plan(muted, VolumeChange::Up(None)).mute, Some(false));
        assert_eq!(plan(muted, VolumeChange::Set(30)).mute, Some(false));
        // Silence asked for, and turning down: it stays as it is.
        assert_eq!(plan(muted, VolumeChange::Set(0)).mute, None);
        assert_eq!(plan(muted, VolumeChange::Down(None)).mute, None);
    }

    #[test]
    fn mute_and_unmute_change_only_the_mute() {
        let loud = volume(60, false);
        let quiet = volume(60, true);
        assert_eq!(plan(loud, VolumeChange::Mute), Plan { percent: None, mute: Some(true) });
        assert_eq!(plan(quiet, VolumeChange::Unmute), Plan { percent: None, mute: Some(false) });
        // Nothing to do to one that already is.
        assert_eq!(plan(quiet, VolumeChange::Mute), Plan { percent: None, mute: None });
        assert_eq!(plan(loud, VolumeChange::Unmute), Plan { percent: None, mute: None });
        assert_eq!(plan(loud, VolumeChange::Status), Plan { percent: None, mute: None });
    }
}
