//! The kept folder's own `CARGO_HOME`. Only downloads are shared with the other kept folders,
//! each `.crate` as a hard link and the index as a copy; the extracted sources
//! (`registry/src`) stay in this folder, so what a build script writes into one can't reach
//! another game build, and removing the folder undoes it.
use super::{invalid, Workspace, WORKER};
use crate::native_mods::links::{link_or_copy, Placed};
use std::{collections::BTreeSet, fs, io, path::Path};
use sunrise_launcher_smod::index::{sha256, sha256_file};

/// The stamp of the last fetch that succeeded, in the folder's cargo home.
const FETCHED: &str = ".fetched";

/// What the folder's downloads are tied to.
#[derive(Default)]
pub(super) struct Downloads {
    /// Run Cargo's fetch offline: the lock and the cache are as the last fetch left them.
    offline: bool,
    /// The mods' locks and the game build they were made for, when the game build is signed.
    digest: Option<String>,
    /// Every checksum the mods' locks pin.
    checksums: BTreeSet<String>,
}

/// What [`Workspace::fill_cargo`] did.
pub(in crate::native_mods) struct Filled {
    pub linked: usize,
    pub copied: usize,
    pub offline: bool,
}

impl Workspace {
    /// Fills this folder's cargo home from the shared one (`shared`), leaving files it already
    /// has. Cargo checks each `.crate` against the lock as it extracts it. The fetch can run
    /// offline when nothing was added and the last fetch here was for these locks (`compiled`
    /// holds one folder per mod) on this signed game build.
    pub(in crate::native_mods) fn fill_cargo(
        &mut self,
        shared: &Path,
        compiled: &Path,
        signed_sdk: Option<&str>,
    ) -> io::Result<Filled> {
        let own = self.dir.join("cargo");
        let mut filled = Filled {
            linked: 0,
            copied: 0,
            offline: false,
        };
        for (part, link) in [("cache", true), ("index", false)] {
            let to = own.join("registry").join(part);
            place_tree(&shared.join("registry").join(part), &to, link, &mut filled)?;
        }
        let mut mods = fs::read_dir(compiled)?.collect::<io::Result<Vec<_>>>()?;
        mods.sort_by_key(|entry| entry.file_name());
        let mut locks = String::new();
        for entry in mods {
            locks.push_str(&fs::read_to_string(entry.path().join("Cargo.lock"))?);
        }
        let checksums = locks
            .lines()
            .filter_map(|line| line.strip_prefix("checksum = \"")?.strip_suffix('"'))
            .map(str::to_owned)
            .collect();
        let digest = signed_sdk.map(|sdk| sha256(format!("{locks}\0{sdk}").as_bytes()));
        let stamp = own.join(FETCHED);
        // Deleted before the tool starts, written only after it succeeds: a failed build never
        // leaves a stamp that says the fetch is done.
        let last = fs::read_to_string(&stamp).ok();
        match fs::remove_file(&stamp) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        filled.offline = digest.is_some() && last == digest && filled.linked + filled.copied == 0;
        self.downloads = Downloads {
            offline: filled.offline,
            digest,
            checksums,
        };
        Ok(filled)
    }

    /// Whether Cargo's fetch runs offline.
    pub(super) fn offline(&self) -> bool {
        self.downloads.offline
    }

    /// Shares the crates this build downloaded, once the compiler's tree is proven gone and the
    /// build's outputs verified: only a `.crate` whose checksum a lock of this build pins, so a
    /// build script can't plant one that every other game build then trusts. Returns how many
    /// were shared and how many were left out. Writes the fetch stamp.
    pub(in crate::native_mods) fn publish_downloads(
        &self,
        shared: &Path,
    ) -> io::Result<(usize, usize)> {
        if self.dir.join(WORKER).exists() {
            return Ok((0, 0));
        }
        let (mut published, mut left_out) = (0, 0);
        let own = self.dir.join("cargo/registry/cache");
        let registries = match fs::read_dir(&own) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
            registries => registries?,
        };
        for registry in registries {
            let registry = registry?;
            let to = shared.join("registry/cache").join(registry.file_name());
            for entry in fs::read_dir(registry.path())? {
                let entry = entry?;
                let target = to.join(entry.file_name());
                if fs::symlink_metadata(&target).is_ok() {
                    continue;
                }
                if !self
                    .downloads
                    .checksums
                    .contains(&sha256_file(&entry.path()).map_err(io::Error::other)?)
                {
                    left_out += 1;
                    continue;
                }
                fs::create_dir_all(&to)?;
                link_or_copy(&entry.path(), &target)?;
                published += 1;
            }
        }
        if let Some(digest) = &self.downloads.digest {
            fs::write(self.dir.join("cargo").join(FETCHED), digest)?;
        }
        Ok((published, left_out))
    }
}

/// Places each regular file of `from` that `to` lacks: a hard link when `link`, else a clone or
/// copy. A link or anything but a file or folder in `from` is refused, never followed.
fn place_tree(from: &Path, to: &Path, link: bool, filled: &mut Filled) -> io::Result<()> {
    let entries = match fs::read_dir(from) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        let to = to.join(entry.file_name());
        if crate::native_cache::linked(&meta) {
            return Err(invalid());
        } else if meta.is_dir() {
            place_tree(&entry.path(), &to, link, filled)?;
        } else if !meta.is_file() {
            return Err(invalid());
        } else if fs::symlink_metadata(&to).is_ok() {
            continue;
        } else {
            fs::create_dir_all(to.parent().expect("a file under its folder"))?;
            let placed = if link {
                link_or_copy(&entry.path(), &to)?
            } else {
                crate::preparer::clone_or_copy(&entry.path(), &to)?;
                Placed::Copied
            };
            match placed {
                Placed::Linked => filled.linked += 1,
                Placed::Copied => filled.copied += 1,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
