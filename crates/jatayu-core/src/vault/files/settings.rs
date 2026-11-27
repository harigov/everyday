//! The files folder and the settings that choose how it is kept.

use super::Vault;
use super::helpers::unavailable;
use crate::error::{Error, Result};
use crate::files::disk::{self, Folder};
use crate::files::{
    FileFilter, FileOrigin, FilesSettings, FolderState, INTERNAL_DIR, StatusFilter, StorageKind,
};
use std::path::Path;

impl Vault {
    pub fn files_settings(&self) -> Result<FilesSettings> {
        self.with_files(|s| s.files_settings())
    }

    /// Validate and store. Trash days at most 3650. A changed `folder` must
    /// be an absolute path; it is not created here (see
    /// [`Vault::set_up_files_folder`]), and the catalog's relative paths are
    /// read against it from then on -- which is what lets somebody move the
    /// folder with a file manager and point Jatayu at the new place.
    ///
    /// Files found in the folder are plain by definition, so
    /// [`FileOrigin::Found`] is left out of `encrypted` rather than kept as a
    /// choice that could never take effect.
    pub fn save_files_settings(&self, settings: &FilesSettings) -> Result<FilesSettings> {
        self.writable()?;
        let mut settings = settings.clone();
        if settings.trash_days > 3650 {
            return Err(Error::Invalid("trash days must be at most 3650".into()));
        }
        if let Some(folder) = &mut settings.folder {
            let trimmed = folder.trim().to_string();
            if trimmed.is_empty() {
                return Err(Error::Invalid("the files folder must not be empty".into()));
            }
            if trimmed.len() > 4096 || trimmed.contains('\0') {
                return Err(Error::Invalid("that is not a folder path".into()));
            }
            if !Path::new(&trimmed).is_absolute() {
                return Err(Error::Invalid(format!(
                    "the files folder must be an absolute path, not {trimmed:?}"
                )));
            }
            *folder = trimmed;
        }
        settings.encrypted.retain(|o| *o != FileOrigin::Found);
        settings.encrypted.sort();
        settings.encrypted.dedup();
        self.with_files(|s| s.put_files_settings(&settings))?;
        self.files_settings()
    }

    /// The files folder, when it has been set up.
    ///
    /// The folder is kept where the settings say, whatever it is called: a
    /// person's `~/Karma Yoga` stays `~/Karma Yoga`. What is renamed is the
    /// application's own folder inside it, `.karmayoga` to [`INTERNAL_DIR`]
    /// ([`Folder::carry_over`]), here, before anything looks for the marker
    /// -- but only by a vault that can write, which no older build can have
    /// open at the same time. A vault opened read-only leaves it as it is,
    /// and the folder still reads as there.
    pub fn files_folder(&self) -> Result<Option<Folder>> {
        let folder = self.files_settings()?.folder.map(Folder::new);
        if let Some(folder) = &folder
            && self.write_lock.is_some()
            && let Err(e) = folder.carry_over()
        {
            // Not fatal here: the folder still reads as there under the old
            // name, and setting it up tries again and says what went wrong.
            tracing::warn!(error = %e, "could not carry the files folder's own things across");
        }
        Ok(folder)
    }

    /// Whether the folder can be used now.
    pub fn folder_state(&self) -> Result<FolderState> {
        match self.files_folder()? {
            Some(folder) => Ok(folder.state(self.expects_files()?)),
            None => Ok(FolderState::NotSetUp {
                suggested: disk::default_folder()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            }),
        }
    }

    /// Set the folder up at `path` (absolute), or at
    /// [`crate::files::disk::default_folder`] when `None`: create it, write
    /// the marker, store it in the settings. A folder that already has files
    /// in it is fine -- the next scan catalogues them.
    pub fn set_up_files_folder(&self, path: Option<&Path>) -> Result<FolderState> {
        self.writable()?;
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => disk::default_folder().ok_or_else(|| {
                Error::Invalid("this machine has no home folder to put the files folder in".into())
            })?,
        };
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "the files folder must be an absolute path, not {}",
                path.display()
            )));
        }
        let text = path
            .to_str()
            .ok_or_else(|| Error::Invalid("the files folder's path is not valid text".into()))?;
        Folder::new(&path).set_up()?;
        let mut settings = self.files_settings()?;
        settings.folder = Some(text.to_string());
        self.save_files_settings(&settings)?;
        self.folder_state()
    }

    /// The folder, set up at the default place first if it never was, and
    /// refused when unavailable. What every write of a plain file starts
    /// with.
    pub fn usable_folder(&self) -> Result<Folder> {
        self.writable()?;
        let folder = match self.files_folder()? {
            Some(folder) => folder,
            None => {
                self.set_up_files_folder(None)?;
                self.files_folder()?
                    .ok_or_else(|| Error::Invalid("the files folder could not be set up".into()))?
            }
        };
        match folder.state(self.expects_files()?) {
            FolderState::Available { .. } => {
                // A folder somebody made and pointed Settings at, or one
                // that was emptied by hand, may lack its marker or its
                // internal folders. Setting up again is safe: it only
                // creates what is missing.
                if !folder.root().join(INTERNAL_DIR).join(disk::MARKER).is_file() {
                    folder.set_up()?;
                }
                Ok(folder)
            }
            FolderState::Unavailable { path, reason } => Err(unavailable(&path, &reason)),
            FolderState::NotSetUp { .. } => {
                Err(Error::Invalid("the files folder has not been set up".into()))
            }
        }
    }

    /// The folder for *reading* a plain file: it must be there, but nothing
    /// is created or changed. A plain file is being read, so the catalog
    /// certainly expects files in the folder, and the count a general state
    /// check would make is skipped -- a film is read in many ranges.
    pub(super) fn available_folder(&self) -> Result<Folder> {
        let Some(folder) = self.files_folder()? else {
            return Err(Error::Invalid("the files folder has not been set up".into()));
        };
        match folder.state(true) {
            FolderState::Available { .. } => Ok(folder),
            FolderState::Unavailable { path, reason } => Err(unavailable(&path, &reason)),
            FolderState::NotSetUp { .. } => {
                Err(Error::Invalid("the files folder has not been set up".into()))
            }
        }
    }

    /// Whether the catalog has a plain file that is not in the trash: what
    /// makes a folder without its marker unavailable rather than new.
    fn expects_files(&self) -> Result<bool> {
        self.with_files(|s| {
            let filter = FileFilter {
                storage: Some(StorageKind::Plain),
                status: StatusFilter::Live,
                include_hidden: true,
                ..Default::default()
            };
            Ok(s.count_files(&filter)? > 0)
        })
    }
}
