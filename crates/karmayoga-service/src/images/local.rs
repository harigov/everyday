//! A quantized picture model, run by stable-diffusion.cpp's `sd-cli`.
//!
//! The weights are the ones the catalogue downloaded. The binary is the
//! pinned release, unzipped once beside that download, unless
//! `KARMAYOGA_SD_CLI` names one already on the machine.

use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self, ImageWeightPaths};
use karmayoga_core::imagegen::LocalImageModel;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A quantized model on a CPU can take several minutes for one picture.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Square side, in pixels. 512 is the size these quantized builds are
/// documented to finish in, including on a machine without much memory.
const SIDE: u32 = 512;

const ENV_BIN: &str = "KARMAYOGA_SD_CLI";

/// Run `bin` on `weights` and return the PNG it wrote.
pub fn paint(
    bin: &Path,
    model: LocalImageModel,
    weights: &ImageWeightPaths,
    prompt: &str,
) -> CommandResult<Vec<u8>> {
    let scratch = std::env::temp_dir().join(format!(
        "karmayoga-image-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&scratch).map_err(|e| {
        CommandError::new(codes::IO, format!("could not start the image model: {e}"))
    })?;
    let _guard = Scratch(scratch.clone());
    let out = scratch.join("out.png");

    let mut cmd = Command::new(bin);
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
        .arg("--vae-tiling");
    // The published Klein recipe turns this on. Z-Image's does not.
    if matches!(model, LocalImageModel::Flux2KleinQ4) {
        cmd.arg("--diffusion-fa");
    }
    let mut child = cmd
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
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            CommandError::new(
                codes::INVALID,
                format!("could not start the image engine ({}): {e}", bin.display()),
            )
        })?;

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
                return Err(CommandError::new(
                    codes::IO,
                    format!("the image engine stopped unexpectedly: {e}"),
                ));
            }
        }
    };

    if !status.success() {
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
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

/// The `sd-cli` to run: `KARMAYOGA_SD_CLI` when it is set, otherwise the
/// binary unzipped from the downloaded engine.
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
    let dir = archive.parent().unwrap_or(Path::new("."));
    let name = if cfg!(windows) { "sd-cli.exe" } else { "sd-cli" };
    let bin = dir.join(name);
    if bin.is_file() {
        return Ok(bin);
    }
    extract_cli(&archive, &bin)?;
    Ok(bin)
}

fn missing_engine() -> CommandError {
    CommandError::new(
        codes::INVALID,
        "The image engine has not been downloaded yet. Download it in Settings → Models.",
    )
}

fn extract_cli(archive: &Path, dest: &Path) -> CommandResult<()> {
    let file = std::fs::File::open(archive).map_err(|e| {
        CommandError::new(codes::IO, format!("could not open the image engine: {e}"))
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| {
        CommandError::new(codes::UNREADABLE, "the image engine download is not a zip archive")
    })?;
    let want = if cfg!(windows) { "sd-cli.exe" } else { "sd-cli" };
    let index = (0..zip.len()).find(|&i| {
        zip.by_index(i)
            .ok()
            .and_then(|entry| entry.enclosed_name().map(|p| p.to_path_buf()))
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .is_some_and(|n| n == want || n == "sd-cli" || n == "sd-cli.exe")
    });
    let Some(index) = index else {
        return Err(CommandError::new(
            codes::UNREADABLE,
            "the image engine download does not contain sd-cli",
        ));
    };
    let mut entry = zip.by_index(index).map_err(|_| {
        CommandError::new(codes::UNREADABLE, "the image engine download could not be read")
    })?;
    let partial = dest.with_extension("partial");
    {
        let mut out = std::fs::File::create(&partial).map_err(|e| {
            CommandError::new(codes::IO, format!("could not unpack the image engine: {e}"))
        })?;
        std::io::copy(&mut entry, &mut out).map_err(|e| {
            CommandError::new(codes::IO, format!("could not unpack the image engine: {e}"))
        })?;
        out.flush().ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&partial, dest).map_err(|e| {
        CommandError::new(codes::IO, format!("could not unpack the image engine: {e}"))
    })?;
    Ok(())
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    if trimmed.chars().count() <= 400 {
        return trimmed.to_string();
    }
    let skip = trimmed.chars().count() - 400;
    chars.by_ref().nth(skip.saturating_sub(1));
    chars.collect()
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

    #[test]
    fn a_stand_in_engine_is_handed_the_weights_and_its_png_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("sd-cli");
        std::fs::write(
            &bin,
            "#!/bin/sh\n\
             out=\n\
             prev=\n\
             for a in \"$@\"; do\n\
               if [ \"$prev\" = -o ]; then out=$a; fi\n\
               prev=$a\n\
             done\n\
             printf '%s\\n' \"$@\" > \"${out}.args\"\n\
             printf '\\211PNG\\r\\n\\032\\n' > \"$out\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }

        let weights = ImageWeightPaths {
            diffusion: dir.path().join("diffusion.gguf"),
            text_encoder: dir.path().join("encoder.gguf"),
            vae: dir.path().join("vae.safetensors"),
        };
        let png = paint(&bin, LocalImageModel::Flux2KleinQ4, &weights, "a red fox").unwrap();
        assert!(png.starts_with(b"\x89PNG"));

        let args = std::fs::read_dir(std::env::temp_dir()).ok();
        // The scratch directory is removed when paint returns, so the args
        // file is gone with it. Re-run into a script that keeps the args
        // beside the binary instead.
        let _ = args;
        std::fs::write(
            &bin,
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
             out=\n\
             prev=\n\
             for a in \"$@\"; do\n\
               if [ \"$prev\" = -o ]; then out=$a; fi\n\
               prev=$a\n\
             done\n\
             printf '\\211PNG\\r\\n\\032\\n' > \"$out\"\n",
        )
        .unwrap();
        paint(&bin, LocalImageModel::Flux2KleinQ4, &weights, "a red fox").unwrap();
        let recorded = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(recorded.contains("diffusion.gguf"), "{recorded}");
        assert!(recorded.contains("encoder.gguf"), "{recorded}");
        assert!(recorded.contains("vae.safetensors"), "{recorded}");
        assert!(recorded.contains("--steps\n4\n") || recorded.contains("--steps\n4"), "{recorded}");
        assert!(recorded.contains("--diffusion-fa"), "{recorded}");
        assert!(recorded.contains("a red fox"), "{recorded}");
    }
}
