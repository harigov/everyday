//! The iPhone's audio session, which an app has to set before it may record.
//!
//! iOS starts every app in the `SoloAmbient` category: it plays, and it does
//! not record. cpal opens the microphone through RemoteIO and follows the
//! session's interruptions, but it never chooses a category -- that is the
//! app's to decide -- so without this, talking to the assistant on an iPhone
//! opened a microphone that heard nothing. Android has no such step; its
//! permission is all (`MainActivity.kt`).
//!
//! So while an [`super::engine::AudioEngine`] runs, the session is
//! `PlayAndRecord` (or `Playback`, for an engine with only a speaker), on the
//! loudspeaker rather than the earpiece and allowed onto a Bluetooth headset,
//! in the default mode: the echo is cancelled here, by `sonora`, and Apple's
//! voice-chat processing on top of it would be a second canceller fighting the
//! first. When the last engine stops the session is deactivated with the
//! option that tells other apps -- the music it interrupted -- that they may
//! carry on.

use std::ptr::null_mut;
use std::sync::Mutex;

use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};

// `AVAudioSessionCategoryOptions`.
const DEFAULT_TO_SPEAKER: usize = 0x8;
const ALLOW_BLUETOOTH_HFP: usize = 0x4;
const ALLOW_BLUETOOTH_A2DP: usize = 0x20;
// `AVAudioSessionSetActiveOptionNotifyOthersOnDeactivation`.
const NOTIFY_OTHERS_ON_DEACTIVATION: usize = 1;

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {
    static AVAudioSessionCategoryPlayAndRecord: *const AnyObject;
    static AVAudioSessionCategoryPlayback: *const AnyObject;
    static AVAudioSessionModeDefault: *const AnyObject;
}

/// How many engines hold the session.
static HELD: Mutex<usize> = Mutex::new(0);

/// The session, held: taken by an engine as it starts, given back as it is
/// dropped -- a start that fails included.
pub struct Hold(());

impl Hold {
    pub fn take(microphone: bool) -> Hold {
        begin(microphone);
        Hold(())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        end();
    }
}

/// Take the session for an engine: set the category it needs and make it
/// active. A failure is logged and not returned: the device opening after
/// this says what is wrong in terms cpal knows, and a speaker can play in
/// whatever category the session was left in.
fn begin(microphone: bool) {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    *held += 1;
    // SAFETY: AVAudioSession is safe to use from any thread; the constants
    // are NSStrings the framework owns for the life of the process, and the
    // error out-parameter is a local that outlives each call.
    unsafe {
        let session: *mut AnyObject = msg_send![class!(AVAudioSession), sharedInstance];
        if session.is_null() {
            tracing::warn!("audio: no audio session");
            return;
        }
        let (category, options) = if microphone {
            (
                AVAudioSessionCategoryPlayAndRecord,
                DEFAULT_TO_SPEAKER | ALLOW_BLUETOOTH_HFP | ALLOW_BLUETOOTH_A2DP,
            )
        } else {
            (AVAudioSessionCategoryPlayback, 0)
        };
        let mut error: *mut AnyObject = null_mut();
        let set: Bool = msg_send![
            session,
            setCategory: category,
            mode: AVAudioSessionModeDefault,
            options: options,
            error: &mut error as *mut *mut AnyObject
        ];
        if !set.as_bool() {
            tracing::warn!(microphone, "audio: the session's category could not be set");
        }
        let mut error: *mut AnyObject = null_mut();
        let active: Bool =
            msg_send![session, setActive: Bool::YES, error: &mut error as *mut *mut AnyObject];
        if !active.as_bool() {
            tracing::warn!("audio: the session could not be made active");
        }
    }
}

/// Give the session back when an engine stops; the last one deactivates it.
fn end() {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    *held = held.saturating_sub(1);
    if *held > 0 {
        return;
    }
    // SAFETY: as in `begin`.
    unsafe {
        let session: *mut AnyObject = msg_send![class!(AVAudioSession), sharedInstance];
        if session.is_null() {
            return;
        }
        let mut error: *mut AnyObject = null_mut();
        let _: Bool = msg_send![
            session,
            setActive: Bool::NO,
            withOptions: NOTIFY_OTHERS_ON_DEACTIVATION,
            error: &mut error as *mut *mut AnyObject
        ];
    }
}
