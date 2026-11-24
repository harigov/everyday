//! A script said aloud into an audio file in the files folder: an article to
//! listen to, the daily podcast. See `docs/plans/podcasts.md` (phase 2) and
//! `karmayoga_core::news::script`.
//!
//! Each line goes to its speaker's voice ([`crate::speech::synth`]) a few
//! sentences at a time: the splitter that cuts a reply for voice mode cuts
//! each line, the last sentence of a line is marked as ending it, and as many
//! sentences in a row as fit in one clip are said together
//! ([`joins`](crate::speech::text::joins)), so the voice carries one on into
//! the next. The voice's [`Delivery`](crate::speech::synth) leaves the pause
//! after each clip that its ending asks for -- a beat at a full stop, longer
//! where a paragraph or an item of a list ends. A change of speaker is given
//! a little more. What comes back is written as
//! Opus in an Ogg file -- a few megabytes for a quarter of an hour -- with the
//! `speech` feature, or WAV without it, and kept in the files folder.
//!
//! # What a script is said in
//!
//! The configured voice (Settings → Models), at the pace chosen there. A
//! [`ScriptSpeaker`](karmayoga_core::news::script::ScriptSpeaker) that names a speaker of
//! the voice on this computer is said by that speaker when the configured
//! voice is one on this computer that has it ([`speaker_number`]); any other
//! speaker, and every speaker when the voice is a remote one, is the
//! configured voice. So a script with two hosts is two voices where there
//! are two to be had, and one voice where there is not.
//!
//! Everything said is [`inert`](crate::speech::script::inert): a script is
//! written from somebody else's words, and a `{word|respelling}` in it is
//! only text. The person's own pronunciations apply as they do everywhere.
//!
//! # Encoding
//!
//! A [`Recorder`] takes samples as they are made and keeps only what has been
//! encoded, so an hour of speech is tens of megabytes of Opus and not
//! hundreds of PCM. [`encode`] is the same over samples already in hand. Opus
//! takes 8, 12, 16, 24 and 48 kHz; a voice at another rate is made 48 kHz
//! ([`recording_rate`]).

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use crate::speech::pcm;
use crate::speech::resample;
use crate::speech::script::{self, LINE_END};
use crate::speech::synth::{Speech, Synthesiser, build_synthesiser_with};
use crate::speech::text::{MAX_CLIP, joins, sentences};
use karmayoga_core::FileEntry;
use karmayoga_core::files::{FileOrigin, ImportOptions, MediaInfo};
use karmayoga_core::news::script::Script;
use karmayoga_core::speech::SynthesiserConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Said after a line when the next is another speaker's, over and above the
/// pause that ended the line: the beat a person leaves before answering.
const SPEAKER_CHANGE: Duration = Duration::from_millis(250);

/// How long to wait before a sentence the voice could not make is asked for
/// again. Once: a voice that fails twice is not going to say it.
const RETRY_AFTER: Duration = Duration::from_millis(500);

/// Say `script` and keep it as `<dir>/<name>.<ext>` in the files folder
/// (origin Feed), answering the file. `progress` hears the fraction done, by
/// how much of the text has been said.
///
/// `dir` is relative to the files folder and `name` has no extension, which
/// is the format's to give: `opus` with the `speech` feature, `wav` without.
/// The file is left for the caller to link and announce. Refuses, in a
/// sentence, a vault another copy has for writing, a voice that is not set up
/// or not downloaded, and a script with nothing in it to say. A caller that
/// stops waiting for this -- the task was told to stop -- drops it, and
/// nothing is written.
pub async fn render_script(
    svc: &Arc<Service>,
    script: &Script,
    dir: &str,
    name: &str,
    progress: &(dyn Fn(f32) + Send + Sync),
) -> CommandResult<FileEntry> {
    let vault = svc.require_unlocked()?;
    if !vault.is_writable() {
        return Err(CommandError::new(
            codes::VAULT_IN_USE,
            "another copy of Karma Yoga has this vault open for writing, so nothing can be \
             saved here.",
        ));
    }
    let voices = Voices::for_script(svc, script).await?;
    let recorded = record(&voices, script, progress).await?;
    store(svc, recorded, dir, name).await
}

// ---- the voices ---------------------------------------------------------------

/// Who says what: a voice for each speaker that has one of its own, and the
/// configured voice for every other.
struct Voices {
    configured: Arc<dyn Synthesiser>,
    own: HashMap<String, Arc<dyn Synthesiser>>,
}

impl Voices {
    /// The voices `script` is said in. The configured voice is built first,
    /// so that what is missing is said in the words every other feature uses.
    async fn for_script(svc: &Arc<Service>, script: &Script) -> CommandResult<Self> {
        let configured = svc.speech_synthesiser().await?;
        let mut voices = Self { configured, own: HashMap::new() };
        if script.speakers.iter().all(|s| s.voice.is_none()) {
            return Ok(voices);
        }
        let vault = svc.require_unlocked()?;
        let access = blocking(move || Ok(vault.voice_access()?)).await?;
        let book = svc.pronunciations();
        book.set(&access.pronunciations);
        // A speaker is made once however many say it.
        let mut made: HashMap<u32, Arc<dyn Synthesiser>> = HashMap::new();
        for speaker in &script.speakers {
            let Some(number) = speaker_number(&access.config.synthesiser, speaker.voice.as_deref())
            else {
                continue;
            };
            let SynthesiserConfig::Local { voice, .. } = access.config.synthesiser else {
                continue;
            };
            let said_by = match made.get(&number) {
                Some(said_by) => said_by.clone(),
                None => {
                    let mut own = access.clone();
                    own.config.synthesiser = SynthesiserConfig::Local { voice, speaker: number };
                    let said_by = build_synthesiser_with(own, book.clone()).await?;
                    made.insert(number, said_by.clone());
                    said_by
                }
            };
            voices.own.insert(speaker.id.clone(), said_by);
        }
        Ok(voices)
    }

    fn of(&self, speaker: &str) -> &dyn Synthesiser {
        self.own.get(speaker).unwrap_or(&self.configured).as_ref()
    }
}

/// The speaker of the configured voice that a script's speaker asks for, when
/// it asks for one the voice has and it is not the one already configured:
/// the voice on this computer's speaker by that name, in any case. `None` is
/// the configured voice -- nothing asked for, a remote voice, or a speaker
/// this voice does not have.
pub fn speaker_number(configured: &SynthesiserConfig, wanted: Option<&str>) -> Option<u32> {
    let wanted = wanted?.trim();
    let SynthesiserConfig::Local { voice, speaker } = configured else { return None };
    let at = voice.speaker_names().iter().position(|n| n.eq_ignore_ascii_case(wanted))?;
    let at = u32::try_from(at).ok()?;
    (at != *speaker).then_some(at)
}

// ---- saying -------------------------------------------------------------------

/// A line as the sentences a voice is handed: cut and made speakable as
/// Listen's are, the last marked as ending a line.
fn line_sentences(text: &str) -> Vec<String> {
    let mut said = sentences(&script::inert(text));
    if let Some(last) = said.last_mut()
        && !last.trim_end().ends_with(LINE_END)
    {
        last.push(LINE_END);
    }
    said
}

/// One sentence said. `None` when the voice has nothing to say for it -- a
/// sentence of nothing but a symbol, or one a server refuses -- which is
/// skipped, as Listen skips it. Anything else that goes wrong is asked again
/// once, after a moment, and then is the render's failure: half an article
/// is not what was asked for.
async fn say(voice: &dyn Synthesiser, sentence: &str) -> CommandResult<Option<Speech>> {
    let mut retried = false;
    loop {
        match voice.synthesise(sentence).await {
            Ok(speech) if speech.samples.is_empty() || speech.sample_rate == 0 => return Ok(None),
            Ok(speech) => return Ok(Some(speech)),
            Err(e) if e.code == codes::INVALID => return Ok(None),
            Err(e) if !retried => {
                tracing::debug!(error = %e, "render: a sentence could not be said; trying again");
                retried = true;
                tokio::time::sleep(RETRY_AFTER).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A line's sentences in the clips they are said in: as many in a row as fit
/// in [`MAX_CLIP`], never past the end of a paragraph or an item of a list.
/// Nothing is waiting to hear the first, so every clip is as long as it may
/// be.
fn clips(said: &[String]) -> Vec<&[String]> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < said.len() {
        let mut end = start + 1;
        let mut len = said[start].chars().count();
        while end < said.len() && joins(&said[end - 1], len, &said[end], MAX_CLIP) {
            len += 1 + said[end].chars().count();
            end += 1;
        }
        out.push(&said[start..end]);
        start = end;
    }
    out
}

/// A clip said: its sentences together, or, when the voice will not say them
/// together, each on its own, as [`say`] says one.
async fn say_clip(voice: &dyn Synthesiser, clip: &[String]) -> CommandResult<Vec<Speech>> {
    if let [sentence] = clip {
        return Ok(say(voice, sentence).await?.into_iter().collect());
    }
    match say(voice, &clip.join(" ")).await {
        Ok(Some(speech)) => return Ok(vec![speech]),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!(error = %e, "render: sentences said together failed; saying them one at a time")
        }
    }
    let mut out = Vec::new();
    for sentence in clip {
        out.extend(say(voice, sentence).await?);
    }
    Ok(out)
}

/// Everything `script` says, in order, recorded.
async fn record(
    voices: &Voices,
    script: &Script,
    progress: &(dyn Fn(f32) + Send + Sync),
) -> CommandResult<Encoded> {
    let lines: Vec<(&str, Vec<String>)> = script
        .lines
        .iter()
        .map(|l| (l.speaker.as_str(), line_sentences(&l.text)))
        .filter(|(_, said)| !said.is_empty())
        .collect();
    // Progress is by how much text has been said, which is how long it takes.
    let total: usize = lines.iter().flat_map(|(_, s)| s).map(|s| weight(s)).sum::<usize>().max(1);
    let mut done = 0;
    let mut recorder: Option<Recorder> = None;
    let mut last_speaker: Option<&str> = None;
    for (speaker, said) in &lines {
        let voice = voices.of(speaker);
        for clip in clips(said) {
            for speech in say_clip(voice, clip).await? {
                let recorder = match &mut recorder {
                    Some(recorder) => recorder,
                    None => recorder
                        .insert(Recorder::new(recording_rate(speech.sample_rate), &script.title)?),
                };
                let samples = at_rate(speech, recorder.rate())?;
                // The beat before an answer comes ahead of the answer, after
                // the pause that ended what was said before it.
                if last_speaker.is_some_and(|last| last != *speaker) {
                    recorder.push(&silence(recorder.rate(), SPEAKER_CHANGE))?;
                }
                recorder.push(&samples)?;
                last_speaker = Some(speaker);
            }
            done += clip.iter().map(|s| weight(s)).sum::<usize>();
            progress((done as f32 / total as f32).min(1.0));
        }
    }
    recorder
        .ok_or_else(|| CommandError::new(codes::INVALID, "there is nothing in it to say"))?
        .finish()
}

/// How much saying a sentence is, to count progress by.
fn weight(sentence: &str) -> usize {
    sentence.chars().filter(|&c| c != LINE_END).count().max(1)
}

/// `speech` as samples at `rate`.
fn at_rate(speech: Speech, rate: u32) -> CommandResult<Vec<i16>> {
    if speech.sample_rate == rate {
        return Ok(speech.samples);
    }
    let converted = resample::clip(&pcm::to_f32(&speech.samples), speech.sample_rate, rate)
        .map_err(|e| {
            CommandError::new(codes::INTERNAL, format!("could not convert a voice's audio: {e}"))
        })?;
    Ok(pcm::to_i16(&converted))
}

fn silence(rate: u32, length: Duration) -> Vec<i16> {
    vec![0; (length.as_secs_f64() * f64::from(rate)).round() as usize]
}

// ---- keeping ------------------------------------------------------------------

/// A recording written out as a file's bytes.
pub struct Encoded {
    pub bytes: Vec<u8>,
    /// What the file is called after its name: `opus` or `wav`.
    pub extension: &'static str,
    pub duration_ms: u64,
}

/// Put `recorded` in the files folder as `<dir>/<name>.<extension>`.
///
/// Its length is written into the file's media information here, because the
/// folder's own reader only knows the lengths of MP4 films and a recording
/// that has none cannot be shown with how long it takes. The file is not
/// handed to that reader for the same reason: what it learned would be
/// nothing, and would replace this.
async fn store(
    svc: &Arc<Service>,
    recorded: Encoded,
    dir: &str,
    name: &str,
) -> CommandResult<FileEntry> {
    let vault = svc.require_unlocked()?;
    let file_name = format!("{name}.{}", recorded.extension);
    let options = ImportOptions {
        dir: Some(dir.to_string()),
        name: Some(file_name.clone()),
        origin: FileOrigin::Feed,
        encrypted: None,
    };
    blocking(move || {
        let file = vault.import_bytes(&recorded.bytes, &file_name, &options)?;
        let media = MediaInfo { duration_ms: Some(recorded.duration_ms), ..file.media.clone() };
        Ok(vault.save_file_derived(file.id, media, None, None)?)
    })
    .await
}

/// The rate a recording is made at for a voice that speaks at `voice_rate`:
/// the voice's own when the format takes it, else what the format takes best.
pub fn recording_rate(voice_rate: u32) -> u32 {
    #[cfg(feature = "speech")]
    {
        if opus_file::RATES.contains(&voice_rate) { voice_rate } else { opus_file::FALLBACK_RATE }
    }
    #[cfg(not(feature = "speech"))]
    {
        voice_rate
    }
}

/// Mono 16-bit samples as they are kept, taken a stretch at a time so that
/// only what is still to be encoded is held: Opus in an Ogg file with the
/// `speech` feature, WAV without it.
pub struct Recorder {
    sink: Sink,
    rate: u32,
    samples: u64,
}

impl Recorder {
    /// A recording at `rate`, which is [`recording_rate`] of a voice's own,
    /// titled `title` where the format has a place for one.
    pub fn new(rate: u32, title: &str) -> CommandResult<Self> {
        Ok(Self { sink: Sink::new(rate, title)?, rate, samples: 0 })
    }

    /// The rate the samples pushed must be at.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn push(&mut self, samples: &[i16]) -> CommandResult<()> {
        self.samples += samples.len() as u64;
        self.sink.push(samples)
    }

    /// The file, once everything is pushed. Refuses an empty recording.
    pub fn finish(self) -> CommandResult<Encoded> {
        if self.samples == 0 {
            return Err(CommandError::new(codes::INVALID, "there is nothing in it to say"));
        }
        let duration_ms = self.samples * 1000 / u64::from(self.rate.max(1));
        Ok(Encoded { bytes: self.sink.finish()?, extension: Sink::EXTENSION, duration_ms })
    }
}

/// `samples` at `rate` as the file they are kept in: a pure function over
/// the samples, which is [`Recorder`] given them all at once. A rate the
/// format does not take is converted first.
pub fn encode(samples: &[i16], rate: u32, title: &str) -> CommandResult<Encoded> {
    let target = recording_rate(rate);
    let samples = at_rate(Speech { samples: samples.to_vec(), sample_rate: rate }, target)?;
    let mut recorder = Recorder::new(target, title)?;
    recorder.push(&samples)?;
    recorder.finish()
}

#[cfg(feature = "speech")]
use opus_file::OpusFile as Sink;
#[cfg(not(feature = "speech"))]
use wav_file::WavFile as Sink;

/// WAV: every sample, held until the end, since the header says how many
/// there are. Only where Opus is not built.
#[cfg(not(feature = "speech"))]
mod wav_file {
    use super::*;
    use crate::speech::wav;

    pub struct WavFile {
        samples: Vec<i16>,
        rate: u32,
    }

    impl WavFile {
        pub const EXTENSION: &'static str = "wav";

        pub fn new(rate: u32, _title: &str) -> CommandResult<Self> {
            Ok(Self { samples: Vec::new(), rate })
        }

        pub fn push(&mut self, samples: &[i16]) -> CommandResult<()> {
            self.samples.extend_from_slice(samples);
            Ok(())
        }

        pub fn finish(self) -> CommandResult<Vec<u8>> {
            Ok(wav::encode(&self.samples, self.rate))
        }
    }
}

/// Opus in an Ogg file, as RFC 7845 has it.
///
/// Three things in it are easy to get wrong, and a player that seeks or
/// reports a length depends on each. The head carries the encoder's
/// look-ahead as `pre-skip`, so that the first sound is not shifted by the
/// codec's own delay. Every position is counted in 48 kHz samples whatever
/// rate was encoded. And the last position is the end of the real sound, not
/// of the last frame, which is padded: the decoder is told how much of the
/// end to leave off. The stream is ended by its last packet, which is why
/// each packet is held back until the next arrives.
#[cfg(feature = "speech")]
mod opus_file {
    use super::*;
    use ogg::{PacketWriteEndInfo, PacketWriter};
    use opus::{Application, Bitrate, Channels, Encoder, Signal};

    /// What Opus takes.
    pub const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
    /// What a voice at any other rate is made.
    pub const FALLBACK_RATE: u32 = 48_000;

    /// Speech is clean from 24 kbit/s; a synthetic voice's sibilants are
    /// better for a little more. A quarter of an hour is under four megabytes.
    const BITRATE: i32 = 32_000;
    /// What one packet can be at most: more than any frame at this bitrate.
    const MAX_PACKET: usize = 4_000;
    /// Frames are 20 ms, which is 960 samples at the rate positions count in.
    const FRAME_UNITS: u64 = 960;
    /// A page about every second, so that a player can seek to within one.
    const PAGE_PACKETS: u64 = 50;
    /// One stream to a file, so any serial will do.
    const SERIAL: u32 = 0x4B59_0001;
    /// The longest title kept, in bytes.
    const TITLE_BYTES: usize = 1_000;

    pub struct OpusFile {
        encoder: Encoder,
        writer: PacketWriter<'static, Vec<u8>>,
        /// Samples in a frame at the rate it was made at.
        frame: usize,
        /// 48 kHz samples to one at `rate`.
        ratio: u64,
        /// The encoder's delay, in 48 kHz samples.
        pre_skip: u64,
        /// Samples not yet enough for a frame.
        pending: Vec<i16>,
        /// Samples pushed, at `rate`.
        pushed: u64,
        /// Packets made.
        made: u64,
        /// Packets written.
        written: u64,
        /// The newest packet, kept back until another comes after it.
        held: Option<Vec<u8>>,
        scratch: Vec<u8>,
    }

    fn failed(what: &str, e: impl std::fmt::Display) -> CommandError {
        CommandError::new(codes::INTERNAL, format!("could not write the audio ({what}): {e}"))
    }

    impl OpusFile {
        pub const EXTENSION: &'static str = "opus";

        pub fn new(rate: u32, title: &str) -> CommandResult<Self> {
            if !RATES.contains(&rate) {
                return Err(failed("sample rate", format!("Opus does not take {rate} Hz")));
            }
            let mut encoder = Encoder::new(rate, Channels::Mono, Application::Voip)
                .map_err(|e| failed("encoder", e))?;
            encoder.set_bitrate(Bitrate::Bits(BITRATE)).map_err(|e| failed("bitrate", e))?;
            encoder.set_signal(Signal::Voice).map_err(|e| failed("signal", e))?;
            let ratio = u64::from(48_000 / rate);
            let lookahead = encoder.get_lookahead().map_err(|e| failed("look-ahead", e))?;
            let pre_skip = u64::try_from(lookahead).unwrap_or(0) * ratio;
            let mut writer = PacketWriter::new(Vec::new());
            writer
                .write_packet(head(rate, pre_skip), SERIAL, PacketWriteEndInfo::EndPage, 0)
                .and_then(|()| {
                    writer.write_packet(tags(title), SERIAL, PacketWriteEndInfo::EndPage, 0)
                })
                .map_err(|e| failed("header", e))?;
            Ok(Self {
                encoder,
                writer,
                frame: (rate / 50) as usize,
                ratio,
                pre_skip,
                pending: Vec::new(),
                pushed: 0,
                made: 0,
                written: 0,
                held: None,
                scratch: vec![0; MAX_PACKET],
            })
        }

        pub fn push(&mut self, samples: &[i16]) -> CommandResult<()> {
            self.pushed += samples.len() as u64;
            self.pending.extend_from_slice(samples);
            self.encode_whole_frames()
        }

        /// Everything pending that fills a frame, encoded; the rest stays.
        fn encode_whole_frames(&mut self) -> CommandResult<()> {
            let pending = std::mem::take(&mut self.pending);
            let mut chunks = pending.chunks_exact(self.frame);
            for frame in &mut chunks {
                self.encode(frame)?;
            }
            self.pending = chunks.remainder().to_vec();
            Ok(())
        }

        fn encode(&mut self, frame: &[i16]) -> CommandResult<()> {
            let len =
                self.encoder.encode(frame, &mut self.scratch).map_err(|e| failed("encoding", e))?;
            self.made += 1;
            if let Some(held) = self.held.replace(self.scratch[..len].to_vec()) {
                self.write(held, false)?;
            }
            Ok(())
        }

        /// Write a packet, which is the one after those already written.
        /// `last` ends the stream and ends it where the sound does.
        fn write(&mut self, packet: Vec<u8>, last: bool) -> CommandResult<()> {
            self.written += 1;
            let (end, position) = if last {
                (PacketWriteEndInfo::EndStream, self.pre_skip + self.pushed * self.ratio)
            } else {
                let end = if self.written.is_multiple_of(PAGE_PACKETS) {
                    PacketWriteEndInfo::EndPage
                } else {
                    PacketWriteEndInfo::NormalPacket
                };
                (end, self.pre_skip + self.written * FRAME_UNITS)
            };
            self.writer.write_packet(packet, SERIAL, end, position).map_err(|e| failed("page", e))
        }

        pub fn finish(mut self) -> CommandResult<Vec<u8>> {
            // Silence after the sound, as far as the encoder's delay: what
            // it still holds of the last of it comes out in these frames.
            let lookahead = self.pre_skip / self.ratio;
            let frames = (self.pushed + lookahead).div_ceil(self.frame as u64);
            let padding = frames * self.frame as u64 - self.pushed;
            self.pending.resize(self.pending.len() + padding as usize, 0);
            self.encode_whole_frames()?;
            let last = self.held.take().ok_or_else(|| failed("stream", "no audio was made"))?;
            self.write(last, true)?;
            debug_assert_eq!(self.made, self.written);
            Ok(self.writer.into_inner())
        }
    }

    /// The identification header, which comes first and alone on its page.
    fn head(rate: u32, pre_skip: u64) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&u16::try_from(pre_skip).unwrap_or(u16::MAX).to_le_bytes());
        head.extend_from_slice(&rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family: mono and stereo
        head
    }

    /// The comment header: who wrote it, and what it is called.
    fn tags(title: &str) -> Vec<u8> {
        let vendor = format!("Karma Yoga, {}", opus::version());
        let mut comments = Vec::new();
        let mut title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut end = title.len().min(TITLE_BYTES);
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
        if !title.is_empty() {
            comments.push(format!("TITLE={title}"));
        }
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for comment in comments {
            tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            tags.extend_from_slice(comment.as_bytes());
        }
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::transcribe::BoxFuture;
    use karmayoga_core::news::script::NARRATOR;
    use karmayoga_core::speech::LocalVoice;
    use std::sync::Mutex;

    /// A voice that says every sentence as a tone as long as its text, and
    /// remembers what it was handed.
    struct Tone {
        hertz: f32,
        rate: u32,
        heard: Mutex<Vec<String>>,
    }

    impl Tone {
        fn new(hertz: f32, rate: u32) -> Arc<Self> {
            Arc::new(Self { hertz, rate, heard: Mutex::new(Vec::new()) })
        }

        fn heard(&self) -> Vec<String> {
            self.heard.lock().unwrap().clone()
        }
    }

    impl Synthesiser for Tone {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.heard.lock().unwrap().push(text.to_string());
            let shown = script::shown(text);
            let count = shown.trim().chars().count().max(1);
            // Ten milliseconds a letter, and the pause a line's end asks for.
            let length = count * self.rate as usize / 100;
            let mut samples: Vec<i16> = (0..length)
                .map(|i| {
                    let t = i as f32 / self.rate as f32;
                    (8000.0 * (2.0 * std::f32::consts::PI * self.hertz * t).sin()) as i16
                })
                .collect();
            if text.trim_end().ends_with(LINE_END) {
                samples.extend(vec![0; self.rate as usize / 2]);
            }
            let rate = self.rate;
            Box::pin(async move { Ok(Speech { samples, sample_rate: rate }) })
        }
    }

    /// A voice that fails as it is told to.
    struct Failing {
        code: &'static str,
        times: Mutex<u32>,
    }

    impl Synthesiser for Failing {
        fn synthesise<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            let mut times = self.times.lock().unwrap();
            let result = if *times > 0 {
                *times -= 1;
                Err(CommandError::new(self.code, "no"))
            } else {
                Ok(Speech { samples: vec![1000; 2400], sample_rate: 24_000 })
            };
            Box::pin(async move { result })
        }
    }

    /// A voice that will not say two sentences at once, and remembers what it
    /// was handed.
    #[derive(Default)]
    struct OneAtATime(Mutex<Vec<String>>);

    impl Synthesiser for OneAtATime {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.0.lock().unwrap().push(text.to_string());
            let result = if text.contains(". ") {
                Err(CommandError::new(codes::INVALID, "one at a time"))
            } else {
                Ok(Speech { samples: vec![1000; 2400], sample_rate: 24_000 })
            };
            Box::pin(async move { result })
        }
    }

    fn voices(configured: Arc<dyn Synthesiser>) -> Voices {
        Voices { configured, own: HashMap::new() }
    }

    fn sine(rate: u32, hertz: f32, seconds: f32) -> Vec<i16> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (8000.0 * (2.0 * std::f32::consts::PI * hertz * t).sin()) as i16
            })
            .collect()
    }

    fn no_progress() -> impl Fn(f32) + Send + Sync {
        |_| {}
    }

    // ---- who says it ------------------------------------------------------------

    #[test]
    fn a_speaker_that_names_one_of_the_voice_s_is_that_speaker() {
        let local = SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 0 };
        assert_eq!(speaker_number(&local, Some("Miles")), Some(5));
        assert_eq!(speaker_number(&local, Some(" miles ")), Some(5), "in any case");
        assert_eq!(speaker_number(&local, Some("Harper")), None, "the one already chosen");
        assert_eq!(speaker_number(&local, Some("Nobody")), None, "a voice that is not there");
        assert_eq!(speaker_number(&local, None), None);
        let kokoro = SynthesiserConfig::Local { voice: LocalVoice::Kokoro, speaker: 0 };
        assert_eq!(speaker_number(&kokoro, Some("af_bella")), Some(2));
        assert_eq!(speaker_number(&kokoro, Some("Miles")), None, "Phonon's speaker, not Kokoro's");
        let remote = SynthesiserConfig::OpenAi { model: "tts-1".into(), voice: "alloy".into() };
        assert_eq!(speaker_number(&remote, Some("Miles")), None);
    }

    // ---- saying -----------------------------------------------------------------

    #[test]
    fn a_line_is_sentences_and_the_last_ends_the_line() {
        assert_eq!(
            line_sentences("First thing, said at length. Second one!"),
            ["First thing, said at length.", format!("Second one!{LINE_END}").as_str()]
        );
        // The items of a list are each a line's end.
        let said = line_sentences("Three things:\n- one thing here\n- two things here");
        assert!(said.len() >= 3, "{said:?}");
        assert!(said.iter().all(|s| s.ends_with(LINE_END) || !s.contains("things here")));
        assert!(said.last().unwrap().ends_with(LINE_END));
        // A stranger's braces are only braces.
        assert_eq!(
            line_sentences("Say {Siobhan|shiv-AWN} twice."),
            [format!("Say (Siobhan|shiv-AWN) twice.{LINE_END}")]
        );
        assert!(line_sentences("  ").is_empty());
    }

    #[test]
    fn a_line_is_said_in_clips_as_long_as_may_be_and_never_past_a_paragraph() {
        let long = "a".repeat(150);
        let said = vec![
            "One.".to_string(),
            format!("Two.{LINE_END}"),
            "Three.".to_string(),
            long.clone(),
            long,
        ];
        assert_eq!(clips(&said), vec![&said[0..2], &said[2..4], &said[4..5]]);
        assert!(clips(&[]).is_empty());
    }

    #[tokio::test]
    async fn sentences_the_voice_will_not_say_together_are_said_one_at_a_time() {
        let voice = Arc::new(OneAtATime::default());
        let script = Script::narrated("T", "One thing here. Another thing there.");
        let said = record(&voices(voice.clone()), &script, &no_progress()).await.unwrap();
        assert_eq!(
            *voice.0.lock().unwrap(),
            [
                format!("One thing here. Another thing there.{LINE_END}"),
                "One thing here.".to_string(),
                format!("Another thing there.{LINE_END}"),
            ]
        );
        assert!(said.duration_ms >= 190, "both said: {} ms", said.duration_ms);
    }

    #[tokio::test]
    async fn every_line_goes_to_its_speaker_s_voice_its_sentences_together() {
        let (a, b) = (Tone::new(220.0, 24_000), Tone::new(330.0, 24_000));
        let mut voices = voices(a.clone());
        voices.own.insert("sam".into(), b.clone());
        let script = Script::parse(
            "Talk",
            "Welcome to the show, everyone. Glad to be here.\n\nSAM: Thanks for having me.\n\nBack to it now.",
            NARRATOR,
        );
        let said = record(&voices, &script, &no_progress()).await.unwrap();
        assert_eq!(
            a.heard(),
            [
                format!("Welcome to the show, everyone. Glad to be here.{LINE_END}"),
                format!("Back to it now.{LINE_END}"),
            ]
        );
        assert_eq!(b.heard(), [format!("Thanks for having me.{LINE_END}")]);
        assert!(said.duration_ms > 2_000, "{} ms", said.duration_ms);
    }

    #[tokio::test]
    async fn a_new_speaker_is_given_a_beat_before_answering() {
        let rate = 24_000u32;
        let (a, b) = (Tone::new(220.0, rate), Tone::new(330.0, rate));
        let mut two_voices = voices(a.clone());
        two_voices.own.insert("sam".into(), b);
        let talk = Script::parse("T", "One.\n\nSAM: Two.\n\nThree.", NARRATOR);
        let talk = record(&two_voices, &talk, &no_progress()).await.unwrap();
        // The same words said by one speaker have no beats in them.
        let alone = Script::narrated("T", "One.\n\nTwo.\n\nThree.");
        let alone = record(&voices(a), &alone, &no_progress()).await.unwrap();
        let beats = talk.duration_ms as i64 - alone.duration_ms as i64;
        assert!(
            (beats - 2 * SPEAKER_CHANGE.as_millis() as i64).abs() <= 5,
            "two changes of speaker leave two beats: {beats} ms"
        );
    }

    #[tokio::test]
    async fn progress_is_the_share_of_the_text_said() {
        let seen = Mutex::new(Vec::new());
        let tone = Tone::new(220.0, 24_000);
        let script = Script::narrated("T", "Aaaaaaaaaa.\n\nBbbbbbbbbbbbbbbbbbbb.");
        record(&voices(tone), &script, &|f| seen.lock().unwrap().push(f)).await.unwrap();
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0] > 0.2 && seen[0] < 0.5, "{seen:?}: a third of the letters");
        assert!((seen[1] - 1.0).abs() < 1e-6, "{seen:?}");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
    }

    #[tokio::test]
    async fn another_rate_is_converted_to_the_first() {
        // The second speaker's voice speaks at another rate.
        let (a, b) = (Tone::new(220.0, 24_000), Tone::new(330.0, 16_000));
        let mut voices = voices(a);
        voices.own.insert("sam".into(), b);
        let script = Script::parse("T", "Hello there you.\n\nSAM: Hello there you.", NARRATOR);
        let said = record(&voices, &script, &no_progress()).await.unwrap();
        // Each is sixteen letters at ten milliseconds each and half a second
        // of silence, and the beat between: as long as that at either rate.
        let expected = 2 * (160 + 500) + SPEAKER_CHANGE.as_millis() as i64;
        assert!(
            (said.duration_ms as i64 - expected).abs() < 40,
            "{} ms, not about {expected} ms",
            said.duration_ms
        );
    }

    #[tokio::test]
    async fn a_sentence_the_voice_refuses_is_skipped_and_one_it_fails_on_is_tried_again() {
        let script = Script::narrated("T", "Something to say.");
        // Refused: nothing in the render, and so nothing to keep.
        let refused = Arc::new(Failing { code: codes::INVALID, times: Mutex::new(9) });
        let err = record(&voices(refused), &script, &no_progress()).await.err().unwrap();
        assert_eq!(err.code, codes::INVALID);
        assert!(err.message.contains("nothing"), "{}", err.message);
        // Failed once: said the second time.
        let flaky = Arc::new(Failing { code: codes::NETWORK, times: Mutex::new(1) });
        let said = record(&voices(flaky), &script, &no_progress()).await.unwrap();
        assert!(said.duration_ms > 0);
        // Failed twice: the render fails, and says why.
        let down = Arc::new(Failing { code: codes::NETWORK, times: Mutex::new(9) });
        let err = record(&voices(down), &script, &no_progress()).await.err().unwrap();
        assert_eq!(err.code, codes::NETWORK);
    }

    #[tokio::test]
    async fn a_script_with_nothing_to_say_is_refused() {
        let script = Script::narrated("T", "   ");
        let err =
            record(&voices(Tone::new(220.0, 24_000)), &script, &no_progress()).await.err().unwrap();
        assert_eq!(err.code, codes::INVALID);
    }

    // ---- encoding ---------------------------------------------------------------

    #[test]
    fn a_recording_is_a_file_with_the_format_s_extension() {
        let rate = 24_000;
        let encoded = encode(&sine(rate, 220.0, 1.5), rate, "A title").unwrap();
        assert_eq!(encoded.duration_ms, 1_500);
        #[cfg(feature = "speech")]
        {
            assert_eq!(encoded.extension, "opus");
            assert!(encoded.bytes.starts_with(b"OggS"));
        }
        #[cfg(not(feature = "speech"))]
        {
            assert_eq!(encoded.extension, "wav");
            assert!(encoded.bytes.starts_with(b"RIFF"));
            let back = crate::speech::wav::decode(&encoded.bytes).unwrap();
            assert_eq!((back.sample_rate, back.samples.len()), (rate, rate as usize * 3 / 2));
        }
        assert!(encode(&[], rate, "x").is_err(), "nothing is not a recording");
    }

    #[test]
    fn a_rate_the_format_does_not_take_is_converted() {
        let encoded = encode(&sine(22_050, 220.0, 1.0), 22_050, "T").unwrap();
        #[cfg(feature = "speech")]
        {
            assert_eq!(recording_rate(22_050), 48_000);
            assert_eq!(recording_rate(24_000), 24_000);
            assert!((encoded.duration_ms as i64 - 1_000).abs() <= 25, "{}", encoded.duration_ms);
        }
        #[cfg(not(feature = "speech"))]
        {
            assert_eq!(recording_rate(22_050), 22_050);
            assert_eq!(encoded.duration_ms, 1_000);
        }
    }

    /// Ogg Opus read back with the crates that write it: its packets, where
    /// each page says it has got to, and the sound it decodes to.
    #[cfg(feature = "speech")]
    mod ogg_opus {
        use super::*;
        use ogg::PacketReader;
        use std::io::Cursor;

        struct Read {
            head: Vec<u8>,
            tags: Vec<u8>,
            /// Each audio packet, and the position its page says, if it ends one.
            audio: Vec<(Vec<u8>, Option<u64>)>,
            ends_stream: bool,
            pages: usize,
        }

        fn read(bytes: &[u8]) -> Read {
            let mut reader = PacketReader::new(Cursor::new(bytes.to_vec()));
            let (mut packets, mut pages, mut ends_stream) = (Vec::new(), 0, false);
            while let Some(packet) = reader.read_packet().unwrap() {
                pages += usize::from(packet.first_in_page());
                ends_stream = packet.last_in_stream();
                let position = packet.last_in_page().then(|| packet.absgp_page());
                packets.push((packet.data, position));
            }
            let mut packets = packets.into_iter();
            let head = packets.next().expect("a head").0;
            let tags = packets.next().expect("tags").0;
            Read { head, tags, audio: packets.collect(), ends_stream, pages }
        }

        fn le16(b: &[u8], at: usize) -> u64 {
            u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
        }

        fn le32(b: &[u8], at: usize) -> u64 {
            u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
        }

        #[test]
        fn the_file_is_ogg_opus_with_its_head_and_tags() {
            let encoded = encode(&sine(24_000, 220.0, 2.0), 24_000, "Why: a title").unwrap();
            assert!(encoded.bytes.starts_with(b"OggS"));
            // The head is the first page's, alone: "OggS", then 26 bytes of
            // page header and a segment table of one, then the packet.
            assert_eq!(&encoded.bytes[28..36], b"OpusHead");
            let read = read(&encoded.bytes);
            assert_eq!(&read.head[..8], b"OpusHead");
            assert_eq!((read.head[8], read.head[9]), (1, 1), "version 1, mono");
            assert!(le16(&read.head, 10) > 0, "the encoder's delay is the pre-skip");
            assert_eq!(le32(&read.head, 12), 24_000, "the rate it was made at");
            assert_eq!(read.head[18], 0, "mapping family 0");
            assert_eq!(&read.tags[..8], b"OpusTags");
            let vendor = le32(&read.tags, 8) as usize;
            let vendor_text = std::str::from_utf8(&read.tags[12..12 + vendor]).unwrap();
            assert!(vendor_text.starts_with("Karma Yoga"), "{vendor_text}");
            let at = 12 + vendor;
            assert_eq!(le32(&read.tags, at), 1, "one comment");
            let len = le32(&read.tags, at + 4) as usize;
            assert_eq!(&read.tags[at + 8..at + 8 + len], b"TITLE=Why: a title");
        }

        #[test]
        fn positions_advance_by_a_frame_and_the_last_is_where_the_sound_ends() {
            let seconds = 3.2;
            let encoded = encode(&sine(24_000, 220.0, seconds), 24_000, "T").unwrap();
            let read = read(&encoded.bytes);
            let pre_skip = le16(&read.head, 10);
            assert!(read.ends_stream, "the last packet ends the stream");
            // 20 ms frames: 3.2 s of sound and the delay's worth of padding.
            assert!((160..=162).contains(&read.audio.len()), "{} packets", read.audio.len());
            // A page about every second.
            assert!(read.pages >= 4, "{} pages", read.pages);
            let positions: Vec<u64> = read.audio.iter().filter_map(|(_, p)| *p).collect();
            assert!(positions.len() >= 3);
            assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
            // A full page of fifty packets is 48 000 samples further on.
            assert_eq!(positions[1] - positions[0], 48_000, "{positions:?}");
            assert_eq!(positions[0], pre_skip + 50 * 960);
            // The last is the sound's length in 48 kHz samples past the delay.
            assert_eq!(*positions.last().unwrap(), pre_skip + (seconds * 48_000.0) as u64);
        }

        #[test]
        fn it_decodes_to_the_sound_that_went_in() {
            let rate = 24_000;
            let samples = sine(rate, 440.0, 2.0);
            let encoded = encode(&samples, rate, "T").unwrap();
            let read = read(&encoded.bytes);
            let pre_skip = le16(&read.head, 10) as usize;
            let mut decoder = opus::Decoder::new(48_000, opus::Channels::Mono).unwrap();
            let mut pcm = Vec::new();
            let mut frame = vec![0i16; 5_760];
            for (packet, _) in &read.audio {
                let n = decoder.decode(packet, &mut frame, false).unwrap();
                pcm.extend_from_slice(&frame[..n]);
            }
            let end = *read.audio.last().unwrap().1.as_ref().unwrap() as usize;
            let heard = &pcm[pre_skip..end];
            assert_eq!(heard.len(), 96_000, "two seconds at 48 kHz, nothing added or lost");
            // The tone comes through: as loud as it went in, give or take
            // what a lossy codec does, and at the pitch it had.
            let rms = |s: &[i16]| {
                (s.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
            };
            let (before, after) = (rms(&samples), rms(&heard[4_800..91_200]));
            assert!((after / before - 1.0).abs() < 0.2, "{before} in, {after} out");
            let crossings =
                heard[4_800..91_200].windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
            let hertz = crossings as f64 / (86_400.0 / 48_000.0);
            assert!((hertz - 440.0).abs() < 10.0, "{hertz} Hz");
        }

        #[test]
        fn what_is_not_a_whole_frame_is_still_said() {
            // 20 ms at 24 kHz is 480 samples; this is 7 and a bit frames.
            let samples = sine(24_000, 220.0, 0.151);
            let encoded = encode(&samples, 24_000, "").unwrap();
            let read = read(&encoded.bytes);
            assert_eq!(&read.tags[..8], b"OpusTags");
            assert_eq!(
                le32(&read.tags, 12 + le32(&read.tags, 8) as usize),
                0,
                "no title, no comment"
            );
            let pre_skip = le16(&read.head, 10);
            let end = read.audio.last().unwrap().1.unwrap();
            assert_eq!(end, pre_skip + samples.len() as u64 * 2);
        }

        #[test]
        fn a_quarter_of_an_hour_is_a_few_megabytes() {
            // Speech-like: a tone that moves, with pauses.
            let rate = 24_000;
            let mut samples = Vec::new();
            for i in 0..90 {
                samples.extend(sine(rate, 150.0 + (i % 7) as f32 * 40.0, 8.0));
                samples.extend(vec![0; rate as usize * 2]);
            }
            let encoded = encode(&samples, rate, "Long").unwrap();
            assert_eq!(encoded.duration_ms, 900_000);
            let megabytes = encoded.bytes.len() as f64 / 1e6;
            assert!(megabytes < 4.5, "{megabytes} MB");
        }
    }

    // ---- into the files folder ----------------------------------------------------

    mod into_a_vault {
        use super::*;
        use karmayoga_core::VaultConfig;
        use karmayoga_core::files::{FileKind, FileStatus};

        struct Fixture {
            svc: Arc<Service>,
            folder: tempfile::TempDir,
            _home: tempfile::TempDir,
        }

        fn fixture() -> Fixture {
            let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let config = VaultConfig { password: None, ..Default::default() };
            let vault = karmayoga_vault::create(home.path(), config).unwrap();
            let svc = Arc::new(Service::new());
            let vault = svc.set(vault);
            vault.set_up_files_folder(Some(folder.path())).unwrap();
            Fixture { svc, folder, _home: home }
        }

        #[tokio::test]
        async fn a_script_becomes_a_file_of_the_feed_with_its_length() {
            let f = fixture();
            let (a, b) = (Tone::new(220.0, 24_000), Tone::new(330.0, 24_000));
            let mut voices = voices(a);
            voices.own.insert("sam".into(), b);
            let script = Script::parse(
                "Why: a test",
                "Welcome to the show, everyone.\n\nSAM: Thanks for having me on.",
                NARRATOR,
            );
            let seen = Mutex::new(Vec::new());
            let recorded =
                record(&voices, &script, &|f| seen.lock().unwrap().push(f)).await.unwrap();
            let file = store(&f.svc, recorded, "Feed/Audio/The Show", "2026-11-22 Why - a test")
                .await
                .unwrap();

            let ext = if cfg!(feature = "speech") { "opus" } else { "wav" };
            let name = format!("2026-11-22 Why - a test.{ext}");
            assert_eq!(file.path(), Some(format!("Feed/Audio/The Show/{name}").as_str()));
            assert_eq!(file.origin, FileOrigin::Feed);
            assert_eq!(file.kind, FileKind::Audio);
            assert!(matches!(file.status, FileStatus::Present));
            let on_disk =
                std::fs::read(f.folder.path().join("Feed/Audio/The Show").join(&name)).unwrap();
            assert_eq!(on_disk.len() as u64, file.size);
            assert!(on_disk.starts_with(if cfg!(feature = "speech") { b"OggS" } else { b"RIFF" }));
            let length = file.media.duration_ms.expect("its length is kept with it");
            assert!(
                (1_700..1_900).contains(&length),
                "{length} ms: two lines, their pauses and a beat"
            );
            // What the catalog holds is what was written.
            let vault = f.svc.require().unwrap();
            assert_eq!(vault.file(file.id).unwrap().media.duration_ms, Some(length));
            assert_eq!(seen.into_inner().unwrap().last().copied(), Some(1.0));
        }

        #[tokio::test]
        async fn a_clash_of_names_is_not_an_overwrite() {
            let f = fixture();
            let tone = Tone::new(220.0, 24_000);
            let script = Script::narrated("T", "Something to say.");
            let again = || async {
                let recorded =
                    record(&voices(tone.clone()), &script, &no_progress()).await.unwrap();
                store(&f.svc, recorded, "Feed/Audio/Show", "Same name").await.unwrap()
            };
            let (first, second) = (again().await, again().await);
            assert_ne!(first.id, second.id);
            assert_ne!(first.path(), second.path());
        }

        #[tokio::test]
        async fn a_locked_vault_refuses_before_anything_is_said() {
            let svc = Arc::new(Service::new());
            let script = Script::narrated("T", "Something to say.");
            let err = render_script(&svc, &script, "Feed/Audio", "x", &no_progress())
                .await
                .err()
                .unwrap();
            assert_eq!(err.code, codes::NO_VAULT);
        }
    }
}
