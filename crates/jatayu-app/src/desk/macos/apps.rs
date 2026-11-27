//! The applications installed here, and starting one.
//!
//! The list is what Launchpad and Spotlight show, read the way they do: the
//! `.app` bundles in `/Applications` (and the folders inside it, such as
//! Utilities), `/System/Applications` for Apple's own, and `~/Applications`
//! for the person's. Each bundle's `Info.plist` names it
//! (`CFBundleDisplayName`, else the bundle's file name -- what Finder shows --
//! else `CFBundleName`) and gives its bundle identifier, which is the id: it
//! is what `open -b` starts it by, and unlike a path it does not change
//! when an application is moved or updated, or when a copy is dragged to
//! another folder.
//!
//! An `Info.plist` is a binary property list, and `plutil -convert json` is
//! the system's own reader for it -- the tool is there on every Mac and
//! needs no library here -- one run per bundle, a few at a time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use jatayu_service::desk::InstalledApp;
use serde_json::Value;

/// Where applications are kept, besides the person's own. Their plain
/// subfolders -- Utilities among them -- are looked in too, by `bundles_in`.
const FOLDERS: [&str; 2] = ["/Applications", "/System/Applications"];

/// Applications that live elsewhere and are asked for by name all the time.
const ALSO: [&str; 1] = ["/System/Library/CoreServices/Finder.app"];

/// How many `plutil` runs at once.
const WORKERS: usize = 8;

/// Every application a launcher would list. Blocking.
pub(in crate::desk) fn installed() -> Vec<InstalledApp> {
    let mut folders: Vec<PathBuf> = FOLDERS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        folders.push(Path::new(&home).join("Applications"));
    }
    let mut bundles = bundles_in(&folders);
    bundles.extend(ALSO.iter().map(PathBuf::from).filter(|path| path.exists()));

    let next = AtomicUsize::new(0);
    let found: Vec<Option<InstalledApp>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(bundle) = bundles.get(at) else { break };
                        mine.push((at, read_bundle(bundle)));
                    }
                    mine
                })
            })
            .collect();
        let mut all: Vec<(usize, Option<InstalledApp>)> =
            workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect();
        all.sort_by_key(|(at, _)| *at);
        all.into_iter().map(|(_, app)| app).collect()
    });
    tidy(found.into_iter().flatten().collect())
}

/// Every `.app` in `folders`, and in the plain folders directly inside them
/// (`/Applications/Utilities`, a vendor's folder of its programs). Not inside
/// a bundle: what is in there is its own.
fn bundles_in(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_bundle(&path) {
                bundles.push(path);
            } else if path.is_dir()
                && let Ok(inside) = std::fs::read_dir(&path)
            {
                bundles.extend(inside.flatten().map(|e| e.path()).filter(|p| is_bundle(p)));
            }
        }
    }
    bundles
}

fn is_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && path.is_dir()
}

/// The application at `bundle`, from its `Info.plist`.
fn read_bundle(bundle: &Path) -> Option<InstalledApp> {
    let plist = bundle.join("Contents/Info.plist");
    let ran =
        super::run("/usr/bin/plutil", &["-convert", "json", "-o", "-", &plist.to_string_lossy()])
            .inspect_err(
                |e| tracing::debug!(error = %e, ?bundle, "an application could not be read"),
            )
            .ok()?;
    if !ran.success {
        tracing::debug!(?bundle, stderr = %ran.stderr.trim(), "an application's plist is unreadable");
        return None;
    }
    let info: Value = serde_json::from_str(&ran.stdout).ok()?;
    let stem = bundle.file_stem()?.to_string_lossy();
    app_from_info(&info, &stem)
}

/// What an `Info.plist` (as JSON) says about an application, and `stem`, its
/// bundle's file name without `.app`. `None` for one that has no bundle
/// identifier to start it by, and for the background helpers a person never
/// opens (`LSBackgroundOnly`).
fn app_from_info(info: &Value, stem: &str) -> Option<InstalledApp> {
    let text =
        |key: &str| info.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    if info.get("LSBackgroundOnly").is_some_and(truthy) {
        return None;
    }
    let id = text("CFBundleIdentifier")?;
    if !is_bundle_id(id) {
        return None;
    }
    let name = text("CFBundleDisplayName")
        .or(Some(stem).filter(|s| !s.is_empty()))
        .or_else(|| text("CFBundleName"))?;
    let keywords = [text("CFBundleName"), Some(stem)]
        .into_iter()
        .flatten()
        .filter(|word| !word.eq_ignore_ascii_case(name))
        .fold(Vec::<String>::new(), |mut kept, word| {
            if !kept.iter().any(|k| k.eq_ignore_ascii_case(word)) {
                kept.push(word.to_string());
            }
            kept
        });
    Some(InstalledApp { id: id.to_string(), name: name.to_string(), keywords })
}

/// A plist boolean, which is `true`, or sometimes the string `"YES"` or a 1.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "yes" | "true" | "1"),
        _ => false,
    }
}

/// Is `id` the shape of a bundle identifier: reverse-DNS letters, digits, dots,
/// hyphens and underscores, and not something `open` would take for an
/// option? It is what `launch` is given, and what the model wrote.
fn is_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with(['-', '.'])
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// One of each bundle identifier -- an application kept in two folders is
/// listed once, the first found -- in the same order every time.
fn tidy(mut apps: Vec<InstalledApp>) -> Vec<InstalledApp> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.id.to_lowercase()));
    apps.sort_by(|a, b| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id))
    });
    apps
}

/// Start the application `id`, a bundle identifier. Blocking.
///
/// `open -b` brings it to the front, or starts it there, as Launchpad does.
pub(in crate::desk) fn launch(id: &str) -> Result<(), String> {
    let id = id.trim();
    if !is_bundle_id(id) {
        return Err("That application isn't installed.".to_string());
    }
    let ran = super::run("/usr/bin/open", &["-b", id]).map_err(|e| {
        tracing::warn!(error = %e, id, "an application could not be started");
        "That application didn't start.".to_string()
    })?;
    if ran.success {
        return Ok(());
    }
    tracing::warn!(id, stderr = %ran.stderr.trim(), "an application would not start");
    // "Unable to find application with bundle identifier ..." and `open`'s
    // `kLSApplicationNotFoundErr`.
    if ran.stderr.contains("Unable to find application") || ran.stderr.contains("-10814") {
        return Err("That application isn't installed.".to_string());
    }
    Err("That application wouldn't start.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_application_is_named_by_its_display_name_and_answers_to_the_others() {
        let info = json!({
            "CFBundleIdentifier": "com.apple.iCal",
            "CFBundleName": "Calendar",
            "CFBundleDisplayName": "Calendar App",
        });
        let app = app_from_info(&info, "Calendar").unwrap();
        assert_eq!(app.id, "com.apple.iCal");
        assert_eq!(app.name, "Calendar App");
        assert_eq!(app.keywords, ["Calendar"]);
    }

    #[test]
    fn without_a_display_name_the_bundle_s_file_name_is_what_finder_shows() {
        let info = json!({"CFBundleIdentifier": "com.microsoft.VSCode", "CFBundleName": "Code"});
        let app = app_from_info(&info, "Visual Studio Code").unwrap();
        assert_eq!(app.name, "Visual Studio Code");
        assert_eq!(app.keywords, ["Code"]);
        let plain =
            json!({"CFBundleIdentifier": "com.apple.calculator", "CFBundleName": "Calculator"});
        assert!(app_from_info(&plain, "Calculator").unwrap().keywords.is_empty());
    }

    #[test]
    fn what_cannot_be_started_or_is_never_opened_is_left_out() {
        assert!(app_from_info(&json!({"CFBundleName": "Nothing"}), "Nothing").is_none());
        assert!(app_from_info(&json!({"CFBundleIdentifier": "  "}), "Blank").is_none());
        assert!(app_from_info(&json!({"CFBundleIdentifier": "-a evil"}), "Evil").is_none());
        let helper = json!({"CFBundleIdentifier": "com.example.helper", "LSBackgroundOnly": true});
        assert!(app_from_info(&helper, "Helper").is_none());
        let yes = json!({"CFBundleIdentifier": "com.example.helper", "LSBackgroundOnly": "YES"});
        assert!(app_from_info(&yes, "Helper").is_none());
        let off = json!({"CFBundleIdentifier": "com.example.app", "LSBackgroundOnly": false});
        assert!(app_from_info(&off, "App").is_some());
    }

    #[test]
    fn bundle_identifiers_are_reverse_dns_and_never_an_option() {
        for good in ["com.apple.Safari", "org.mozilla.firefox", "com.foo-bar.App_2"] {
            assert!(is_bundle_id(good), "{good}");
        }
        for bad in ["", "-b", "--help", ".hidden", "com apple", "a;b", "a\nb", "../x"] {
            assert!(!is_bundle_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn one_of_each_in_a_steady_order() {
        let app = |id: &str, name: &str| InstalledApp {
            id: id.to_string(),
            name: name.to_string(),
            keywords: Vec::new(),
        };
        let listed = tidy(vec![
            app("com.b", "banana"),
            app("com.a", "Apple"),
            app("com.COPY", "Copy"),
            app("com.copy", "Copy again"),
        ]);
        let names: Vec<_> = listed.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Apple", "banana", "Copy"]);
    }

    #[test]
    fn the_system_applications_are_found_here() {
        // Calculator is Apple's and is on every Mac this is built or run on.
        let bundles = bundles_in(&[PathBuf::from("/System/Applications")]);
        assert!(bundles.iter().any(|b| b.ends_with("Calculator.app")));
        let app = read_bundle(Path::new("/System/Applications/Calculator.app")).unwrap();
        assert_eq!(app.id, "com.apple.calculator");
        assert_eq!(app.name, "Calculator");
    }
}
