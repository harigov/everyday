//! Finding the program a stdio connection names, when a desktop session's
//! `PATH` does not have it. See `docs/plans/connections.md`'s "stdio".
//!
//! A program started from a launcher inherits the session's `PATH`, not the
//! one a shell profile builds -- which is where `nvm`, `bun` and `cargo`
//! put their programs. So the places they put them are looked in too.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where `command` is: itself when it has a path, else the first match on
/// `path` (the `PATH` value, split as the platform does), else the first
/// match in the places a desktop session's `PATH` often lacks (`home`'s
/// `.nvm/versions/node/*/bin` newest version first, `.local/bin`,
/// `.cargo/bin`, `.bun/bin`, `.deno/bin`, then `/usr/local/bin` and
/// `/opt/homebrew/bin`). `None` when it is nowhere. Only executable files
/// count.
pub fn resolve(command: &str, path: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }
    let given = Path::new(command);
    if given.is_absolute() || given.components().count() > 1 {
        return is_executable(given).then(|| given.to_path_buf());
    }

    let on_path = path.map(std::env::split_paths).into_iter().flatten();
    let in_home = home.map(extra_dirs_under).into_iter().flatten();
    let elsewhere = ["/usr/local/bin", "/opt/homebrew/bin"].into_iter().map(PathBuf::from);
    on_path
        .chain(in_home)
        .chain(elsewhere)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(command))
        .find(|candidate| is_executable(candidate))
}

/// The `PATH` a child gets: `dir` (the found program's directory) first,
/// then `inherited`, without repeating `dir`.
pub fn child_path(dir: &Path, inherited: Option<&str>) -> String {
    let rest = inherited
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty() && entry != dir);
    let joined = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest));
    match joined {
        Ok(joined) => joined.to_string_lossy().into_owned(),
        // An entry that cannot be joined (it holds the separator): the
        // directory alone is still what the program needs.
        Err(_) => dir.display().to_string(),
    }
}

/// The directories under a home that tools put programs in, in the order
/// they are looked in.
fn extra_dirs_under(home: &Path) -> Vec<PathBuf> {
    let mut dirs = nvm_bins(home);
    for sub in [".local/bin", ".cargo/bin", ".bun/bin", ".deno/bin"] {
        dirs.push(home.join(sub));
    }
    dirs
}

/// `~/.nvm/versions/node/*/bin`, the newest version first.
fn nvm_bins(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".nvm").join("versions").join("node");
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut versions: Vec<(Vec<u64>, OsString)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let name = entry.file_name();
            (version_key(&name.to_string_lossy()), name)
        })
        .collect();
    versions.sort();
    versions.reverse();
    versions.into_iter().map(|(_, name)| root.join(name).join("bin")).collect()
}

/// `v24.13.0` as `[24, 13, 0]`, so that 24.13 sorts after 24.2. A segment
/// that is not a number counts as 0.
fn version_key(name: &str) -> Vec<u64> {
    name.trim_start_matches('v').split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "jatayu-connect-{label}-{}-{}",
                std::process::id(),
                unique()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn program(&self, relative: &str, executable: bool) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    #[test]
    fn finds_a_program_on_the_path_in_order() {
        let scratch = Scratch::new("path");
        let first = scratch.program("a/kyc-tool-one", true);
        scratch.program("b/kyc-tool-one", true);
        let path = std::env::join_paths([scratch.0.join("a"), scratch.0.join("b")]).unwrap();
        assert_eq!(resolve("kyc-tool-one", path.to_str(), None), Some(first));
    }

    #[test]
    fn a_file_that_is_not_executable_does_not_count() {
        let scratch = Scratch::new("mode");
        scratch.program("a/kyc-tool-two", false);
        let good = scratch.program("b/kyc-tool-two", true);
        let path = std::env::join_paths([scratch.0.join("a"), scratch.0.join("b")]).unwrap();
        assert_eq!(resolve("kyc-tool-two", path.to_str(), None), Some(good));
        assert_eq!(resolve("kyc-tool-two", scratch.0.join("a").to_str(), None), None);
    }

    #[test]
    fn a_command_with_a_path_is_itself_or_nothing() {
        let scratch = Scratch::new("given");
        let program = scratch.program("bin/kyc-tool-three", true);
        let given = program.to_str().unwrap();
        assert_eq!(resolve(given, None, None), Some(program.clone()));
        assert_eq!(resolve(&format!("{given}-missing"), None, None), None);
        assert_eq!(resolve("", None, None), None);
    }

    #[test]
    fn the_newest_node_comes_first() {
        let scratch = Scratch::new("nvm");
        scratch.program(".nvm/versions/node/v9.11.2/bin/kyc-tool-four", true);
        scratch.program(".nvm/versions/node/v24.2.0/bin/kyc-tool-four", true);
        let newest = scratch.program(".nvm/versions/node/v24.13.0/bin/kyc-tool-four", true);
        scratch.program(".local/bin/kyc-tool-four", true);
        assert_eq!(resolve("kyc-tool-four", Some(""), Some(&scratch.0)), Some(newest));
    }

    #[test]
    fn falls_back_to_the_places_a_session_path_lacks() {
        let scratch = Scratch::new("home");
        let local = scratch.program(".local/bin/kyc-tool-five", true);
        assert_eq!(resolve("kyc-tool-five", Some("/nonexistent"), Some(&scratch.0)), Some(local));
        assert_eq!(resolve("kyc-tool-five", Some("/nonexistent"), None), None);
        let cargo = scratch.program(".cargo/bin/kyc-tool-six", true);
        assert_eq!(resolve("kyc-tool-six", None, Some(&scratch.0)), Some(cargo));
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_key("v24.13.0") > version_key("v24.2.0"));
        assert!(version_key("v10.0.0") > version_key("v9.11.2"));
    }

    #[test]
    fn the_found_directory_goes_first_without_repeating() {
        let joined = child_path(Path::new("/opt/node/bin"), Some("/usr/bin:/opt/node/bin:/bin"));
        assert_eq!(joined, "/opt/node/bin:/usr/bin:/bin");
        assert_eq!(child_path(Path::new("/opt/node/bin"), None), "/opt/node/bin");
        assert_eq!(child_path(Path::new("/opt/node/bin"), Some("")), "/opt/node/bin");
    }
}
