//! Downloading episodes into the files folder, and clearing them out again.
//! See `docs/plans/podcasts.md`.
//!
//! One supervised task ([`TASK_KEY`]) works through a queue of stories to
//! download, one or two at a time, streaming each to `Feed/Podcasts/<show>/`
//! and linking it with [`karmayoga_core::Vault::update_story_files`]. The
//! refresher hands it what each refresh brought ([`after_refresh`]), where a
//! show's download and Up Next policies are applied; a button or Up Next
//! asks for one episode ([`request`]). The same task runs the clean-up
//! ([`sweep`]) that each show's keep policy asks for.
//!
//! # Where the bytes go
//!
//! An episode is fetched over [`crate::http::public_client`]: the address is
//! one a stranger's feed named, and a feed that names `http://192.168.0.1/`
//! is not asking for an episode. The body is streamed to a scratch file
//! outside the files folder -- never held in memory, and never seen by the
//! folder's watcher half-written -- and only then handed to
//! [`karmayoga_core::Vault::import_file`], which puts it where Files puts
//! everything (or seals it, if the person has asked for the Feed's files to be
//! sealed), under a free name. A scratch file rather than the vault's upload
//! calls because those want the size up front, and a server that sends no
//! `Content-Length` -- or a wrong one -- is an everyday thing here.
//!
//! The same derive step every other way of adding a file runs afterwards
//! ([`crate::files::probe_later`]), so the file's length and the rest of what
//! it says about itself are filled as for any file.
//!
//! # Nothing here is a promise to keep
//!
//! The waiting list, the progress and the failures live in memory
//! ([`Downloads`]) and go when the vault locks. A download that was under way
//! when the task was stopped goes back to the front of the list, and starts
//! again from the beginning: an episode is tens of megabytes, and resuming a
//! stranger's server's ranges is more failure modes than it saves minutes.
//! What lasts is what is linked to the story, which is the file.

use super::announce;
use super::route::{Refusals, Route};
use crate::http::BROWSER_USER_AGENT;
use super::queue;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Kind, Op};
use crate::files;
use crate::http;
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::files::{
    FileFilter, FileKind, FileOrigin, FileQuery, FileStatus, FileUpdate, ImportOptions, PropValue,
    StatusFilter,
};
use karmayoga_core::id::{FileId, NewsSourceId, StoryId};
use karmayoga_core::news::podcast::PODCASTS_DIR;
use karmayoga_core::news::transcript::TRANSCRIPT_PROPERTY;
use karmayoga_core::news::{
    AddedBy, ContentRef, DownloadPolicy, EnclosureKind, KeepPolicy, PodcastPolicy, StoryFiles,
    TranscribePolicy,
};
use karmayoga_core::{FileEntry, QueuePlace, Vault};
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, watch};

/// The supervisor key of the download task.
pub const TASK_KEY: &str = "news-downloads";

/// How many episodes are fetched at once. Two keeps a second one going while
/// the first is being copied into the folder, without taking a person's
/// whole connection from whatever else they are doing.
const AT_ONCE: usize = 2;

/// How long the task sleeps when nothing wakes it. Clean-up by a keep policy
/// is time-driven -- an episode becomes "played a week ago" without anything
/// happening -- so it comes round on its own a few times a day.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The most an episode may be. A four-hour video is a few gigabytes; past
/// this it is not an episode, and a server that keeps sending would otherwise
/// fill the disk.
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long a server may say nothing -- before its headers, or between two
/// pieces of the body -- before the download is given up as stalled. The
/// shared client's own thirty-second limit covers a whole request, which is
/// right for a feed and wrong for a film, so each request here lifts that
/// limit ([`TOTAL`]) and this is what notices a dead connection instead.
const STALL: Duration = Duration::from_secs(60);

/// The longest a whole download may take, however steadily it goes. A bound
/// and not a plan: [`STALL`] is what ends a download that has gone wrong.
const TOTAL: Duration = Duration::from_secs(12 * 60 * 60);

/// How much is gathered before it is written, so the disk is asked for a
/// megabyte at a time instead of for each of the several thousand pieces a
/// large episode arrives in.
const FLUSH_AT: usize = 1024 * 1024;

/// How many of a show's episodes are downloaded when it is followed with
/// "download every new episode": its first refresh brings the whole back
/// catalogue, which could be a thousand episodes.
const FIRST_REFRESH_ALL: usize = 3;

/// How long an episode is safe from "keep the newest N", counted from when
/// it was downloaded. Without it, pressing Download on an old episode of a
/// show that keeps its latest few would have it taken away again at the next
/// pass, which would look like the button had not worked.
const FRESH_SECS: i64 = 24 * 60 * 60;

/// The failures kept for the interface to show, newest last; a vault whose
/// files folder is missing would otherwise add one for every episode of
/// every refresh.
const MAX_FAILED: usize = 50;

/// How long a downloaded episode's file may be missing from the folder before
/// the story is unlinked from it.
const MISSING_GRACE_SECS: i64 = 24 * 60 * 60;

/// How many files are read from the catalog at once.
const CATALOG_BATCH: usize = 1000;

/// How old a scratch file must be before the next run of the task takes it
/// for one a crash left.
const STALE_SCRATCH: Duration = Duration::from_secs(60 * 60);

/// Longest an episode's title may be in its file name, in bytes. A name may
/// be 255 bytes at most, and a title in a script that takes three bytes a
/// letter is not to use them all.
const TITLE_BYTES: usize = 120;

/// Longest a show's folder name may be, in bytes.
const SHOW_BYTES: usize = 80;

/// The extensions of audio and video a file name may be given.
const MEDIA_EXTENSIONS: &[&str] =
    &["mp3", "m4a", "aac", "ogg", "oga", "opus", "wav", "flac", "mp4", "m4v", "mov", "webm", "mkv"];

// ── What is going on ─────────────────────────────────────────────────────

/// What is being downloaded and waiting to be, held in memory for the life
/// of the unlocked vault. Lives on [`Service`] (`Service::listening`).
#[derive(Default)]
pub struct Downloads {
    state: Mutex<State>,
    /// Rung when something is added to the list or the clean-up is asked
    /// for; the task waits on it between passes. A ring with nobody waiting
    /// is kept, so one that lands while a pass is running is not lost.
    wake: Notify,
    /// Rung whenever a download stops being under way, by whatever route, for
    /// whoever is waiting on one ([`Downloads::wait_until_idle`]).
    idle: Notify,
}

#[derive(Default)]
struct State {
    waiting: VecDeque<Waiting>,
    running: Vec<Running>,
    /// Kept until the same episode is asked for again, or the vault locks.
    failed: VecDeque<Failure>,
}

struct Waiting {
    story: StoryId,
    /// What the interface calls it, kept from the moment it was queued so
    /// that a list of downloads needs no look at the vault.
    title: String,
}

struct Running {
    story: StoryId,
    title: String,
    progress: Arc<Progress>,
    /// Turns `true` to stop this one and throw away what it has.
    cancel: watch::Sender<bool>,
}

struct Failure {
    story: StoryId,
    title: String,
    /// A sentence fit to show, with no address in it.
    error: String,
}

/// How far one download has got, written by the download and read by
/// [`status`].
#[derive(Default)]
struct Progress {
    received: AtomicU64,
    /// Zero while the server has not said.
    total: AtomicU64,
}

/// One episode the task has taken off the list.
struct Job {
    story: StoryId,
    progress: Arc<Progress>,
    cancel: watch::Receiver<bool>,
}

/// How a job came out.
enum Ended {
    /// It is in the files folder and linked, or it already was.
    Finished,
    /// Somebody stopped it.
    Cancelled,
    Failed(String),
}

impl Downloads {
    /// Forget everything: the vault locked.
    pub fn clear(&self) {
        let mut state = self.state();
        for running in state.running.drain(..) {
            let _ = running.cancel.send(true);
        }
        state.waiting.clear();
        state.failed.clear();
        drop(state);
        self.idle.notify_waiters();
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Put an episode on the list. Answers whether it was added: one that is
    /// already waiting or under way is left as it is, so asking twice -- a
    /// refresh's policy, then Up Next -- is one download. Asking again for
    /// one that failed forgets the failure: that is the retry.
    fn queue(&self, story: StoryId, title: String) -> bool {
        let mut state = self.state();
        if state.running.iter().any(|r| r.story == story)
            || state.waiting.iter().any(|w| w.story == story)
        {
            return false;
        }
        state.failed.retain(|f| f.story != story);
        state.waiting.push_back(Waiting { story, title });
        true
    }

    /// Take the next episode off the list and mark it as under way.
    fn start_next(&self) -> Option<Job> {
        let mut state = self.state();
        let Waiting { story, title } = state.waiting.pop_front()?;
        let progress = Arc::new(Progress::default());
        let (cancel, stopped) = watch::channel(false);
        state.running.push(Running { story, title, progress: progress.clone(), cancel });
        Some(Job { story, progress, cancel: stopped })
    }

    /// Record how a job came out.
    fn end(&self, story: StoryId, ended: Ended) {
        let mut state = self.state();
        let Some(at) = state.running.iter().position(|r| r.story == story) else { return };
        let running = state.running.remove(at);
        if let Ended::Failed(error) = ended {
            state.failed.push_back(Failure { story, title: running.title, error });
            while state.failed.len() > MAX_FAILED {
                state.failed.pop_front();
            }
        }
        drop(state);
        self.idle.notify_waiters();
    }

    /// Resolves once `story` is not being downloaded: at once if it is not,
    /// else when the download that is ends, finished or stopped.
    ///
    /// For whoever must not run while a download of the story might still be
    /// about to link a file to it -- removing one. A download that has been
    /// told to stop still has its last steps to take, which cannot be undone
    /// halfway: the file is copied in and linked in one go.
    async fn wait_until_idle(&self, story: StoryId) {
        loop {
            // Made before the look, so an end that lands between the two is
            // not missed.
            let ended = self.idle.notified();
            if !self.state().running.iter().any(|r| r.story == story) {
                return;
            }
            ended.await;
        }
    }

    /// Stop one episode, or take it off the list, or forget that it failed.
    fn cancel(&self, story: StoryId) {
        let mut state = self.state();
        state.waiting.retain(|w| w.story != story);
        state.failed.retain(|f| f.story != story);
        if let Some(running) = state.running.iter().find(|r| r.story == story) {
            let _ = running.cancel.send(true);
        }
    }

    /// What was under way when a task was stopped goes back to the front of
    /// the list: the future that was downloading it is gone, and nothing else
    /// will ever end it. One that had been cancelled is not asked for again.
    fn requeue_interrupted(&self) {
        let mut state = self.state();
        let interrupted = std::mem::take(&mut state.running);
        for running in interrupted.into_iter().rev() {
            if !*running.cancel.borrow() {
                state.waiting.push_front(Waiting { story: running.story, title: running.title });
            }
        }
        drop(state);
        self.idle.notify_waiters();
    }

    fn statuses(&self) -> Vec<DownloadStatus> {
        let state = self.state();
        let running = state.running.iter().map(|r| {
            let total = r.progress.total.load(Ordering::Relaxed);
            DownloadStatus {
                story_id: r.story,
                title: r.title.clone(),
                state: DownloadState::Downloading,
                received: r.progress.received.load(Ordering::Relaxed),
                total: (total > 0).then_some(total),
                error: None,
            }
        });
        let waiting = state.waiting.iter().map(|w| DownloadStatus {
            story_id: w.story,
            title: w.title.clone(),
            state: DownloadState::Waiting,
            received: 0,
            total: None,
            error: None,
        });
        let failed = state.failed.iter().map(|f| DownloadStatus {
            story_id: f.story,
            title: f.title.clone(),
            state: DownloadState::Failed,
            received: 0,
            total: None,
            error: Some(f.error.clone()),
        });
        running.chain(waiting).chain(failed).collect()
    }

    fn wake(&self) {
        self.wake.notify_one();
    }
}

/// One episode on its way, for the interface's progress.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub story_id: StoryId,
    pub title: String,
    pub state: DownloadState,
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadState {
    Waiting,
    Downloading,
    Failed,
}

// ── The task ─────────────────────────────────────────────────────────────

/// Start the download task if it is not already running.
///
/// Only where this process may write: an episode is linked to its story,
/// and a second window that merely reads must not fetch files it cannot keep.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.is_writable() || !vault.supports_news() || !vault.supports_files() {
        return;
    }
    // Looks the vault up each time it starts rather than keeping one: see
    // `news::ensure_task` for why a vault held by a factory the supervisor
    // keeps for good would be wrong.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault)
                    if vault.is_writable() && vault.supports_news() && vault.supports_files() =>
                {
                    run(svc, vault, stop).await
                }
                _ => Ok(Outcome::Done),
            }
        })
    });
}

/// The supervised loop: a pass at the start, then one whenever something is
/// queued and one every [`EVERY`]. Returns when `stop` turns `true`.
async fn run(svc: Arc<Service>, vault: Arc<Vault>, mut stop: watch::Receiver<bool>) -> TaskResult {
    svc.listening.downloads.requeue_interrupted();
    let scratch = vault.clone();
    let _ = tokio::task::spawn_blocking(move || clear_stale_scratch(&scratch)).await;
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        // Locked between one pass and the next: nothing can be written and
        // nothing should be fetched. The supervisor stops this task on the
        // lock in any case; until it does, wait.
        if files::still_open(&svc, &vault) && pass(&svc, &vault, &Route::Public, &mut stop).await {
            return Ok(Outcome::Done);
        }
        tokio::select! {
            () = svc.listening.downloads.wake.notified() => {}
            () = tokio::time::sleep(EVERY) => {}
            stopping = stop.changed() => {
                if stopping.is_err() {
                    return Ok(Outcome::Done);
                }
            }
        }
    }
}

/// One pass: everything on the list, [`AT_ONCE`] at a time, then the
/// clean-up. Answers whether the task was asked to stop in the middle of it.
///
/// Listens for a wake while downloads are under way, so an episode asked for
/// while a slot is free starts at once and does not wait for a long one to
/// finish.
async fn pass(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    route: &Route,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < AT_ONCE {
            let Some(job) = svc.listening.downloads.start_next() else { break };
            running.push(run_job(svc.clone(), vault.clone(), route.clone(), job));
        }
        if running.is_empty() {
            break;
        }
        tokio::select! {
            _ = running.next() => {}
            () = svc.listening.downloads.wake.notified() => {}
            stopping = stop.changed() => {
                // Dropping `running` drops what is downloading, and each
                // scratch file goes with its download. What it was
                // downloading is on the list again, and nobody waits on it.
                if stopping.is_err() || *stop.borrow() {
                    drop(running);
                    svc.listening.downloads.requeue_interrupted();
                    return true;
                }
            }
        }
    }
    if let Err(e) = sweep(svc).await {
        tracing::warn!(error = %e, "could not clear out old episodes");
    }
    false
}

/// One download, from the list to the files folder, and what became of it.
async fn run_job(svc: Arc<Service>, vault: Arc<Vault>, route: Route, mut job: Job) {
    let story = job.story;
    let ended = match download(&svc, &vault, &route, &mut job).await {
        Ok(()) => Ended::Finished,
        Err(Stop::Cancelled) => Ended::Cancelled,
        Err(Stop::Failed(message)) => {
            tracing::debug!(%story, %message, "podcasts: an episode could not be downloaded");
            Ended::Failed(message)
        }
    };
    svc.listening.downloads.end(story, ended);
}

/// Why a download did not finish.
#[derive(Debug)]
enum Stop {
    Cancelled,
    /// A sentence fit for the line under an episode's title.
    Failed(String),
}

fn failed(message: impl Into<String>) -> Stop {
    Stop::Failed(message.into())
}

impl From<CommandError> for Stop {
    fn from(e: CommandError) -> Self {
        Stop::Failed(e.message)
    }
}

/// Fetch one episode, put it in the files folder and link it to its story.
///
/// Nothing is fetched for an episode that is already there: the story's
/// `files.media` names a file the catalog still has in place. One whose file
/// was trashed or went missing is fetched again, and the link replaced.
async fn download(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    route: &Route,
    job: &mut Job,
) -> Result<(), Stop> {
    let id = job.story;
    let (story, show, present, transcribe) = blocking({
        let vault = vault.clone();
        move || {
            let story = vault.story(id)?;
            let source = vault.news_source(story.source_id);
            let show = source.as_ref().map(|s| s.title.clone()).unwrap_or_default();
            // Whether the show's policy is to transcribe an episode the moment
            // it is here. A source that cannot be read is a show with no policy.
            let transcribe = source
                .ok()
                .and_then(|s| vault.podcast_policy(&s).ok().flatten())
                .is_some_and(|p| p.transcribe == TranscribePolicy::WhenDownloaded);
            let present = story.files.media.is_some_and(|file| is_present(&vault, file));
            Ok((story, show, present, transcribe))
        }
    })
    .await?;
    if present {
        return Ok(());
    }
    let Some(enclosure) = story.media.clone() else {
        return Err(failed("that story has no episode to download."));
    };

    let scratch = Scratch::new(vault, id)
        .map_err(|e| failed(format!("could not make room for the episode: {e}.")))?;
    let fetched = fetch_to(route, &enclosure.url, enclosure.length, scratch.path(), job).await?;

    let mime =
        if enclosure.mime.trim().is_empty() { &fetched.content_type } else { &enclosure.mime };
    let extension = extension_for(&enclosure.url, mime, enclosure.kind());
    let date = karmayoga_core::model::local_date_in(
        story.published_at,
        &karmayoga_core::model::system_tz(),
    );
    let options = ImportOptions {
        dir: Some(show_dir(&show)),
        name: Some(episode_name(date, &story.title, extension)),
        origin: FileOrigin::Feed,
        encrypted: None,
    };
    if *job.cancel.borrow() {
        return Err(Stop::Cancelled);
    }

    let file = blocking({
        let (vault, from) = (vault.clone(), scratch.path().to_path_buf());
        move || {
            let file = vault.import_file(&from, &options)?;
            // The copy in the files folder is whole; the scratch one is
            // not needed a moment longer, and an episode can be gigabytes.
            let _ = std::fs::remove_file(&from);
            if let Err(e) = vault.update_story_files(id, |files| files.media = Some(file.id)) {
                // The story is gone -- expired, or its source deleted -- while
                // the episode came down. A file nothing refers to would sit
                // in the folder for ever, since only a story's own policy
                // ever clears one out.
                let _ = vault.trash_files(&[file.id]);
                return Err(e.into());
            }
            Ok(file)
        }
    })
    .await?;

    announce(svc, Kind::File, Op::Created, vec![file.id.to_string()]);
    announce(svc, Kind::Story, Op::Updated, vec![id.to_string()]);
    files::probe_later(svc, vault, vec![file.id]);
    if transcribe {
        super::transcribe::request(svc, super::transcribe::Job::Story(id));
    }
    Ok(())
}

/// Whether a file is in the catalog and where it says it is.
fn is_present(vault: &Vault, file: FileId) -> bool {
    vault.file(file).is_ok_and(|f| matches!(f.status, FileStatus::Present))
}

// ── The network ──────────────────────────────────────────────────────────

/// What [`Route::open`] says when it refuses an episode's address.
const REFUSALS: Refusals = Refusals {
    not_a_web_address: "the episode's address is not a web address.",
    only_web: "only web addresses, starting http:// or https://, can be downloaded.",
    no_site: "the episode's address has no site in it.",
    private_network: "that episode is on a private network, and episodes are only \
         downloaded from the internet.",
};

/// What a download brought, beyond the file it wrote.
struct Fetched {
    /// What the server said it was, for choosing an extension when neither
    /// the feed nor the address did.
    content_type: String,
}

/// Stream `address` into the scratch file at `to`, following redirects,
/// counting what has arrived into the job's progress and stopping the moment
/// the job is cancelled.
async fn fetch_to(
    route: &Route,
    address: &str,
    expected: Option<u64>,
    to: &Path,
    job: &mut Job,
) -> Result<Fetched, Stop> {
    let (client, url) = route.open(address, &REFUSALS).map_err(Stop::Failed)?;
    let request = client
        .get(url)
        .header(USER_AGENT, BROWSER_USER_AGENT)
        .header(ACCEPT, "audio/*, video/*;q=0.9, */*;q=0.5")
        // The client's own limit is for a feed and ends a request after half
        // a minute, body included. See [`STALL`].
        .timeout(TOTAL);
    let sent = tokio::select! {
        sent = tokio::time::timeout(STALL, request.send()) => sent,
        () = cancelled(&mut job.cancel) => return Err(Stop::Cancelled),
    };
    let mut response = match sent {
        Err(_) => return Err(failed("the site did not answer in time.")),
        Ok(Err(e)) => return Err(failed(describe(&e))),
        Ok(Ok(response)) => response,
    };

    let status = response.status();
    if !status.is_success() {
        return Err(failed(explain_status(status)));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    // A refusal page, or a consent wall, served in place of the episode with
    // a 200: saved as `.mp3` it would be a file that will not play and cannot
    // be told from a good one in a list.
    if content_type.starts_with("text/") || content_type.starts_with("application/json") {
        return Err(failed("the site sent a web page instead of the episode."));
    }
    // What the feed said the size was is only a guess for the bar to start
    // from; the server's own length wins.
    let total = response.content_length().filter(|n| *n > 0).or(expected.filter(|n| *n > 0));
    if total.is_some_and(|n| n > MAX_BYTES) {
        return Err(failed("that episode is larger than the 4 GB this app will download."));
    }
    job.progress.total.store(total.unwrap_or(0), Ordering::Relaxed);

    let file = std::fs::File::create(to)
        .map_err(|e| failed(format!("could not save the episode: {e}.")))?;
    let mut out = Writer { file: Some(file), buffer: Vec::new() };
    let mut received: u64 = 0;
    loop {
        let piece = tokio::select! {
            piece = tokio::time::timeout(STALL, response.chunk()) => piece,
            () = cancelled(&mut job.cancel) => return Err(Stop::Cancelled),
        };
        let piece = match piece {
            Err(_) => return Err(failed("the download stalled, and was given up.")),
            Ok(Err(e)) => return Err(failed(describe(&e))),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(piece))) => piece,
        };
        received += piece.len() as u64;
        if received > MAX_BYTES {
            return Err(failed("that episode is larger than the 4 GB this app will download."));
        }
        job.progress.received.store(received, Ordering::Relaxed);
        out.push(&piece).await.map_err(|e| failed(format!("could not save the episode: {e}.")))?;
    }
    out.finish().await.map_err(|e| failed(format!("could not save the episode: {e}.")))?;
    if received == 0 {
        return Err(failed("the site sent nothing."));
    }
    Ok(Fetched { content_type })
}

/// Resolves when a job is told to stop, and never if it is not.
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            // Nobody can cancel this any more; the download is for the task
            // that is running it to end.
            std::future::pending::<()>().await;
        }
    }
}

/// A file written a megabyte at a time on the blocking pool, since tokio's
/// own file type is not built into this crate and a thread per piece would
/// be thousands of them.
struct Writer {
    /// `None` only while a write is under way on the pool.
    file: Option<std::fs::File>,
    buffer: Vec<u8>,
}

impl Writer {
    async fn push(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= FLUSH_AT { self.flush().await } else { Ok(()) }
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        let Some(mut file) = self.file.take() else { return Ok(()) };
        let buffer = std::mem::take(&mut self.buffer);
        let (file, written) = tokio::task::spawn_blocking(move || {
            let written = file.write_all(&buffer);
            (file, written)
        })
        .await
        .map_err(std::io::Error::other)?;
        self.file = Some(file);
        written
    }

    async fn finish(mut self) -> std::io::Result<()> {
        self.flush().await
    }
}

/// A transport failure as a sentence, with no address in it: an episode's
/// address can carry a subscriber's token, as a private feed's does.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the site did not answer in time.".to_string()
    } else if e.is_connect() {
        "could not reach the site. Check the network.".to_string()
    } else if e.is_redirect() {
        "the address redirected too many times, or to somewhere it may not.".to_string()
    } else {
        format!("the download failed: {}", http::strip_url(&e.to_string()))
    }
}

/// An answer other than success, as a sentence.
fn explain_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "the site refused the download.".to_string(),
        404 | 410 => "the episode is not there any more.".to_string(),
        429 => "the site asked us to slow down. Try again later.".to_string(),
        500..=599 => format!("the site is having trouble ({status})."),
        _ => format!("the site answered {status}."),
    }
}

// ── The scratch place ────────────────────────────────────────────────────

/// Where an episode is written while it arrives: outside the files folder,
/// which a sync tool may be carrying to every device, and which watches
/// itself for new files. Named for the story, so the same episode fetched
/// again replaces what a crash left of it.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(vault: &Vault, story: StoryId) -> std::io::Result<Self> {
        let dir = scratch_dir(vault);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        // What an episode is called says what somebody listens to: for their
        // eyes, as the folder of a sealed upload is.
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(Self { path: dir.join(format!("{story}.part")) })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    /// Whatever way the download ended -- done, failed, cancelled, or the
    /// task dropped it -- nothing of it is left behind.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn scratch_dir(vault: &Vault) -> PathBuf {
    karmayoga_core::files::disk::scratch_base(vault.path()).join("karmayoga-downloads")
}

/// Remove what a crash left: partial downloads nobody is writing to any
/// more. Only those old enough that no download of this run can be one.
fn clear_stale_scratch(vault: &Vault) {
    let Ok(entries) = std::fs::read_dir(scratch_dir(vault)) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= STALE_SCRATCH);
        if old && entry.path().extension().is_some_and(|e| e == "part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ── What a refresh brings ────────────────────────────────────────────────

/// A refresh of `source` stored `inserted`: apply the show's download and
/// Up Next policies to them. Quick: queues work and wakes the task.
///
/// The lookups -- the source, its policy, the new stories -- are made on the
/// blocking pool by a task of their own, so a refresh that has just stored a
/// hundred stories does not wait on them.
pub fn after_refresh(svc: &Arc<Service>, source: NewsSourceId, inserted: &[StoryId]) {
    if inserted.is_empty() {
        return;
    }
    // Outside a runtime there is nowhere to do it, and nothing to be done
    // about that: a test calling the refresher directly.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let (svc, inserted) = (svc.clone(), inserted.to_vec());
    runtime.spawn(async move {
        if let Err(e) = apply_policies(&svc, source, inserted).await {
            tracing::debug!(error = %e, "podcasts: could not apply a show's policies");
        }
    });
}

/// One new episode, as far as a policy cares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arrival {
    story: StoryId,
    title: String,
    published_at: Timestamp,
    /// It carries something to hear or watch.
    playable: bool,
}

async fn apply_policies(
    svc: &Arc<Service>,
    source: NewsSourceId,
    inserted: Vec<StoryId>,
) -> CommandResult<()> {
    let Some(vault) = svc.get().filter(|v| v.is_unlocked() && v.is_writable()) else {
        return Ok(());
    };
    let looked = blocking({
        let vault = vault.clone();
        move || {
            let found = vault.news_source(source)?;
            let Some(policy) = vault.podcast_policy(&found)? else { return Ok(None) };
            // A source whose every story is one of these has never had
            // another: this is its first look, and it brought the whole of
            // its back catalogue.
            let first = vault.story_keys(source)?.len() <= inserted.len();
            let arrivals: Vec<Arrival> = vault
                .stories_by_id(&inserted)?
                .into_iter()
                .map(|s| Arrival {
                    story: s.id,
                    title: s.title,
                    published_at: s.published_at,
                    playable: s.media.as_ref().is_some_and(|m| m.is_playable()),
                })
                .collect();
            Ok(Some((policy, arrivals, first)))
        }
    })
    .await?;
    let Some((policy, arrivals, first)) = looked else { return Ok(()) };

    let downloads = &svc.listening.downloads;
    // In the order the policy picked them -- newest first -- which is the
    // order they come down in.
    let wanted = to_download(policy.download, &arrivals, first);
    for story in &wanted {
        if let Some(arrival) = arrivals.iter().find(|a| a.story == *story) {
            downloads.queue(arrival.story, arrival.title.clone());
        }
    }
    if !wanted.is_empty() {
        downloads.wake();
    }
    queue_new(svc, &policy, &arrivals, first).await;
    Ok(())
}

/// Put what a show's policy says goes to Up Next there, one at a time and in
/// the order they were published. Up Next asks for each episode's download
/// itself.
async fn queue_new(svc: &Arc<Service>, policy: &PodcastPolicy, arrivals: &[Arrival], first: bool) {
    if !policy.queue_new {
        return;
    }
    for story in to_queue(arrivals, first) {
        let added = queue::add(svc, ContentRef::Story(story), QueuePlace::End, AddedBy::Show).await;
        if let Err(e) = added {
            tracing::debug!(%story, error = %e, "podcasts: could not put a new episode in Up Next");
        }
    }
}

/// Newest first, which is the order an episode is wanted in; the same moment
/// is told apart by id, so the answer does not depend on the order a
/// refresh happened to list them in.
fn newest_first(arrivals: &[Arrival]) -> Vec<&Arrival> {
    let mut ordered: Vec<&Arrival> = arrivals.iter().collect();
    ordered.sort_by(|a, b| b.published_at.cmp(&a.published_at).then(b.story.cmp(&a.story)));
    ordered
}

/// Which of a refresh's new episodes the download policy has downloaded.
///
/// "Every new episode" is capped at the newest [`FIRST_REFRESH_ALL`] on a
/// show's first refresh, since that one brings its whole back catalogue and
/// nobody following a show wants a thousand episodes at once.
fn to_download(policy: DownloadPolicy, arrivals: &[Arrival], first_refresh: bool) -> Vec<StoryId> {
    let limit = match policy {
        DownloadPolicy::Off => return Vec::new(),
        DownloadPolicy::Newest { count } => count as usize,
        DownloadPolicy::All if first_refresh => FIRST_REFRESH_ALL,
        DownloadPolicy::All => usize::MAX,
    };
    newest_first(arrivals).into_iter().filter(|a| a.playable).take(limit).map(|a| a.story).collect()
}

/// Which of a refresh's new episodes go to Up Next, in the order they go:
/// oldest first, so a show heard in order stays in order. On a first refresh
/// only the newest one: its back catalogue is not a backlog to listen to.
fn to_queue(arrivals: &[Arrival], first_refresh: bool) -> Vec<StoryId> {
    let mut playable: Vec<StoryId> =
        newest_first(arrivals).into_iter().filter(|a| a.playable).map(|a| a.story).collect();
    if first_refresh {
        playable.truncate(1);
    }
    playable.reverse();
    playable
}

// ── Asking ───────────────────────────────────────────────────────────────

/// Download one episode, whatever its show's policy says.
///
/// One story is read to learn its title and that it has an episode at all: a
/// single row, which is why this does not go to the blocking pool as the
/// reading of a whole refresh does. An episode already in the files folder is
/// not asked for again.
pub fn request(svc: &Arc<Service>, story: StoryId) -> CommandResult<()> {
    let vault = svc.require_unlocked()?;
    if !vault.is_writable() {
        return Err(CommandError::new(
            codes::VAULT_IN_USE,
            "another copy of Karma Yoga has this vault open for writing, so nothing can be \
             downloaded here.",
        ));
    }
    let found = vault.story(story)?;
    if found.media.is_none() {
        return Err(CommandError::new(codes::INVALID, "that story has no episode to download."));
    }
    if found.files.media.is_some_and(|file| is_present(&vault, file)) {
        return Ok(());
    }
    svc.listening.downloads.queue(story, found.title);
    svc.listening.downloads.wake();
    Ok(())
}

/// Stop downloading one episode, or take it off the waiting list.
pub fn cancel(svc: &Arc<Service>, story: StoryId) {
    svc.listening.downloads.cancel(story);
}

/// Everything waiting, under way, or failed since it was last asked for.
pub fn status(svc: &Service) -> Vec<DownloadStatus> {
    svc.listening.downloads.statuses()
}

/// Move one episode's downloaded file to the files trash and unlink it.
///
/// An explicit act, so it takes the file whoever put it there and whether or
/// not it is a favourite; the keep policies are the ones that are careful.
/// A file already gone from the folder, or already in the trash, is just
/// unlinked. The transcript and anything kept of the article are left alone.
pub async fn remove_download(svc: &Arc<Service>, story: StoryId) -> CommandResult<()> {
    let vault = svc.require()?;
    // One that is still coming down is stopped first, and waited for: a
    // download told to stop may already be past its last look at whether it
    // has been, copying the file in and linking it, and would otherwise put
    // the episode back the moment it was removed.
    svc.listening.downloads.cancel(story);
    svc.listening.downloads.wait_until_idle(story).await;
    let removed = blocking({
        let vault = vault.clone();
        move || {
            let Some(media) = vault.story(story)?.files.media else { return Ok(None) };
            let trashed = match vault.file(media) {
                Ok(file) if matches!(file.status, FileStatus::Present) => {
                    vault.trash_files(&[media])?;
                    true
                }
                Ok(_) | Err(karmayoga_core::Error::NotFound { .. }) => false,
                Err(e) => return Err(e.into()),
            };
            vault.update_story_files(story, |files| {
                if files.media == Some(media) {
                    files.media = None;
                }
            })?;
            Ok(Some((media, trashed)))
        }
    })
    .await?;
    if let Some((file, trashed)) = removed {
        announce(svc, Kind::Story, Op::Updated, vec![story.to_string()]);
        if trashed {
            files::announce(svc, vec![file]);
        }
    }
    Ok(())
}

// ── Clearing out ─────────────────────────────────────────────────────────

/// Apply every show's keep policy. Answers how many files went to the trash.
///
/// Only an episode the Feed downloaded itself -- origin `feed`, still linked
/// as the story's `media` -- and never one that is a favourite, whose story
/// is saved or in Up Next, or that is gone from the folder already. A
/// transcript, a kept article or its audio is not touched: those are the
/// Feed's memory of what was heard.
///
/// It also unlinks what points at a file somebody trashed or deleted in Files,
/// which no policy would otherwise ever notice ([`link_is_dead`]): a story from
/// its episode, its transcript and its audio, and a recording from the
/// transcript its properties name, so that the interface stops offering to open
/// or play what is not there and offers to make it again.
pub async fn sweep(svc: &Arc<Service>) -> CommandResult<u32> {
    let vault = svc.require()?;
    if !vault.is_writable() || !vault.supports_news() {
        return Ok(0);
    }
    let now = svc.now();
    let swept = blocking({
        let vault = vault.clone();
        move || clear_out(&vault, now)
    })
    .await?;
    let changed: Vec<String> =
        swept.stories.iter().chain(&swept.dangling).map(|s| s.to_string()).collect();
    announce(svc, Kind::Story, Op::Updated, changed);
    files::announce(svc, swept.files.into_iter().chain(swept.recordings).collect());
    Ok(swept.stories.len() as u32)
}

/// What a sweep did.
#[derive(Default)]
struct Swept {
    /// Stories whose episode went to the trash.
    stories: Vec<StoryId>,
    /// Those episodes' files.
    files: Vec<FileId>,
    /// Stories unlinked from a file that was already gone: the episode, or
    /// its transcript, audio or summary.
    dangling: Vec<StoryId>,
    /// Recordings whose property naming a transcript was taken off, because
    /// the transcript is gone.
    recordings: Vec<FileId>,
}

/// One downloaded episode, as far as a keep policy cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    story: StoryId,
    published_at: Timestamp,
    /// When it was played to the end, if it was.
    finished_at: Option<Timestamp>,
    /// When its file was added to the files folder.
    downloaded_at: Timestamp,
    /// Never cleared: a favourite, or its story is saved or in Up Next.
    kept: bool,
}

/// Which of one show's downloaded episodes its keep policy clears out.
///
/// "Newest N" counts the episodes that are kept too: they are downloaded, and
/// the N newest of the show are what stays. One downloaded in the last day
/// stays whatever its age ([`FRESH_SECS`]).
fn to_clear(keep: KeepPolicy, held: &[Held], now: Timestamp) -> Vec<StoryId> {
    match keep {
        KeepPolicy::Forever => Vec::new(),
        KeepPolicy::AfterPlayed { days } => {
            let after = i64::from(days) * 24 * 60 * 60;
            held.iter()
                .filter(|h| !h.kept)
                .filter(|h| {
                    h.finished_at.is_some_and(|at| now.as_second() - at.as_second() > after)
                })
                .map(|h| h.story)
                .collect()
        }
        KeepPolicy::Newest { count } => {
            let mut ordered: Vec<&Held> = held.iter().collect();
            ordered.sort_by(|a, b| b.published_at.cmp(&a.published_at).then(b.story.cmp(&a.story)));
            ordered
                .into_iter()
                .skip(count as usize)
                .filter(|h| !h.kept)
                .filter(|h| now.as_second() - h.downloaded_at.as_second() > FRESH_SECS)
                .map(|h| h.story)
                .collect()
        }
    }
}

/// The blocking half of [`sweep`].
fn clear_out(vault: &Vault, now: Timestamp) -> CommandResult<Swept> {
    let mut swept = Swept::default();
    // Every story that has any file, which is the few that were played or
    // transcribed and not the thousands that were only read.
    let stories = vault.stories_with_files()?;
    let linked: Vec<FileId> = stories.iter().flat_map(|s| s.files.all()).collect();
    let files = catalog_entries(vault, &linked)?;

    // A link to a file that is gone from the catalog, in the trash, or has
    // been missing from the folder for good, is a story that offers a
    // download, a transcript or a recording to hear that it does not have --
    // and, for an episode, that never expires for want of knowing.
    let mut live = Vec::with_capacity(stories.len());
    for story in stories {
        let lost = lost_links(&story.files, &files, now);
        if lost.is_empty() {
            if story.files.media.is_some() {
                live.push(story);
            }
            continue;
        }
        // Only what it still says: a download may have linked another file
        // since the stories were read.
        let unlinked = vault.update_story_files(story.id, |files| lost.unlink(files));
        match unlinked {
            Ok(_) => swept.dangling.push(story.id),
            Err(e) => tracing::warn!(error = %e, "podcasts: could not unlink a lost file"),
        }
        if lost.media.is_none() && story.files.media.is_some() {
            live.push(story);
        }
    }
    unlink_lost_transcripts(vault, now, &mut swept)?;
    if live.is_empty() {
        return Ok(swept);
    }

    let mut policies: HashMap<NewsSourceId, PodcastPolicy> = HashMap::new();
    for source in vault.news_sources()? {
        if let Some(policy) = vault.podcast_policy(&source)? {
            policies.insert(source.id, policy);
        }
    }
    let stories: Vec<_> =
        live.into_iter().filter(|s| policies.contains_key(&s.source_id)).collect();
    if stories.is_empty() {
        return Ok(swept);
    }
    let queued: HashSet<StoryId> = vault
        .queue()?
        .into_iter()
        .filter_map(|e| match e.what {
            ContentRef::Story(id) => Some(id),
            _ => None,
        })
        .collect();
    // Only a policy that goes by what was played needs to know.
    let asked: Vec<ContentRef> = stories
        .iter()
        .filter(|s| matches!(policies[&s.source_id].keep, KeepPolicy::AfterPlayed { .. }))
        .map(|s| ContentRef::Story(s.id))
        .collect();
    let finished: HashMap<StoryId, Timestamp> = if asked.is_empty() {
        HashMap::new()
    } else {
        vault
            .consumptions_for(&asked)?
            .into_iter()
            .filter(|c| c.finished)
            .filter_map(|c| match c.what {
                ContentRef::Story(id) => Some((id, c.finished_at.unwrap_or(c.last_at))),
                _ => None,
            })
            .collect()
    };

    let mut shows: HashMap<NewsSourceId, Vec<(Held, FileId)>> = HashMap::new();
    for story in stories {
        let Some(media) = story.files.media else { continue };
        // Not the Feed's own: not for it to take.
        let Some(file) = files.get(&media).filter(|f| f.origin == FileOrigin::Feed) else {
            continue;
        };
        // Missing from the folder, but not for long enough to be given up:
        // nothing to move.
        if !matches!(file.status, FileStatus::Present) {
            continue;
        }
        let held = Held {
            story: story.id,
            published_at: story.published_at,
            finished_at: finished.get(&story.id).copied(),
            downloaded_at: file.added_at,
            kept: file.favourite || story.saved || queued.contains(&story.id),
        };
        shows.entry(story.source_id).or_default().push((held, media));
    }

    for (source, episodes) in shows {
        let held: Vec<Held> = episodes.iter().map(|(h, _)| *h).collect();
        let clear: HashSet<StoryId> =
            to_clear(policies[&source].keep, &held, now).into_iter().collect();
        for (held, file) in episodes.into_iter().filter(|(h, _)| clear.contains(&h.story)) {
            if let Err(e) = vault.trash_files(&[file]) {
                tracing::warn!(error = %e, "podcasts: could not move an old episode to the trash");
                continue;
            }
            let unlinked = vault.update_story_files(held.story, |files| {
                if files.media == Some(file) {
                    files.media = None;
                }
            });
            if let Err(e) = unlinked {
                tracing::warn!(error = %e, "podcasts: could not unlink an old episode");
            }
            swept.stories.push(held.story);
            swept.files.push(file);
        }
    }
    Ok(swept)
}

/// The links of one story that lead nowhere any more.
#[derive(Debug, Default, PartialEq, Eq)]
struct Lost {
    media: Option<FileId>,
    transcript: Option<FileId>,
    text: Option<FileId>,
    audio: Option<FileId>,
    summary: Option<FileId>,
}

impl Lost {
    fn is_empty(&self) -> bool {
        *self == Lost::default()
    }

    /// Take the lost links off `files`: each only if it still names the file
    /// that was found lost.
    fn unlink(&self, files: &mut StoryFiles) {
        for (slot, lost) in [
            (&mut files.media, self.media),
            (&mut files.transcript, self.transcript),
            (&mut files.text, self.text),
            (&mut files.audio, self.audio),
            (&mut files.summary, self.summary),
        ] {
            if lost.is_some() && *slot == lost {
                *slot = None;
            }
        }
    }
}

/// Which of a story's links lead nowhere, by `catalog`.
///
/// The episode, the transcript and the audio are lost when their file is, by
/// [`link_is_dead`]. The kept article is not, until it is gone from the catalog
/// altogether: putting it in the trash is somebody saying they do not want it
/// kept, and reading the article again must not write it out again behind their
/// back ([`super::keep`]). A summary is treated as the kept article is: one
/// somebody binned is not unlinked from its story by the next sweep, and the
/// story's tab, which finds the file in the trash, says there is none.
fn lost_links(files: &StoryFiles, catalog: &HashMap<FileId, FileEntry>, now: Timestamp) -> Lost {
    let dead = |file: Option<FileId>| file.filter(|f| link_is_dead(catalog.get(f), now));
    Lost {
        media: dead(files.media),
        transcript: dead(files.transcript),
        text: files.text.filter(|f| !catalog.contains_key(f)),
        audio: dead(files.audio),
        summary: files.summary.filter(|f| !catalog.contains_key(f)),
    }
}

/// Take the property that names a transcript off every recording whose
/// transcript is lost. A recording that is not an episode has no story to say
/// it has one, and its properties are all that does.
///
/// Every audio and video file is looked at, which is what a catalog can answer
/// without an index on properties; the few that name a transcript are then read
/// from the catalog.
fn unlink_lost_transcripts(vault: &Vault, now: Timestamp, swept: &mut Swept) -> CommandResult<()> {
    let recordings = vault.files(&FileQuery {
        filter: FileFilter {
            kinds: vec![FileKind::Audio, FileKind::Video],
            status: StatusFilter::Live,
            include_hidden: true,
            ..FileFilter::default()
        },
        ..FileQuery::default()
    })?;
    let named: Vec<(FileId, FileId)> = recordings
        .files
        .iter()
        .filter_map(|recording| match recording.properties.get(TRANSCRIPT_PROPERTY) {
            Some(PropValue::Text(id)) => FileId::parse(id).ok().map(|t| (recording.id, t)),
            _ => None,
        })
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    let transcripts: Vec<FileId> = named.iter().map(|(_, transcript)| *transcript).collect();
    let catalog = catalog_entries(vault, &transcripts)?;
    for (recording, transcript) in named {
        if !link_is_dead(catalog.get(&transcript), now) {
            continue;
        }
        let update = FileUpdate {
            properties: [(TRANSCRIPT_PROPERTY.to_string(), None)].into(),
            ..FileUpdate::default()
        };
        match vault.update_files(&[recording], &update) {
            Ok(_) => swept.recordings.push(recording),
            Err(e) => tracing::warn!(error = %e, "podcasts: could not forget a lost transcript"),
        }
    }
    Ok(())
}

/// The catalog's records for `ids`, in as few reads as the store allows.
/// Trashed and missing files are included; an id the catalog does not have is
/// not in the answer.
fn catalog_entries(vault: &Vault, ids: &[FileId]) -> CommandResult<HashMap<FileId, FileEntry>> {
    let mut found = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(CATALOG_BATCH) {
        let query = FileQuery {
            filter: FileFilter {
                ids: Some(chunk.to_vec()),
                status: StatusFilter::All,
                include_hidden: true,
                ..FileFilter::default()
            },
            ..FileQuery::default()
        };
        for file in vault.files(&query)?.files {
            found.insert(file.id, file);
        }
    }
    Ok(found)
}

/// Whether a story's link to its episode leads nowhere any more: the file is
/// not in the catalog, or is in the trash, or has been missing from the
/// folder for longer than [`MISSING_GRACE_SECS`].
///
/// A file that is missing is given time, where a trashed or deleted one is
/// not: a folder a sync tool is still filling, or a drive that was away, is
/// missing files that come back, and an episode unlinked meanwhile would be
/// downloaded a second time.
fn link_is_dead(file: Option<&FileEntry>, now: Timestamp) -> bool {
    match file.map(|f| &f.status) {
        None | Some(FileStatus::Trashed { .. }) => true,
        Some(FileStatus::Present) => false,
        Some(FileStatus::Missing { since }) => {
            now.as_second() - since.as_second() > MISSING_GRACE_SECS
        }
    }
}

// ── Names ────────────────────────────────────────────────────────────────

/// `text` as one segment of a path: what a file system refuses is taken out
/// or replaced, whitespace is one space, it neither begins nor ends in a dot
/// or a space, and it is at most `max_bytes` long. `fallback` when nothing
/// is left.
///
/// More careful than the files folder requires on Linux, because the folder
/// may be carried to other systems by a sync tool, and a show or an episode
/// whose title has a colon in it is the ordinary case.
pub(crate) fn safe_name(text: &str, max_bytes: usize, fallback: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' | '\\' => out.push('-'),
            // "Episode 12: The thing" reads best as "Episode 12 - The thing".
            ':' => out.push_str(if chars.peek().is_some_and(|n| n.is_whitespace()) {
                " -"
            } else {
                "-"
            }),
            '*' | '?' | '"' | '<' | '>' | '|' => {}
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    let words = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut name = words.trim_matches(['.', ' ']);
    if name.len() > max_bytes {
        let mut end = max_bytes;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name = name[..end].trim_matches(['.', ' ']);
    }
    if name.is_empty() { fallback.to_string() } else { name.to_string() }
}

/// The folder a show's episodes go in, relative to the files folder.
fn show_dir(show: &str) -> String {
    format!("{PODCASTS_DIR}/{}", safe_name(show, SHOW_BYTES, "Podcast"))
}

/// An episode's file name: its date, which keeps a show's folder in the order
/// the episodes came out, then its title.
fn episode_name(published: Date, title: &str, extension: &str) -> String {
    format!(
        "{:04}-{:02}-{:02} {}.{extension}",
        published.year(),
        published.month(),
        published.day(),
        safe_name(title, TITLE_BYTES, "Episode")
    )
}

/// The extension an episode's file gets: the address's own when it ends in
/// one that is audio or video -- a publisher's file name is the most reliable
/// word there is on what the file is -- else what the type says, else
/// whatever the enclosure is most likely to be.
fn extension_for(url: &str, mime: &str, kind: EnclosureKind) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let file = path.rsplit('/').next().unwrap_or("");
    if let Some((_, ext)) = file.rsplit_once('.') {
        let ext = ext.to_ascii_lowercase();
        if let Some(known) = MEDIA_EXTENSIONS.iter().find(|k| **k == ext) {
            return known;
        }
    }
    let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match mime.as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" | "audio/aacp" => "aac",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "video/mp4" => "mp4",
        "video/x-m4v" => "m4v",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        _ => match kind {
            EnclosureKind::Audio => "mp3",
            EnclosureKind::Video => "mp4",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn arrival(title: &str, published: &str, playable: bool) -> Arrival {
        Arrival {
            story: StoryId::new(),
            title: title.into(),
            published_at: at(published),
            playable,
        }
    }

    fn titles(arrivals: &[Arrival], ids: &[StoryId]) -> Vec<String> {
        ids.iter()
            .map(|id| arrivals.iter().find(|a| a.story == *id).unwrap().title.clone())
            .collect()
    }

    // ── Names ────────────────────────────────────────────────────────────

    #[test]
    fn a_name_is_made_safe_for_a_folder() {
        assert_eq!(safe_name("Episode 12: The Thing", 100, "x"), "Episode 12 - The Thing");
        assert_eq!(safe_name("10:30 news", 100, "x"), "10-30 news");
        assert_eq!(safe_name("AC/DC \\ Back", 100, "x"), "AC-DC - Back");
        assert_eq!(safe_name("What? \"Really\" <now> *|", 100, "x"), "What Really now");
        assert_eq!(safe_name("  many   spaces\tand\nlines ", 100, "x"), "many spaces and lines");
        // Nothing to hide a file under, and nothing to end a name on that
        // another system would change.
        assert_eq!(safe_name("..hidden..", 100, "x"), "hidden");
        assert_eq!(safe_name("ends with a dot. ", 100, "x"), "ends with a dot");
        assert_eq!(safe_name("", 100, "Episode"), "Episode");
        assert_eq!(safe_name("?*", 100, "Podcast"), "Podcast");
    }

    #[test]
    fn a_long_name_is_cut_on_a_letter_not_through_one() {
        let cut = safe_name(&"é".repeat(100), 11, "x");
        assert_eq!(cut, "é".repeat(5), "ten bytes, not eleven cut through the sixth");
        assert_eq!(safe_name("abc def", 4, "x"), "abc");
    }

    #[test]
    fn an_episode_is_named_for_its_date_and_title() {
        let date = Date::new(2026, 3, 9).unwrap();
        assert_eq!(episode_name(date, "Why: a test", "mp3"), "2026-03-09 Why - a test.mp3");
        assert_eq!(episode_name(date, "", "m4a"), "2026-03-09 Episode.m4a");
        assert_eq!(show_dir("The Daily: Special"), "Feed/Podcasts/The Daily - Special");
        assert_eq!(show_dir("///"), "Feed/Podcasts/---");
        assert_eq!(show_dir("?"), "Feed/Podcasts/Podcast");
    }

    #[test]
    fn the_extension_comes_from_the_address_then_the_type_then_the_kind() {
        let ext = |url: &str, mime: &str, kind| extension_for(url, mime, kind);
        let audio = EnclosureKind::Audio;
        assert_eq!(ext("https://cdn.example/a/ep1.M4A?x=1.mp3", "", audio), "m4a");
        assert_eq!(
            ext("https://dts.podtrac.com/redirect.mp3/host.example/abc.mp3?updated=1", "", audio),
            "mp3"
        );
        // An address that says nothing: what the feed or the server said.
        assert_eq!(ext("https://example.com/download?id=7", "audio/mp4", audio), "m4a");
        assert_eq!(ext("https://example.com/e/12", "audio/mpeg; charset=binary", audio), "mp3");
        assert_eq!(ext("https://example.com/e/12", "video/quicktime", EnclosureKind::Video), "mov");
        assert_eq!(ext("https://example.com/e/12", "audio/ogg", audio), "ogg");
        // An extension that is not audio or video is not trusted.
        assert_eq!(ext("https://example.com/e/12.php", "audio/opus", audio), "opus");
        // Nothing said at all.
        assert_eq!(ext("https://example.com/e/12", "", audio), "mp3");
        assert_eq!(
            ext("https://example.com/e/12", "application/octet-stream", EnclosureKind::Video),
            "mp4"
        );
    }

    // ── What a refresh brings ────────────────────────────────────────────

    #[test]
    fn the_download_policy_picks_the_newest_that_can_be_played() {
        let arrivals = [
            arrival("old", "2026-01-01T00:00:00Z", true),
            arrival("newest", "2026-01-04T00:00:00Z", true),
            arrival("an article", "2026-01-05T00:00:00Z", false),
            arrival("middle", "2026-01-03T00:00:00Z", true),
            arrival("older", "2026-01-02T00:00:00Z", true),
        ];
        let picked = |policy, first| titles(&arrivals, &to_download(policy, &arrivals, first));
        assert!(picked(DownloadPolicy::Off, false).is_empty());
        assert_eq!(picked(DownloadPolicy::Newest { count: 2 }, false), ["newest", "middle"]);
        assert_eq!(
            picked(DownloadPolicy::All, false),
            ["newest", "middle", "older", "old"],
            "everything that plays, newest first, and no article"
        );
        assert!(picked(DownloadPolicy::Newest { count: 0 }, false).is_empty());
        // Newest N is N however old the show is; All is capped on the first look.
        assert_eq!(picked(DownloadPolicy::Newest { count: 4 }, true).len(), 4);
        assert_eq!(picked(DownloadPolicy::All, true), ["newest", "middle", "older"]);
    }

    #[test]
    fn what_goes_to_up_next_is_oldest_first_and_only_the_newest_on_a_first_look() {
        let arrivals = [
            arrival("b", "2026-01-02T00:00:00Z", true),
            arrival("c", "2026-01-03T00:00:00Z", true),
            arrival("note", "2026-01-04T00:00:00Z", false),
            arrival("a", "2026-01-01T00:00:00Z", true),
        ];
        assert_eq!(titles(&arrivals, &to_queue(&arrivals, false)), ["a", "b", "c"]);
        assert_eq!(titles(&arrivals, &to_queue(&arrivals, true)), ["c"]);
        assert!(to_queue(&[], false).is_empty());
    }

    // ── Clearing out ─────────────────────────────────────────────────────

    fn held(published: &str, finished: Option<&str>, downloaded: &str, kept: bool) -> Held {
        Held {
            story: StoryId::new(),
            published_at: at(published),
            finished_at: finished.map(at),
            downloaded_at: at(downloaded),
            kept,
        }
    }

    #[test]
    fn nothing_is_cleared_when_everything_is_kept() {
        let now = at("2026-06-01T00:00:00Z");
        let episodes = [held(
            "2026-01-01T00:00:00Z",
            Some("2026-01-02T00:00:00Z"),
            "2026-01-01T00:00:00Z",
            false,
        )];
        assert!(to_clear(KeepPolicy::Forever, &episodes, now).is_empty());
    }

    #[test]
    fn an_episode_is_cleared_a_number_of_days_after_it_was_played_to_the_end() {
        let now = at("2026-06-10T12:00:00Z");
        let long_ago = "2026-01-01T00:00:00Z";
        let played_long_ago = held(long_ago, Some("2026-06-01T00:00:00Z"), long_ago, false);
        let played_just_now = held(long_ago, Some("2026-06-08T00:00:00Z"), long_ago, false);
        let never_played = held(long_ago, None, long_ago, false);
        let saved = held(long_ago, Some("2026-06-01T00:00:00Z"), long_ago, true);
        let episodes = [played_long_ago, played_just_now, never_played, saved];
        assert_eq!(
            to_clear(KeepPolicy::AfterPlayed { days: 7 }, &episodes, now),
            [played_long_ago.story],
            "a week and a day is more than a week; two days is not; unplayed and kept stay"
        );
        // Zero days: as soon as it is over.
        assert_eq!(
            to_clear(KeepPolicy::AfterPlayed { days: 0 }, &episodes, now),
            [played_long_ago.story, played_just_now.story]
        );
        // On the very day, not yet: "more than" the days.
        let today = held(long_ago, Some("2026-06-10T12:00:00Z"), long_ago, false);
        assert!(to_clear(KeepPolicy::AfterPlayed { days: 0 }, &[today], now).is_empty());
    }

    #[test]
    fn the_newest_few_of_a_show_are_kept_and_the_rest_cleared() {
        let now = at("2026-06-10T00:00:00Z");
        let old = "2026-05-01T00:00:00Z";
        let one = held("2026-01-01T00:00:00Z", None, old, false);
        let two = held("2026-01-02T00:00:00Z", None, old, false);
        let three = held("2026-01-03T00:00:00Z", None, old, false);
        let four = held("2026-01-04T00:00:00Z", None, old, false);
        let episodes = [two, four, one, three];
        assert_eq!(
            to_clear(KeepPolicy::Newest { count: 2 }, &episodes, now),
            [two.story, one.story]
        );
        assert!(to_clear(KeepPolicy::Newest { count: 4 }, &episodes, now).is_empty());
        assert!(to_clear(KeepPolicy::Newest { count: 9 }, &episodes, now).is_empty());
        assert_eq!(to_clear(KeepPolicy::Newest { count: 0 }, &episodes, now).len(), 4);
    }

    #[test]
    fn a_kept_episode_stays_and_takes_a_place_among_the_newest() {
        let now = at("2026-06-10T00:00:00Z");
        let old = "2026-05-01T00:00:00Z";
        let kept_old = held("2026-01-01T00:00:00Z", None, old, true);
        let two = held("2026-01-02T00:00:00Z", None, old, false);
        let three = held("2026-01-03T00:00:00Z", None, old, false);
        let cleared = to_clear(KeepPolicy::Newest { count: 2 }, &[kept_old, two, three], now);
        assert!(cleared.is_empty(), "two are the newest; the third is old but kept");
        let cleared = to_clear(KeepPolicy::Newest { count: 1 }, &[kept_old, two, three], now);
        assert_eq!(cleared, [two.story]);
    }

    #[test]
    fn an_episode_downloaded_in_the_last_day_is_not_taken_by_keeping_the_newest() {
        let now = at("2026-06-10T12:00:00Z");
        let just_asked_for = held("2026-01-01T00:00:00Z", None, "2026-06-10T11:00:00Z", false);
        let newer = held("2026-01-02T00:00:00Z", None, "2026-05-01T00:00:00Z", false);
        assert!(
            to_clear(KeepPolicy::Newest { count: 1 }, &[just_asked_for, newer], now).is_empty()
        );
        // A day later it is just an old episode.
        let later = at("2026-06-11T12:00:01Z");
        assert_eq!(
            to_clear(KeepPolicy::Newest { count: 1 }, &[just_asked_for, newer], later),
            [just_asked_for.story]
        );
    }

    // ── The list ─────────────────────────────────────────────────────────

    #[test]
    fn an_episode_is_on_the_list_once_and_a_failure_is_forgotten_when_it_is_asked_for_again() {
        let list = Downloads::default();
        let (a, b) = (StoryId::new(), StoryId::new());
        assert!(list.queue(a, "A".into()));
        assert!(list.queue(b, "B".into()));
        assert!(!list.queue(a, "A again".into()), "already waiting");

        let job = list.start_next().expect("a job");
        assert_eq!(job.story, a, "first in, first out");
        assert!(!list.queue(a, "A".into()), "and not again while it is under way");
        let states: Vec<_> = list.statuses().iter().map(|s| (s.story_id, s.state)).collect();
        assert_eq!(states, [(a, DownloadState::Downloading), (b, DownloadState::Waiting)]);

        job.progress.total.store(1000, Ordering::Relaxed);
        job.progress.received.store(250, Ordering::Relaxed);
        let now = list.statuses();
        assert_eq!((now[0].received, now[0].total), (250, Some(1000)));
        assert_eq!(now[1].total, None);

        list.end(a, Ended::Failed("the site refused the download.".into()));
        let after = list.statuses();
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].state, DownloadState::Failed);
        assert_eq!(after[1].error.as_deref(), Some("the site refused the download."));

        assert!(list.queue(a, "A".into()), "asked for again: a retry");
        let again: Vec<_> = list.statuses().iter().map(|s| (s.story_id, s.state)).collect();
        assert_eq!(again, [(b, DownloadState::Waiting), (a, DownloadState::Waiting)]);
    }

    #[test]
    fn cancelling_takes_an_episode_off_the_list_and_tells_one_under_way() {
        let list = Downloads::default();
        let (a, b, c) = (StoryId::new(), StoryId::new(), StoryId::new());
        for (id, title) in [(a, "A"), (b, "B"), (c, "C")] {
            list.queue(id, title.into());
        }
        let mut job = list.start_next().unwrap();
        list.cancel(b);
        assert_eq!(list.statuses().len(), 2, "B left the list");
        assert!(!*job.cancel.borrow());
        list.cancel(a);
        assert!(*job.cancel.borrow_and_update(), "the one under way is told");
        list.end(a, Ended::Cancelled);
        assert!(
            list.statuses().iter().all(|s| s.story_id == c),
            "a cancelled one is not a failure"
        );
    }

    #[test]
    fn what_was_under_way_when_the_task_stopped_starts_again_first() {
        let list = Downloads::default();
        let (a, b, c) = (StoryId::new(), StoryId::new(), StoryId::new());
        for (id, title) in [(a, "A"), (b, "B"), (c, "C")] {
            list.queue(id, title.into());
        }
        let _one = list.start_next().unwrap();
        let _two = list.start_next().unwrap();
        list.requeue_interrupted();
        let order: Vec<_> = list.statuses().iter().map(|s| (s.story_id, s.state)).collect();
        assert_eq!(
            order,
            [(a, DownloadState::Waiting), (b, DownloadState::Waiting), (c, DownloadState::Waiting)]
        );
    }

    /// Something that must happen within a few seconds, or the test is stuck.
    async fn timely<F: std::future::Future>(what: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(5), what).await.expect("it did not happen")
    }

    #[tokio::test]
    async fn something_waiting_for_an_episode_is_let_go_when_its_download_ends() {
        let list = Arc::new(Downloads::default());
        let story = StoryId::new();
        timely(list.wait_until_idle(story)).await;

        list.queue(story, "A".into());
        timely(list.wait_until_idle(story)).await;
        let _job = list.start_next().unwrap();
        let waiter = tokio::spawn({
            let list = list.clone();
            async move { list.wait_until_idle(story).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "it is under way");
        list.end(story, Ended::Cancelled);
        timely(waiter).await.unwrap();

        // And by the lock, which ends everything at once.
        list.queue(story, "A".into());
        let _job = list.start_next().unwrap();
        let waiter = tokio::spawn({
            let list = list.clone();
            async move { list.wait_until_idle(story).await }
        });
        tokio::task::yield_now().await;
        list.clear();
        timely(waiter).await.unwrap();
    }

    #[test]
    fn what_a_cancelled_download_was_doing_is_not_asked_for_again_when_the_task_restarts() {
        let list = Downloads::default();
        let (a, b) = (StoryId::new(), StoryId::new());
        list.queue(a, "A".into());
        list.queue(b, "B".into());
        let _one = list.start_next().unwrap();
        let _two = list.start_next().unwrap();
        list.cancel(a);
        list.requeue_interrupted();
        let order: Vec<_> = list.statuses().iter().map(|s| s.story_id).collect();
        assert_eq!(order, [b]);
    }

    #[test]
    fn the_failures_kept_are_bounded_and_a_lock_forgets_everything() {
        let list = Downloads::default();
        for n in 0..(MAX_FAILED + 5) {
            let id = StoryId::new();
            list.queue(id, format!("Episode {n}"));
            let _ = list.start_next().unwrap();
            list.end(id, Ended::Failed("no".into()));
        }
        let kept = list.statuses();
        assert_eq!(kept.len(), MAX_FAILED);
        assert_eq!(kept[0].title, "Episode 5", "the oldest went first");
        list.queue(StoryId::new(), "Waiting".into());
        list.clear();
        assert!(list.statuses().is_empty());
    }
}

/// The downloader against a server on loopback and a vault in a temporary
/// directory: what only the whole of it decides -- where the file lands, that
/// the story is linked to it, what is said when it goes wrong, and what the
/// clean-up will not touch.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::events::{Change, EventSink};
    use karmayoga_core::VaultConfig;
    use karmayoga_core::news::{
        ConsumeMode, Consumption, Enclosure, NewsSource, PodcastSettings, SourceKind, Story,
    };
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // ── A server for episodes ────────────────────────────────────────────

    /// What the server sends for one path.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
        /// Send this much of the body and then say nothing, with the
        /// connection open: a download that has stopped coming.
        stall_after: Option<usize>,
    }

    impl Reply {
        fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
            Self { status: 200, content_type, body, stall_after: None }
        }

        fn status(status: u16) -> Self {
            Self { status, content_type: "text/plain", body: Vec::new(), stall_after: None }
        }
    }

    /// A server on loopback. Dropping the handle stops it.
    struct Server {
        port: u16,
        hits: Arc<AtomicUsize>,
        _stop: watch::Sender<bool>,
    }

    impl Server {
        async fn start<const N: usize>(routes: [(&'static str, Reply); N]) -> Server {
            let routes: Arc<HashMap<&'static str, Reply>> = Arc::new(routes.into_iter().collect());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let hits = Arc::new(AtomicUsize::new(0));
            let (stop, mut stopped) = watch::channel(false);
            let counted = hits.clone();
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        accepted = listener.accept() => accepted,
                        _ = stopped.changed() => break,
                    };
                    let Ok((mut socket, _)) = accepted else { break };
                    let (routes, counted, mut stopped) =
                        (routes.clone(), counted.clone(), stopped.clone());
                    tokio::spawn(async move {
                        let mut head = Vec::new();
                        let mut buffer = [0u8; 1024];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match socket.read(&mut buffer).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buffer[..n]),
                            }
                        }
                        counted.fetch_add(1, Ordering::SeqCst);
                        let text = String::from_utf8_lossy(&head).into_owned();
                        let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                        let reply =
                            routes.get(path.as_str()).cloned().unwrap_or(Reply::status(404));
                        let header = format!(
                            "HTTP/1.1 {} Status\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
                             Connection: close\r\n\r\n",
                            reply.status,
                            reply.content_type,
                            reply.body.len()
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                        match reply.stall_after {
                            Some(sent) => {
                                let _ = socket.write_all(&reply.body[..sent]).await;
                                let _ = socket.flush().await;
                                // Held open until the server is dropped.
                                let _ = stopped.changed().await;
                            }
                            None => {
                                let _ = socket.write_all(&reply.body).await;
                                let _ = socket.shutdown().await;
                            }
                        }
                    });
                }
            });
            Server { port, hits, _stop: stop }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    /// The way an episode is reached in these tests: a client that will talk
    /// to loopback, and to no proxy.
    fn anywhere() -> Route {
        Route::Anywhere(reqwest::Client::builder().no_proxy().build().unwrap())
    }

    // ── A vault with a files folder ──────────────────────────────────────

    #[derive(Default)]
    struct Collector {
        changes: Mutex<Vec<Change>>,
    }

    impl EventSink for Collector {
        fn changed(&self, change: Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    impl Collector {
        fn count(&self, kind: Kind, op: Op) -> usize {
            self.changes.lock().unwrap().iter().filter(|c| c.kind == kind && c.op == op).count()
        }
    }

    struct Fixture {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        events: Arc<Collector>,
        folder: tempfile::TempDir,
        _home: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        let events = Arc::new(Collector::default());
        svc.set_events(events.clone());
        let vault = svc.set(vault);
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        Fixture { svc, vault, events, folder, _home: home }
    }

    fn follow(vault: &Vault, title: &str, settings: Option<PodcastSettings>) -> NewsSource {
        let kind = SourceKind::Web { url: format!("https://example.com/{title}.xml") };
        let mut source = NewsSource::new(kind, title);
        source.podcast = settings;
        vault.save_news_source(&source).unwrap();
        source
    }

    fn story(vault: &Vault, source: &NewsSource, title: &str, published: &str) -> Story {
        let story =
            Story::new(source.id, format!("key {title}"), title, published.parse().unwrap());
        vault.insert_stories(&[(story.clone(), None)]).unwrap();
        story
    }

    fn episode(
        vault: &Vault,
        source: &NewsSource,
        title: &str,
        published: &str,
        url: &str,
    ) -> Story {
        let mut story =
            Story::new(source.id, format!("key {title}"), title, published.parse().unwrap());
        story.media = Some(Enclosure {
            url: url.into(),
            mime: "audio/mpeg".into(),
            length: None,
            duration_ms: None,
            transcript_url: String::new(),
            transcript_mime: String::new(),
            episode: None,
            season: None,
        });
        vault.insert_stories(&[(story.clone(), None)]).unwrap();
        story
    }

    /// Ask for an episode and run its download to the end.
    async fn fetch(f: &Fixture, story: &Story, route: Route) {
        request(&f.svc, story.id).unwrap();
        let job = f.svc.listening.downloads.start_next().expect("it was queued");
        run_job(f.svc.clone(), f.vault.clone(), route, job).await;
    }

    fn scratch_files(vault: &Vault) -> usize {
        std::fs::read_dir(scratch_dir(vault)).map(|d| d.count()).unwrap_or(0)
    }

    // ── Downloading ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn an_episode_lands_in_its_shows_folder_and_is_linked_to_its_story() {
        let body: Vec<u8> = (0..2_500_000u32).map(|n| (n % 251) as u8).collect();
        let server = Server::start([("/ep.mp3", Reply::ok("audio/mpeg", body.clone()))]).await;
        let f = fixture();
        let show = follow(&f.vault, "The Show: Live", Some(PodcastSettings::default()));
        let ep =
            episode(&f.vault, &show, "Why: a test", "2026-03-09T12:00:00Z", &server.url("/ep.mp3"));

        request(&f.svc, ep.id).unwrap();
        let queued = status(&f.svc);
        assert_eq!((queued.len(), queued[0].state), (1, DownloadState::Waiting));
        assert_eq!(queued[0].title, "Why: a test");
        let job = f.svc.listening.downloads.start_next().unwrap();
        run_job(f.svc.clone(), f.vault.clone(), anywhere(), job).await;

        let linked = f.vault.story(ep.id).unwrap().files.media.expect("the story is linked");
        let file = f.vault.file(linked).unwrap();
        let date = karmayoga_core::model::local_date_in(
            ep.published_at,
            &karmayoga_core::model::system_tz(),
        );
        let dir = "Feed/Podcasts/The Show - Live";
        let name = episode_name(date, "Why: a test", "mp3");
        assert_eq!(file.path(), Some(format!("{dir}/{name}").as_str()));
        assert_eq!(file.origin, FileOrigin::Feed);
        assert!(matches!(file.status, FileStatus::Present));
        assert_eq!(file.size, body.len() as u64);
        assert_eq!(
            std::fs::read(f.folder.path().join(dir).join(&name)).unwrap(),
            body,
            "every byte, in order, across several writes"
        );

        assert!(status(&f.svc).is_empty(), "nothing is left on the list");
        assert_eq!(scratch_files(&f.vault), 0, "and nothing in the scratch place");
        assert_eq!(f.events.count(Kind::File, Op::Created), 1);
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 1);

        // Asked for again, it is not fetched again.
        request(&f.svc, ep.id).unwrap();
        assert!(status(&f.svc).is_empty());
        assert_eq!(server.hits(), 1);
        files::settled().await;
    }

    #[tokio::test]
    async fn a_download_that_goes_wrong_says_why_and_links_nothing() {
        let server = Server::start([
            ("/gone.mp3", Reply::status(404)),
            ("/page.mp3", Reply::ok("text/html", b"<html>Please accept cookies</html>".to_vec())),
            ("/empty.mp3", Reply::ok("audio/mpeg", Vec::new())),
        ])
        .await;
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        let cases = [
            ("/gone.mp3", "the episode is not there any more."),
            ("/page.mp3", "the site sent a web page instead of the episode."),
            ("/empty.mp3", "the site sent nothing."),
        ];
        for (path, said) in cases {
            let ep = episode(&f.vault, &show, path, "2026-03-09T12:00:00Z", &server.url(path));
            fetch(&f, &ep, anywhere()).await;
            let listed = status(&f.svc);
            let this = listed.iter().find(|s| s.story_id == ep.id).expect("it is on the list");
            assert_eq!(this.state, DownloadState::Failed);
            assert_eq!(this.error.as_deref(), Some(said));
            assert_eq!(f.vault.story(ep.id).unwrap().files.media, None);
        }
        assert_eq!(scratch_files(&f.vault), 0);
        assert_eq!(f.events.count(Kind::File, Op::Created), 0);

        // Asking again is the retry: the failure is forgotten and it waits.
        let again = status(&f.svc)[0].story_id;
        request(&f.svc, again).unwrap();
        let after = status(&f.svc);
        assert_eq!(after.iter().filter(|s| s.state == DownloadState::Failed).count(), 2);
        assert!(after.iter().any(|s| s.story_id == again && s.state == DownloadState::Waiting));
    }

    #[tokio::test]
    async fn an_address_on_a_private_network_is_refused_before_anything_is_asked() {
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        for url in ["http://127.0.0.1:9/ep.mp3", "http://192.168.1.20/ep.mp3", "http://nas/ep.mp3"]
        {
            let ep = episode(&f.vault, &show, url, "2026-03-09T12:00:00Z", url);
            fetch(&f, &ep, Route::Public).await;
            let listed = status(&f.svc);
            let this = listed.iter().find(|s| s.story_id == ep.id).unwrap();
            assert_eq!(this.state, DownloadState::Failed);
            let said = this.error.as_deref().unwrap();
            assert!(said.contains("private network"), "{said}");
            assert!(!said.contains("192.168"), "the address is not repeated: {said}");
        }
    }

    #[tokio::test]
    async fn a_cancelled_download_leaves_nothing_behind() {
        let mut stalls = Reply::ok("audio/mpeg", vec![7u8; 1_000_000]);
        stalls.stall_after = Some(10_000);
        let server = Server::start([("/slow.mp3", stalls)]).await;
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        let ep = episode(&f.vault, &show, "Slow", "2026-03-09T12:00:00Z", &server.url("/slow.mp3"));

        request(&f.svc, ep.id).unwrap();
        let job = f.svc.listening.downloads.start_next().unwrap();
        let progress = job.progress.clone();
        let running = tokio::spawn(run_job(f.svc.clone(), f.vault.clone(), anywhere(), job));
        tokio::time::timeout(Duration::from_secs(10), async {
            while progress.received.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("some of it arrived");
        let now = status(&f.svc);
        assert_eq!((now[0].state, now[0].total), (DownloadState::Downloading, Some(1_000_000)));
        assert!(now[0].received > 0);

        cancel(&f.svc, ep.id);
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("it stopped when it was told to")
            .unwrap();
        assert!(status(&f.svc).is_empty(), "a cancelled download is not a failure");
        assert_eq!(f.vault.story(ep.id).unwrap().files.media, None);
        assert_eq!(scratch_files(&f.vault), 0, "the part that came is gone");
    }

    #[tokio::test]
    async fn an_episode_asked_for_while_another_is_coming_down_starts_at_once() {
        let mut stalls = Reply::ok("audio/mpeg", vec![7u8; 1_000_000]);
        stalls.stall_after = Some(10_000);
        let server = Server::start([
            ("/long.mp3", stalls),
            ("/short.mp3", Reply::ok("audio/mpeg", vec![1u8; 5_000])),
        ])
        .await;
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        let long =
            episode(&f.vault, &show, "Long", "2026-03-09T12:00:00Z", &server.url("/long.mp3"));
        let short =
            episode(&f.vault, &show, "Short", "2026-03-10T12:00:00Z", &server.url("/short.mp3"));

        request(&f.svc, long.id).unwrap();
        let (stop, mut stopped) = watch::channel(false);
        let pass_of = tokio::spawn({
            let (svc, vault) = (f.svc.clone(), f.vault.clone());
            async move { pass(&svc, &vault, &anywhere(), &mut stopped).await }
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while status(&f.svc).iter().all(|s| s.received == 0) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the long one started");

        // The long one is still coming down; the short one asked for now has
        // a slot, and does not wait for it.
        request(&f.svc, short.id).unwrap();
        // Linked, and then let go of: the two are a moment apart.
        tokio::time::timeout(Duration::from_secs(10), async {
            while f.vault.story(short.id).unwrap().files.media.is_none()
                || status(&f.svc).iter().any(|s| s.story_id == short.id)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the short one landed while the long one was still coming");
        let listed = status(&f.svc);
        assert_eq!((listed.len(), listed[0].story_id), (1, long.id));
        assert_eq!(listed[0].state, DownloadState::Downloading);

        // Stopped, the task lets go of the long one, which will start again.
        stop.send(true).unwrap();
        let stopped_midway = tokio::time::timeout(Duration::from_secs(5), pass_of)
            .await
            .expect("the pass stopped")
            .unwrap();
        assert!(stopped_midway);
        let after = status(&f.svc);
        assert_eq!((after.len(), after[0].state), (1, DownloadState::Waiting));
        assert_eq!(scratch_files(&f.vault), 0);
        files::settled().await;
    }

    #[tokio::test]
    async fn removing_an_episode_that_is_still_coming_down_waits_for_it_to_stop() {
        let mut stalls = Reply::ok("audio/mpeg", vec![7u8; 1_000_000]);
        stalls.stall_after = Some(10_000);
        let server = Server::start([("/slow.mp3", stalls)]).await;
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        let ep = episode(&f.vault, &show, "Slow", "2026-03-09T12:00:00Z", &server.url("/slow.mp3"));

        request(&f.svc, ep.id).unwrap();
        let job = f.svc.listening.downloads.start_next().unwrap();
        let progress = job.progress.clone();
        let running = tokio::spawn(run_job(f.svc.clone(), f.vault.clone(), anywhere(), job));
        tokio::time::timeout(Duration::from_secs(10), async {
            while progress.received.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("some of it arrived");

        tokio::time::timeout(Duration::from_secs(5), remove_download(&f.svc, ep.id))
            .await
            .expect("removal waited for the download to stop, and then finished")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), running).await.expect("it ended").unwrap();
        assert!(status(&f.svc).is_empty());
        assert_eq!(f.vault.story(ep.id).unwrap().files.media, None);
        assert_eq!(f.events.count(Kind::File, Op::Created), 0);
    }

    #[test]
    fn a_link_to_a_file_that_is_gone_is_dead_and_to_one_that_is_only_missing_not_yet() {
        let f = fixture();
        let options = ImportOptions::new(FileOrigin::Feed);
        let mut file = f.vault.import_bytes(b"sound", "a.mp3", &options).unwrap();
        let now = Timestamp::now();
        let ago = |secs: i64| Timestamp::from_second(now.as_second() - secs).unwrap();

        assert!(!link_is_dead(Some(&file), now), "present");
        assert!(link_is_dead(None, now), "not in the catalog");
        file.status = FileStatus::Trashed { at: ago(5), from: String::new() };
        assert!(link_is_dead(Some(&file), now), "in the trash");
        file.status = FileStatus::Missing { since: ago(60 * 60) };
        assert!(!link_is_dead(Some(&file), now), "missing for an hour: it may come back");
        file.status = FileStatus::Missing { since: ago(MISSING_GRACE_SECS + 1) };
        assert!(link_is_dead(Some(&file), now), "missing for good");
    }

    #[tokio::test]
    async fn only_a_story_with_an_episode_can_be_asked_for() {
        let f = fixture();
        let show = follow(&f.vault, "Show", Some(PodcastSettings::default()));
        let article = story(&f.vault, &show, "An article", "2026-03-09T12:00:00Z");
        let error = request(&f.svc, article.id).unwrap_err();
        assert_eq!(error.code, codes::INVALID);
        assert!(status(&f.svc).is_empty());
        assert_eq!(request(&f.svc, StoryId::new()).unwrap_err().code, codes::NOT_FOUND);
    }

    // ── What a refresh brings ────────────────────────────────────────────

    #[tokio::test]
    async fn a_refresh_has_the_download_policy_applied_to_what_it_brought() {
        let f = fixture();
        let newest = PodcastSettings {
            download: Some(DownloadPolicy::Newest { count: 1 }),
            ..Default::default()
        };
        let show = follow(&f.vault, "Newest", Some(newest));
        let url = "https://cdn.example/ep.mp3";
        let old = episode(&f.vault, &show, "old", "2026-01-01T00:00:00Z", url);
        let new = episode(&f.vault, &show, "new", "2026-01-03T00:00:00Z", url);
        let note = story(&f.vault, &show, "a note", "2026-01-04T00:00:00Z");
        apply_policies(&f.svc, show.id, vec![old.id, new.id, note.id]).await.unwrap();
        let waiting: Vec<_> = status(&f.svc).iter().map(|s| s.story_id).collect();
        assert_eq!(waiting, [new.id]);

        // Off, and a feed that is not a podcast, queue nothing at all.
        let off = PodcastSettings { download: Some(DownloadPolicy::Off), ..Default::default() };
        let quiet = show_with(&f, "Off", Some(off), 2);
        apply_policies(&f.svc, quiet.0.id, quiet.1).await.unwrap();
        let plain = show_with(&f, "Plain feed", None, 2);
        apply_policies(&f.svc, plain.0.id, plain.1).await.unwrap();
        assert_eq!(status(&f.svc).len(), 1);
    }

    /// A show of `count` episodes, all stored at once.
    fn show_with(
        f: &Fixture,
        title: &str,
        settings: Option<PodcastSettings>,
        count: usize,
    ) -> (NewsSource, Vec<StoryId>) {
        let show = follow(&f.vault, title, settings);
        let ids = (0..count)
            .map(|n| {
                let published = format!("2026-02-{:02}T00:00:00Z", n + 1);
                episode(
                    &f.vault,
                    &show,
                    &format!("{title} {n}"),
                    &published,
                    "https://cdn.example/e.mp3",
                )
                .id
            })
            .collect();
        (show, ids)
    }

    #[tokio::test]
    async fn every_new_episode_is_capped_on_a_shows_first_refresh_and_not_after() {
        let f = fixture();
        let all = PodcastSettings { download: Some(DownloadPolicy::All), ..Default::default() };
        let (show, backlog) = show_with(&f, "Everything", Some(all), 5);
        apply_policies(&f.svc, show.id, backlog.clone()).await.unwrap();
        let waiting: Vec<_> = status(&f.svc).iter().map(|s| s.story_id).collect();
        assert_eq!(waiting, [backlog[4], backlog[3], backlog[2]], "the newest three, newest first");

        // A later refresh brings two more: both, as it is not a back catalogue.
        let fresh: Vec<StoryId> = (10..12)
            .map(|n| {
                let published = format!("2026-03-{n}T00:00:00Z");
                episode(
                    &f.vault,
                    &show,
                    &format!("later {n}"),
                    &published,
                    "https://cdn.example/e.mp3",
                )
                .id
            })
            .collect();
        apply_policies(&f.svc, show.id, fresh.clone()).await.unwrap();
        assert_eq!(status(&f.svc).len(), 5);
    }

    // ── Clearing out ─────────────────────────────────────────────────────

    /// An episode of `source` that has been downloaded: a Feed file in the
    /// show's folder, linked.
    fn downloaded(
        f: &Fixture,
        source: &NewsSource,
        title: &str,
        published: &str,
    ) -> (Story, FileId) {
        downloaded_as(f, source, title, published, FileOrigin::Feed)
    }

    fn downloaded_as(
        f: &Fixture,
        source: &NewsSource,
        title: &str,
        published: &str,
        origin: FileOrigin,
    ) -> (Story, FileId) {
        let story = story(&f.vault, source, title, published);
        let options =
            ImportOptions { dir: Some(show_dir(&source.title)), ..ImportOptions::new(origin) };
        let file = f.vault.import_bytes(b"sound", &format!("{title}.mp3"), &options).unwrap();
        f.vault.update_story_files(story.id, |files| files.media = Some(file.id)).unwrap();
        (story, file.id)
    }

    fn finished(vault: &Vault, story: &Story, days_ago: i64) {
        let mut played =
            Consumption::new(ContentRef::Story(story.id), &story.title, ConsumeMode::Listen);
        played.finished = true;
        played.progress = 1.0;
        let now = Timestamp::now().as_second();
        played.finished_at = Some(Timestamp::from_second(now - days_ago * 24 * 60 * 60).unwrap());
        vault.save_consumption(&played).unwrap();
    }

    fn linked(vault: &Vault, story: &Story) -> Option<FileId> {
        vault.story(story.id).unwrap().files.media
    }

    #[tokio::test]
    async fn clearing_out_moves_a_played_episode_to_the_trash_and_nothing_that_is_not_the_feeds_to_take()
     {
        let f = fixture();
        let after_three = PodcastSettings {
            keep: Some(KeepPolicy::AfterPlayed { days: 3 }),
            ..Default::default()
        };
        let daily = follow(&f.vault, "Daily", Some(after_three));
        let when = "2026-01-01T00:00:00Z";

        let (played, played_file) = downloaded(&f, &daily, "played long ago", when);
        let transcript = f
            .vault
            .import_bytes(
                b"[00:00:00] words",
                "played long ago transcript.md",
                &ImportOptions::new(FileOrigin::Feed),
            )
            .unwrap();
        f.vault
            .update_story_files(played.id, |files| files.transcript = Some(transcript.id))
            .unwrap();
        finished(&f.vault, &played, 10);

        let (recent, _) = downloaded(&f, &daily, "played yesterday", when);
        finished(&f.vault, &recent, 1);
        let (unplayed, _) = downloaded(&f, &daily, "never played", when);
        let _ = &unplayed;

        let (favourite, favourite_file) = downloaded(&f, &daily, "a favourite", when);
        finished(&f.vault, &favourite, 10);
        let star = FileUpdate { favourite: Some(true), ..Default::default() };
        f.vault.update_files(&[favourite_file], &star).unwrap();

        let (saved, _) = downloaded(&f, &daily, "a saved story", when);
        finished(&f.vault, &saved, 10);
        f.vault.set_stories_saved(&[saved.id], true).unwrap();

        let (queued, _) = downloaded(&f, &daily, "in Up Next", when);
        finished(&f.vault, &queued, 10);
        let entry =
            karmayoga_core::news::QueueEntry::new(ContentRef::Story(queued.id), "in Up Next", 0);
        f.vault.add_to_queue(entry, QueuePlace::End).unwrap();

        // Put in the show's folder by somebody, and linked to the story: not
        // a file the Feed downloaded, so not one it takes away.
        let (found, found_file) =
            downloaded_as(&f, &daily, "not the Feed's", when, FileOrigin::Files);
        finished(&f.vault, &found, 10);

        // Trashed in Files by hand, and emptied from the trash too: nothing
        // there for a story to play.
        let (gone, gone_file) = downloaded(&f, &daily, "already in the trash", when);
        finished(&f.vault, &gone, 10);
        f.vault.trash_files(&[gone_file]).unwrap();
        let (deleted, deleted_file) = downloaded(&f, &daily, "deleted for good", when);
        f.vault.trash_files(&[deleted_file]).unwrap();
        f.vault.delete_files_forever(&[deleted_file]).unwrap();

        // A show that keeps everything, and a feed that is not a podcast.
        let forever = follow(
            &f.vault,
            "Forever",
            Some(PodcastSettings { keep: Some(KeepPolicy::Forever), ..Default::default() }),
        );
        let (kept, _) = downloaded(&f, &forever, "kept for ever", when);
        finished(&f.vault, &kept, 100);
        let plain = follow(&f.vault, "Plain", None);
        let (article, _) = downloaded(&f, &plain, "an article", when);
        finished(&f.vault, &article, 100);
        let (lost, lost_file) = downloaded(&f, &plain, "lost from a feed with no policy", when);
        f.vault.trash_files(&[lost_file]).unwrap();

        assert_eq!(sweep(&f.svc).await.unwrap(), 1, "only the one played long ago");

        assert_eq!(linked(&f.vault, &played), None, "unlinked");
        assert!(f.vault.file(played_file).unwrap().status.is_trashed(), "and in the trash");
        let still = f.vault.story(played.id).unwrap().files;
        assert_eq!(still.transcript, Some(transcript.id), "its transcript is the Feed's memory");
        assert!(matches!(f.vault.file(transcript.id).unwrap().status, FileStatus::Present));
        for story in [&recent, &unplayed, &favourite, &saved, &queued, &found, &kept, &article] {
            assert!(linked(&f.vault, story).is_some(), "{} stays", story.title);
        }
        assert!(matches!(f.vault.file(favourite_file).unwrap().status, FileStatus::Present));
        assert!(matches!(f.vault.file(found_file).unwrap().status, FileStatus::Present));
        for story in [&gone, &deleted, &lost] {
            assert_eq!(
                linked(&f.vault, story),
                None,
                "{}: nothing to play, so no link",
                story.title
            );
        }
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 1, "one announcement for all four");
        assert_eq!(f.events.count(Kind::File, Op::Updated), 1);

        // Nothing more to do the second time.
        assert_eq!(sweep(&f.svc).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn clearing_out_keeps_the_newest_of_a_show_but_not_what_was_only_just_downloaded() {
        let f = fixture();
        let newest_one =
            PodcastSettings { keep: Some(KeepPolicy::Newest { count: 1 }), ..Default::default() };
        let tidy = follow(&f.vault, "Tidy", Some(newest_one));
        let (older, older_file) = downloaded(&f, &tidy, "older", "2026-01-01T00:00:00Z");
        let (newer, _) = downloaded(&f, &tidy, "newer", "2026-01-02T00:00:00Z");

        assert_eq!(sweep(&f.svc).await.unwrap(), 0, "just downloaded: not yet");
        assert!(linked(&f.vault, &older).is_some());

        let later = Timestamp::now().as_second() + 3 * 24 * 60 * 60;
        let swept = clear_out(&f.vault, Timestamp::from_second(later).unwrap()).unwrap();
        assert_eq!(swept.stories, [older.id]);
        assert_eq!(swept.files, [older_file]);
        assert!(linked(&f.vault, &newer).is_some());
        assert_eq!(linked(&f.vault, &older), None);
    }

    #[tokio::test]
    async fn what_points_at_a_file_that_was_trashed_is_taken_off_and_a_kept_article_is_not() {
        let f = fixture();
        let show = follow(&f.vault, "Daily", Some(PodcastSettings::default()));
        let (ep, media) = downloaded(&f, &show, "An episode", "2026-03-09T12:00:00Z");
        let make = |name: &str| {
            f.vault.import_bytes(b"x", name, &ImportOptions::new(FileOrigin::Feed)).unwrap().id
        };
        let (transcript, text, audio) = (make("t.md"), make("a.md"), make("a.opus"));
        f.vault
            .update_story_files(ep.id, |files| {
                files.transcript = Some(transcript);
                files.text = Some(text);
                files.audio = Some(audio);
            })
            .unwrap();
        // An episode's recording names its transcript, and so does one that is
        // not an episode's at all.
        let names_it = FileUpdate {
            properties: [(
                TRANSCRIPT_PROPERTY.to_string(),
                Some(PropValue::Text(transcript.to_string())),
            )]
            .into(),
            ..FileUpdate::default()
        };
        let memo = f
            .vault
            .import_bytes(b"sound", "memo.mp3", &ImportOptions::new(FileOrigin::Files))
            .unwrap();
        f.vault.update_files(&[media, memo.id], &names_it).unwrap();

        // While every file is there, nothing is touched.
        sweep(&f.svc).await.unwrap();
        let files = f.vault.story(ep.id).unwrap().files;
        assert_eq!(
            (files.media, files.transcript, files.text, files.audio),
            (Some(media), Some(transcript), Some(text), Some(audio))
        );
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 0);

        // Trashed in Files: the story stops offering what is not there, and the
        // recordings stop naming a transcript that is not. The article somebody
        // trashed stays unlinked from nothing: it is how they said they do not
        // want it kept again.
        f.vault.trash_files(&[transcript, text, audio]).unwrap();
        sweep(&f.svc).await.unwrap();
        let files = f.vault.story(ep.id).unwrap().files;
        assert_eq!(
            (files.media, files.transcript, files.text, files.audio),
            (Some(media), None, Some(text), None)
        );
        for recording in [media, memo.id] {
            let properties = f.vault.file(recording).unwrap().properties;
            assert!(!properties.contains_key(TRANSCRIPT_PROPERTY), "{recording}");
        }
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 1);
        assert_eq!(f.events.count(Kind::File, Op::Updated), 1, "both recordings, announced once");

        // And the second time there is nothing left to do.
        sweep(&f.svc).await.unwrap();
        assert_eq!(f.events.count(Kind::Story, Op::Updated), 1);
    }

    #[test]
    fn which_links_are_lost_depends_on_the_slot() {
        let f = fixture();
        let options = ImportOptions::new(FileOrigin::Feed);
        let mut trashed = f.vault.import_bytes(b"x", "a.md", &options).unwrap();
        let present = f.vault.import_bytes(b"x", "b.md", &options).unwrap();
        let now = Timestamp::now();
        trashed.status = FileStatus::Trashed { at: now, from: String::new() };
        let gone = FileId::new();
        let catalog: HashMap<FileId, FileEntry> =
            [(trashed.id, trashed.clone()), (present.id, present.clone())].into();
        let files = StoryFiles {
            media: Some(trashed.id),
            transcript: Some(gone),
            text: Some(trashed.id),
            audio: Some(present.id),
            summary: Some(trashed.id),
        };
        let lost = lost_links(&files, &catalog, now);
        assert_eq!(lost.media, Some(trashed.id));
        assert_eq!(lost.transcript, Some(gone));
        assert_eq!(lost.text, None, "in the trash is not gone, for an article somebody binned");
        assert_eq!(lost.audio, None);
        assert_eq!(lost.summary, None, "nor for a summary");
        assert!(lost_links(&StoryFiles::default(), &catalog, now).is_empty());

        // Unlinking takes only what it was found to name.
        let mut changed = StoryFiles { media: Some(present.id), ..files.clone() };
        lost.unlink(&mut changed);
        assert_eq!(changed.media, Some(present.id), "another episode was linked since");
        assert_eq!(changed.transcript, None);
        let text_gone =
            lost_links(&StoryFiles { text: Some(gone), ..Default::default() }, &catalog, now);
        assert_eq!(text_gone.text, Some(gone));
        // A summary that is gone from the catalog is unlinked, and only if it
        // is still the one that was found gone.
        let summary_gone =
            lost_links(&StoryFiles { summary: Some(gone), ..Default::default() }, &catalog, now);
        assert_eq!(summary_gone.summary, Some(gone));
        let mut linked = StoryFiles { summary: Some(gone), ..Default::default() };
        summary_gone.unlink(&mut linked);
        assert_eq!(linked.summary, None);
        let mut rewritten = StoryFiles { summary: Some(present.id), ..Default::default() };
        summary_gone.unlink(&mut rewritten);
        assert_eq!(rewritten.summary, Some(present.id), "another summary was linked since");
    }
}
