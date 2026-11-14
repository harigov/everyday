//! The built interface, wherever this process keeps it.
//!
//! The desktop app already carries a copy -- Tauri embeds `ui/dist` in the
//! binary -- so it hands its own asset resolver in, wrapped as an
//! [`Assets`]. `karmayoga serve` has no such copy and reads a folder instead
//! ([`Assets::folder`]). The routes do not care which.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// One file of the interface.
pub struct Asset {
    pub bytes: Cow<'static, [u8]>,
    pub mime: String,
}

type Lookup = dyn Fn(&str) -> Option<Asset> + Send + Sync;

/// Where the interface's files come from.
#[derive(Clone)]
pub struct Assets(Arc<Lookup>);

impl Assets {
    /// Files answered by `lookup`, which is given a path relative to the
    /// root with no leading slash (`index.html`, `assets/index-abc.js`).
    pub fn new(lookup: impl Fn(&str) -> Option<Asset> + Send + Sync + 'static) -> Self {
        Self(Arc::new(lookup))
    }

    /// Files under `root`, read when asked for -- `ui/dist`, typically.
    pub fn folder(root: PathBuf) -> Self {
        Self::new(move |path| read_under(&root, path))
    }

    pub fn get(&self, path: &str) -> Option<Asset> {
        (self.0)(path)
    }
}

/// Read `path` under `root`, refusing anything that would climb out of it.
fn read_under(root: &Path, path: &str) -> Option<Asset> {
    let relative = Path::new(path);
    if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let full = root.join(relative);
    // Symlinks inside a build folder are not expected; one that points out of
    // it is refused rather than followed.
    let canonical = full.canonicalize().ok()?;
    let base = root.canonicalize().ok()?;
    if !canonical.starts_with(&base) || !canonical.is_file() {
        return None;
    }
    let bytes = std::fs::read(&canonical).ok()?;
    Some(Asset { bytes: Cow::Owned(bytes), mime: mime_for(path).to_string() })
}

/// The content type a browser needs for each kind of file a Vite build and
/// pdf.js's data files contain. Anything else is served as bytes, which a
/// browser will not execute or render.
pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_serves_what_is_in_it_and_nothing_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dist");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(root.join("assets/app.js"), "1").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "no").unwrap();

        let assets = Assets::folder(root);
        let index = assets.get("index.html").unwrap();
        assert_eq!(index.mime, "text/html; charset=utf-8");
        assert_eq!(&*index.bytes, b"<!doctype html>");
        assert_eq!(assets.get("assets/app.js").unwrap().mime, "text/javascript; charset=utf-8");
        assert!(assets.get("../secret.txt").is_none());
        assert!(assets.get("/etc/passwd").is_none());
        assert!(assets.get("assets").is_none(), "a folder is not a file");
        assert!(assets.get("missing.js").is_none());
    }
}
