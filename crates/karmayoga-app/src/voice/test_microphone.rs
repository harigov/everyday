//! A recording in place of the microphone, for testing a debug build on an
//! Android emulator.
//!
//! The emulator's microphone hears this machine's audio only now and then --
//! a switch that takes effect for a second or two and goes again -- so a
//! spoken turn cannot be tested by playing a clip at it. Instead: put 16 kHz
//! mono little-endian 16-bit samples at `test-microphone.raw` in the app's
//! data folder (`adb shell run-as app.karmayogi.KarmaYoga cp ... ./`), and
//! the next conversation hears them as what the microphone picked up, once,
//! from its first second; the file is deleted as it is read. Everything after
//! the microphone is the real thing: the connection, the computer's models
//! and assistant, the reply played on the phone's speaker.
//! `ui/scripts/phone/android-voice.mjs` does all of it.
//!
//! Only in a debug build, and only from the app's private storage, which
//! nothing but the app and `run-as` on a debuggable build can write.

use std::sync::Mutex;

const FILE: &str = "test-microphone.raw";

/// The recording, and how far into it the microphone has got.
static RECORDING: Mutex<Option<(Vec<i16>, usize)>> = Mutex::new(None);

/// What the microphone heard, or the recording's next stretch of the same
/// length while there is one.
pub fn replace(mut heard: Vec<i16>) -> Vec<i16> {
    let mut recording = RECORDING.lock().unwrap();
    if recording.is_none() {
        let path = karmayoga_vault::data_dir().join(FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            let _ = std::fs::remove_file(&path);
            let samples = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
            tracing::info!("voice: hearing {FILE} in place of the microphone");
            *recording = Some((samples, 0));
        }
    }
    let Some((samples, at)) = recording.as_mut() else { return heard };
    if *at >= samples.len() {
        *recording = None;
        return heard;
    }
    for sample in heard.iter_mut() {
        *sample = samples.get(*at).copied().unwrap_or(0);
        *at += 1;
    }
    heard
}
