//! A quantized picture model, run by stable-diffusion.cpp's `sd-cli`.
//!
//! The weights are the ones the catalogue downloaded. The binary is the
//! pinned release, unpacked beside that download, unless `KARMAYOGA_SD_CLI`
//! names one already on the machine.

use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self, ImageWeightPaths};
use karmayoga_core::imagegen::LocalImageModel;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A quantized model on a CPU can take several minutes for one picture.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Square side, in pixels. 512 is the size these quantized builds are
/// documented to finish in, including on a machine without much memory.
const SIDE: u32 = 512;

const ENV_BIN: &str = "KARMAYOGA_SD_CLI";

const BIN: &str = if cfg!(windows) { "sd-cli.exe" } else { "sd-cli" };

/// What an unpacked engine's directory is called, before the checksum of the
/// archive it came from.
const ENGINE_DIR: &str = "engine-";

/// How much of what `sd-cli` prints is kept: its last words, which are where
/// a failure is explained.
const STDERR_KEPT: usize = 64 * 1024;

/// Run `bin` on `weights` and return the PNG it wrote.
pub fn paint(
    bin: &Path,
    model: LocalImageModel,
    weights: &ImageWeightPaths,
    prompt: &str,
) -> CommandResult<Vec<u8>> {
    let scratch = std::env::temp_dir().join(format!("karmayoga-image-{}", unique()));
    std::fs::create_dir_all(&scratch).map_err(|e| {
        CommandError::new(codes::IO, format!("could not start the image model: {e}"))
    })?;
    let _guard = Scratch(scratch.clone());
    let out = scratch.join("out.png");

    let mut cmd = Command::new(bin);
    // Both published recipes -- Klein's model card, and the stable-diffusion.cpp
    // guide to Z-Image on a 4 GB card -- turn on flash attention beside the
    // offload, which is what keeps the peak down on a small machine.
    cmd.arg("--diffusion-model")
        .arg(&weights.diffusion)
        .arg("--vae")
        .arg(&weights.vae)
        .arg("--llm")
        .arg(&weights.text_encoder)
        .arg("--cfg-scale")
        .arg("1.0")
        .arg("--steps")
        .arg(model.steps().to_string())
        .arg("--offload-to-cpu")
        .arg("--diffusion-fa")
        .arg("--vae-tiling")
        .arg("-W")
        .arg(SIDE.to_string())
        .arg("-H")
        .arg(SIDE.to_string())
        .arg("-p")
        .arg(prompt)
        .arg("-o")
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // No console window over the application for the minutes a picture
        // takes, nor one that ends the run when somebody closes it.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = spawn(&mut cmd).map_err(|e| {
        CommandError::new(
            codes::INVALID,
            format!("could not start the image engine ({}): {e}", bin.display()),
        )
    })?;
    let stderr = drain_tail(child.stderr.take());

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CommandError::new(
                    codes::TIMED_OUT,
                    format!("{} did not finish within ten minutes", model.name()),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => {
                let _ = child.kill();
                return Err(CommandError::new(
                    codes::IO,
                    format!("the image engine stopped unexpectedly: {e}"),
                ));
            }
        }
    };

    if !status.success() {
        // Something the engine started could hold the pipe open after it has
        // gone; wait a little for what it wrote, not for that.
        let err = stderr.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        let err = String::from_utf8_lossy(&err);
        if let Some(why) = too_old(&err) {
            return Err(CommandError::new(codes::UNSUPPORTED, why));
        }
        let tail = tail(&err);
        return Err(CommandError::new(
            codes::INVALID,
            if tail.is_empty() {
                format!("{} failed ({status})", model.name())
            } else {
                format!("{} failed: {tail}", model.name())
            },
        ));
    }

    let png = std::fs::read(&out).map_err(|e| {
        CommandError::new(
            codes::UNREADABLE,
            format!("{} did not write a picture: {e}", model.name()),
        )
    })?;
    if png.is_empty() || !png.starts_with(b"\x89PNG") {
        return Err(CommandError::new(
            codes::UNREADABLE,
            format!("{} did not write a picture", model.name()),
        ));
    }
    Ok(png)
}

/// Start `cmd`, waiting out "text file busy".
///
/// Linux will not run a file that is open for writing anywhere, and a file
/// this process wrote a moment ago -- the engine, just unpacked -- can still
/// be open in a child another thread forked meanwhile, until that child's own
/// exec closes it. A few milliseconds, so a few short waits.
fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
    let mut busy = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && busy < 20 => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            outcome => return outcome,
        }
    }
}

/// Read a pipe to its end on a thread of its own, keeping the last
/// [`STDERR_KEPT`] bytes.
///
/// Not `backup::creds`'s `drain`, which keeps the first megabyte and stops
/// reading there: `sd-cli` logs every step, a child whose pipe is closed can
/// die of it mid-picture, and what is wanted is the end, where a failure is
/// explained. Read the whole time, so a child that fills the pipe never
/// stalls waiting for somebody to empty it.
fn drain_tail(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 8 * 1024];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        kept.extend_from_slice(&chunk[..n]);
                        if kept.len() > 2 * STDERR_KEPT {
                            kept.drain(..kept.len() - STDERR_KEPT);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            if kept.len() > STDERR_KEPT {
                kept.drain(..kept.len() - STDERR_KEPT);
            }
            let _ = sender.send(kept);
        });
    }
    receiver
}

/// What to say when the engine could not even be loaded, because this
/// system is older than the one it was built for -- see `SD_CLI_SOURCES` in
/// the catalogue. The loader's own words name a symbol version and help
/// nobody.
fn too_old(stderr: &str) -> Option<String> {
    let glibc = stderr.contains("GLIBC") && stderr.contains("not found");
    let dyld = stderr.contains("dyld")
        && (stderr.contains("newer") || stderr.contains("Symbol not found"));
    if !glibc && !dyld {
        return None;
    }
    let needs = if dyld {
        "macOS 26 or later"
    } else {
        "Ubuntu 24.04 or later, or another Linux with glibc 2.38"
    };
    Some(format!(
        "The image engine does not run on this system: it needs {needs}. {ENV_BIN} can name an \
         sd-cli built on this computer instead."
    ))
}

/// The `sd-cli` to run: `KARMAYOGA_SD_CLI` when it is set, otherwise the
/// one unpacked from the downloaded engine.
///
/// Reads and writes files, so it is called from a blocking thread.
pub fn engine() -> CommandResult<PathBuf> {
    if let Some(over) = std::env::var_os(ENV_BIN) {
        let path = PathBuf::from(over);
        if path.as_os_str().is_empty() {
            return Err(missing_engine());
        }
        return Ok(path);
    }
    let Some(archive) = files::sd_cli_archive() else {
        return Err(missing_engine());
    };
    unpacked(&archive.path, archive.sha256)
}

fn missing_engine() -> CommandError {
    CommandError::new(
        codes::INVALID,
        "The image engine has not been downloaded yet. Download it in Settings → Models.",
    )
}

/// The `sd-cli` in `archive`, unpacked beside it with everything else the
/// archive holds.
///
/// All of it, not `sd-cli` alone: it is linked against `libstable-diffusion`
/// and the ggml libraries packed with it, and looks for them beside itself
/// (`$ORIGIN`, `@executable_path`, a DLL's own directory). Into a directory
/// named for the archive's checksum, so that an engine a later build pins,
/// downloaded over this one, is unpacked afresh rather than the old binary
/// run on newer weights or flags. Used as it is only when every file the
/// archive lists is there at its size -- a Delete that stopped halfway, on a
/// library still in use on Windows, can leave the binary without what it
/// loads. And renamed into place whole, so that a directory is never seen
/// half-written.
///
/// Called under `images::PAINTING`, so nothing else in this process is
/// unpacking, or running from the directory being replaced.
fn unpacked(archive: &Path, sha256: &str) -> CommandResult<PathBuf> {
    let home = archive.parent().unwrap_or(Path::new("."));
    let dir = home.join(format!("{ENGINE_DIR}{}", &sha256[..sha256.len().min(16)]));
    let bin = dir.join(BIN);
    let file = std::fs::File::open(archive).map_err(|e| {
        CommandError::new(codes::IO, format!("could not open the image engine: {e}"))
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| {
        CommandError::new(codes::UNREADABLE, "the image engine download is not a zip archive")
    })?;
    let members = members(&mut zip)?;
    if !members.iter().any(|m| m.name == BIN) {
        return Err(CommandError::new(
            codes::UNREADABLE,
            "the image engine download does not contain sd-cli",
        ));
    }
    if whole(&dir, &members) {
        return Ok(bin);
    }
    let staging = home.join(format!(".unpacking-{}", unique()));
    let outcome = unpack_into(&mut zip, &members, &staging).and_then(|()| {
        // Whatever an earlier, broken copy left behind.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&staging, &dir)
            .or_else(|e| if whole(&dir, &members) { Ok(()) } else { Err(unpack_failed(e)) })
    });
    let _ = std::fs::remove_dir_all(&staging);
    outcome?;
    forget_other_engines(home, &dir);
    Ok(bin)
}

/// One file the engine's archive holds, and what it unpacks to.
struct Member {
    index: usize,
    name: std::ffi::OsString,
    bytes: u64,
}

/// Every file in the archive that lands somewhere. An entry that names a
/// directory, or a link, is passed over: the archives pinned are flat, and
/// nothing in one decides where a file lands except its own name.
fn members(zip: &mut zip::ZipArchive<std::fs::File>) -> CommandResult<Vec<Member>> {
    let mut members = Vec::new();
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(|_| unreadable())?;
        if !entry.is_file() {
            continue;
        }
        let Some(path) = entry.enclosed_name() else { continue };
        let mut parts = path.components();
        if let (Some(Component::Normal(name)), None) = (parts.next(), parts.next()) {
            members.push(Member { index, name: name.to_os_string(), bytes: entry.size() });
        }
    }
    Ok(members)
}

/// Whether `dir` holds every member, each at its size.
fn whole(dir: &Path, members: &[Member]) -> bool {
    members.iter().all(|m| {
        std::fs::metadata(dir.join(&m.name)).is_ok_and(|f| f.is_file() && f.len() == m.bytes)
    })
}

fn unpack_into(
    zip: &mut zip::ZipArchive<std::fs::File>,
    members: &[Member],
    dest: &Path,
) -> CommandResult<()> {
    std::fs::create_dir_all(dest).map_err(unpack_failed)?;
    for member in members {
        let mut entry = zip.by_index(member.index).map_err(|_| unreadable())?;
        let target = dest.join(&member.name);
        let mut out = std::fs::File::create(&target).map_err(unpack_failed)?;
        std::io::copy(&mut entry, &mut out).map_err(unpack_failed)?;
        drop(out);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                .map_err(unpack_failed)?;
        }
    }
    Ok(())
}

fn unreadable() -> CommandError {
    CommandError::new(codes::UNREADABLE, "the image engine download could not be read")
}

/// Engines unpacked from an archive no longer downloaded. Best effort: one
/// that will not go now goes with the model's folder.
fn forget_other_engines(home: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(home) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = entry.file_name().to_str().is_some_and(|n| n.starts_with(ENGINE_DIR));
        if ours && path != keep && path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn unpack_failed(e: std::io::Error) -> CommandError {
    CommandError::new(codes::IO, format!("could not unpack the image engine: {e}"))
}

/// A name nothing else in this process, or any other, is using.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= 400 {
        return trimmed.to_string();
    }
    trimmed.chars().skip(count - 400).collect()
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn weights(dir: &Path) -> ImageWeightPaths {
        ImageWeightPaths {
            diffusion: dir.join("diffusion.gguf"),
            text_encoder: dir.join("encoder.gguf"),
            vae: dir.join("vae.safetensors"),
        }
    }

    /// A stand-in `sd-cli`: a shell script that keeps its arguments beside
    /// itself, runs `body`, and writes a PNG header to `-o`.
    #[cfg(unix)]
    fn stand_in(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("sd-cli");
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
             {body}\n\
             out=\n\
             prev=\n\
             for a in \"$@\"; do\n\
               if [ \"$prev\" = -o ]; then out=$a; fi\n\
               prev=$a\n\
             done\n\
             printf '\\211PNG\\r\\n\\032\\n' > \"$out\"\n"
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[cfg(unix)]
    #[test]
    fn a_stand_in_engine_is_handed_the_weights_and_its_png_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stand_in(dir.path(), "");
        for model in LocalImageModel::ALL {
            let png = paint(&bin, model, &weights(dir.path()), "a red fox").unwrap();
            assert!(png.starts_with(b"\x89PNG"));
            let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
            let args: Vec<&str> = args.lines().collect();
            let after = |flag: &str| {
                let at = args.iter().position(|a| *a == flag).unwrap_or_else(|| {
                    panic!("{flag} was not passed: {args:?}");
                });
                args[at + 1]
            };
            assert!(after("--diffusion-model").ends_with("diffusion.gguf"));
            assert!(after("--llm").ends_with("encoder.gguf"));
            assert!(after("--vae").ends_with("vae.safetensors"));
            assert_eq!(after("--steps"), model.steps().to_string());
            assert_eq!(after("-p"), "a red fox");
            assert!(args.contains(&"--diffusion-fa"), "{model:?}: {args:?}");
            assert!(args.contains(&"--offload-to-cpu"), "{model:?}: {args:?}");
        }
    }

    /// More than a pipe holds, written before the engine exits: the run ends
    /// when the engine does, not at the ten-minute timeout.
    #[cfg(unix)]
    #[test]
    fn an_engine_that_says_a_lot_does_not_stall() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stand_in(dir.path(), "head -c 1000000 /dev/zero | tr '\\0' x >&2");
        let started = Instant::now();
        paint(&bin, LocalImageModel::Flux2KleinQ4, &weights(dir.path()), "a fox").unwrap();
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_is_explained_in_the_engines_last_words() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stand_in(dir.path(), "echo 'loading' >&2; echo 'unknown model type' >&2; exit 3");
        let err =
            paint(&bin, LocalImageModel::ZImageTurboQ4, &weights(dir.path()), "a fox").unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert!(err.message.contains("unknown model type"), "{}", err.message);

        let bin = stand_in(
            dir.path(),
            "echo \"sd-cli: /lib/x86_64-linux-gnu/libc.so.6: version \\`GLIBC_2.38' not found\" >&2; exit 1",
        );
        let err =
            paint(&bin, LocalImageModel::ZImageTurboQ4, &weights(dir.path()), "a fox").unwrap_err();
        assert_eq!(err.code, codes::UNSUPPORTED);
        assert!(err.message.contains("Ubuntu 24.04"), "{}", err.message);
    }

    fn archive(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn the_whole_engine_is_unpacked_beside_its_binary_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let zip = archive(
            dir.path(),
            "sd-cli.zip",
            &[
                (BIN, b"binary"),
                ("libstable-diffusion.so", b"library"),
                ("ggml-cpu-haswell.dll", b"backend"),
                ("nested/elsewhere.so", b"not flat"),
            ],
        );
        let bin = unpacked(&zip, "0123456789abcdef0123").unwrap();
        let engine = bin.parent().unwrap();
        assert_eq!(engine, dir.path().join("engine-0123456789abcdef"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"binary");
        assert_eq!(std::fs::read(engine.join("libstable-diffusion.so")).unwrap(), b"library");
        assert_eq!(std::fs::read(engine.join("ggml-cpu-haswell.dll")).unwrap(), b"backend");
        assert!(!engine.join("elsewhere.so").exists());
        assert!(!engine.join("nested").exists());

        // Found, not unpacked again.
        std::fs::write(&bin, b"BINARY").unwrap();
        assert_eq!(unpacked(&zip, "0123456789abcdef0123").unwrap(), bin);
        assert_eq!(std::fs::read(&bin).unwrap(), b"BINARY");

        // Unpacked again when part of it has gone.
        std::fs::remove_file(engine.join("libstable-diffusion.so")).unwrap();
        assert_eq!(unpacked(&zip, "0123456789abcdef0123").unwrap(), bin);
        assert_eq!(std::fs::read(engine.join("libstable-diffusion.so")).unwrap(), b"library");
        assert_eq!(std::fs::read(&bin).unwrap(), b"binary");

        // Nothing left half-done beside it.
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with(".unpacking")), "{names:?}");
    }

    #[test]
    fn a_newer_engine_is_unpacked_afresh_and_the_old_one_goes() {
        let dir = tempfile::tempdir().unwrap();
        let zip = archive(dir.path(), "sd-cli.zip", &[(BIN, b"old")]);
        let old = unpacked(&zip, "aaaaaaaaaaaaaaaa").unwrap();

        let zip = archive(dir.path(), "sd-cli.zip", &[(BIN, b"new")]);
        let new = unpacked(&zip, "bbbbbbbbbbbbbbbb").unwrap();
        assert_ne!(old, new);
        assert_eq!(std::fs::read(&new).unwrap(), b"new");
        assert!(!old.exists(), "the old engine is still there");
    }

    #[test]
    fn an_archive_without_the_binary_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let zip = archive(dir.path(), "sd-cli.zip", &[("README.txt", b"no binary")]);
        let err = unpacked(&zip, "cccccccccccccccc").unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
    }

    #[test]
    fn only_a_loader_that_could_not_start_the_engine_is_called_too_old() {
        assert!(too_old("version `GLIBCXX_3.4.32' not found (required by sd-cli)").is_some());
        assert!(
            too_old("dyld[1]: sd-cli built for macOS 26.0 which is newer than running OS")
                .unwrap()
                .contains("macOS 26")
        );
        assert!(too_old("error: model file not found").is_none());
        assert!(too_old("").is_none());
    }
}
