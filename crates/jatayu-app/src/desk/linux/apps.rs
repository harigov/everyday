//! The applications installed here, and starting one.
//!
//! The list is GIO's: every `.desktop` file the desktop's launcher would show,
//! from the system, from the user's own `~/.local/share/applications`,
//! and from Flatpak and Snap's exports -- the same list the Activities
//! overview searches, because it is read the same way. Each is named by its
//! desktop file's id (`org.gnome.Nautilus.desktop`), and answers to what its
//! file calls it, its generic name ("Web Browser" for Firefox, "Files" for
//! Nautilus) and its keywords.
//!
//! # Starting one so that it comes to the front
//!
//! A window opened by a program that was not itself asked for it is not
//! given the keyboard by a desktop that has any manners -- under Wayland
//! especially, where an application cannot raise itself at all and the
//! compositor decides from a token the *launcher* has to obtain. GDK makes
//! the token (it is what `gtk_show_uri` and every GTK launcher does), but only
//! on the main thread, where the display is, and only through a *launch
//! context* made from it. So the application is started from the main
//! thread, with GDK's context -- asynchronously, because a D-Bus activated
//! application is a round trip to a program that has to start before it
//! answers, and the main thread is the window's -- and falls back to a plain
//! GIO context where there is no display to ask.
//!
//! The context is also where the environment is cleaned: the child starts
//! with what a program started from the shell would have, not with ours (see
//! [`crate::desk::environment`]).

use gtk::gio;
use gtk::prelude::*;
use jatayu_service::desk::InstalledApp;
use tauri::AppHandle;

use crate::desk::environment::{self, Change};

/// How long to wait for an application to say that it has started. An
/// application GIO has handed to the desktop's own launch service answers
/// when the service has it, and the service may be starting the program for
/// the first time; but one that has not answered in this long is not going to.
const STARTING: std::time::Duration = std::time::Duration::from_secs(15);

/// Every application a launcher would list. Blocking.
pub(in crate::desk) fn installed() -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = gio::AppInfo::all()
        .into_iter()
        .filter(|info| info.should_show())
        .filter_map(|info| {
            let id = info.id()?.to_string();
            let name = info.display_name().trim().to_string();
            if name.is_empty() {
                return None;
            }
            let mut keywords = Vec::new();
            if let Some(desktop) = info.downcast_ref::<gio::DesktopAppInfo>() {
                keywords.extend(desktop.keywords().iter().map(|word| word.to_string()));
                keywords.extend(desktop.generic_name().map(|generic| generic.to_string()));
            }
            Some(InstalledApp { id, name: name.clone(), keywords: tidy_keywords(keywords, &name) })
        })
        .collect();
    // The same order every time -- GIO's is the order of a directory scan.
    apps.sort_by(|a, b| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id))
    });
    apps
}

/// `keywords` trimmed, without the empty ones, the repeats, or the name
/// itself: a desktop file's generic name is often the name.
fn tidy_keywords(keywords: Vec<String>, name: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for word in keywords {
        let word = word.trim().to_string();
        let seen = |other: &str| other.eq_ignore_ascii_case(&word);
        if word.is_empty() || seen(name) || kept.iter().any(|k| seen(k)) {
            continue;
        }
        kept.push(word);
    }
    kept
}

/// Start the application `id`, from the main thread.
pub(in crate::desk) async fn launch(app: &AppHandle, id: String) -> Result<(), String> {
    let (done, answer) = tokio::sync::oneshot::channel();
    crate::writing::on_main(app, move || start(&id, done)).await?;
    match tokio::time::timeout(STARTING, answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("That application didn't start.".to_string()),
        Err(_) => Err("That application is taking too long to start.".to_string()),
    }
}

/// [`launch`], on the main thread: begin it, and let `done` hear how it went.
fn start(id: &str, done: tokio::sync::oneshot::Sender<Result<(), String>>) {
    // The id as `apps` gave it ends in `.desktop`; one that was typed by
    // somebody -- or a model -- who left it off is still the same application.
    let info = gio::DesktopAppInfo::new(id).or_else(|| {
        let bare = !id.ends_with(".desktop");
        bare.then(|| gio::DesktopAppInfo::new(&format!("{id}.desktop"))).flatten()
    });
    let Some(info) = info else {
        let _ = done.send(Err("That application isn't installed.".to_string()));
        return;
    };
    let name = info.display_name().to_string();
    let context = launch_context();
    info.launch_uris_async(&[], Some(&context), None::<&gio::Cancellable>, move |result| {
        let answer = result.map_err(|e| {
            tracing::warn!(error = %e, application = %name, "an application would not start");
            format!("{name} wouldn't start.")
        });
        let _ = done.send(answer);
    });
}

/// What starts an application: GDK's, which can ask the display for a token to
/// give the window focus, or GIO's own where there is no display to ask --
/// with the environment cleaned either way.
fn launch_context() -> gio::AppLaunchContext {
    let context: gio::AppLaunchContext = gtk::gdk::Display::default()
        .and_then(|display| display.app_launch_context())
        .map(|context| context.upcast::<gio::AppLaunchContext>())
        .unwrap_or_default();
    for change in environment::changes(environment::current()) {
        match change {
            Change::Unset(name) => context.unsetenv(name),
            Change::Set(name, value) => context.setenv(name, value),
        }
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_trimmed_and_never_repeat_what_is_already_known() {
        let words = ["Web Browser", " web browser", "", "Internet", "FIREFOX", "internet"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(tidy_keywords(words, "Firefox"), ["Web Browser", "Internet"]);
        assert_eq!(tidy_keywords(Vec::new(), "Files"), Vec::<String>::new());
    }
}
