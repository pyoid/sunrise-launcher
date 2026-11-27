//! The eviction pass: stamping a build's caches as used, and evicting the least recently used.
use super::*;

impl Storage {
    pub(in crate::storage) fn stamp_build(&self) -> Result<(), Error> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        let root = Root::open(&self.root)?;
        let Some(directory) = root.directory.as_deref() else {
            return Ok(());
        };
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .follow(FollowSymlinks::No);
        let stamp = |parent: &Dir, name: &std::ffi::OsStr| -> Result<(), Error> {
            parent
                .open_dir_nofollow(name)?
                .open_with(".last-used", &options)?;
            Ok(())
        };
        for cache in [BuildCache::Cargo, BuildCache::Scratch] {
            let name = cache.relative();
            let path = self.root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) if linked(&meta) => return Err(Error::Unsafe(path)),
                Ok(meta) if meta.is_dir() => stamp(directory, name.as_ref())?,
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let rust = BuildCache::Rust.relative();
        let path = self.root.join(rust);
        if checked(&path, true)?.is_some() {
            let directory = directory.open_dir_nofollow(rust)?;
            for entry in directory.entries()? {
                let name = entry?.file_name();
                if usage_stamp(Path::new(&name)) {
                    continue;
                }
                let child = path.join(&name);
                let meta = fs::symlink_metadata(&child)?;
                if linked(&meta) {
                    return Err(Error::Unsafe(child));
                }
                if meta.is_dir()
                    && name
                        .to_string_lossy()
                        .starts_with(crate::rust_toolchain::VERSION)
                {
                    stamp(&directory, &name)?;
                }
            }
        }
        Ok(())
    }

    /// Not deletion candidates: an unproved worker can still be writing to them.
    pub(super) fn retained_compiler_bytes(
        &self,
        counted: &mut HashSet<(u64, u64)>,
    ) -> Result<Option<u64>, Error> {
        let native = self.root.join("native");
        let mut bytes = None;
        if checked(&native, true)?.is_some() {
            for entry in fs::read_dir(&native)? {
                let entry = entry?;
                if entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(native_mods::ATTEMPT_PREFIX)
                        || name.starts_with(native_mods::OLD_ATTEMPT_PREFIX)
                }) {
                    // A changing or unreadable tree is still protected; zero means its size is unknown.
                    let size = Tree::build(&entry.path()).map_or(0, |tree| tree.size(counted));
                    bytes = Some(bytes.unwrap_or(0u64).saturating_add(size));
                }
            }
        }
        Ok(bytes)
    }

    pub(in crate::storage) fn evict_inventory(
        &self,
        now: std::time::SystemTime,
        max_bytes: Option<u64>,
    ) -> Result<Report, Error> {
        self.evict_inventory_observed(now, max_bytes, |_| {})
    }

    pub(crate) fn evict_inventory_observed(
        &self,
        now: std::time::SystemTime,
        max_bytes: Option<u64>,
        mut before_clear: impl FnMut(&Path),
    ) -> Result<Report, Error> {
        use super::policy::{self, Tier, Unit};
        let entries = self.entries()?;
        for entry in &entries {
            entry.last_used()?;
        }
        let mut revisions = HashSet::new();
        let mut unknown_revision = false;
        let mut native_enabled = false;
        for instance in instances::list_in(&self.game, &self.data) {
            for bundle in crate::native_status::bundles_on(&instance.home) {
                if let Some(native) = bundle.manifest.native {
                    native_enabled = true;
                    revisions.insert(native.sunrise);
                }
            }
            match instance.resolve_version(None) {
                Ok(version) => {
                    if let Some(about) = native_mods::about(&version) {
                        revisions.insert(about.sunrise);
                    } else if let Some(nightly) = version.strip_prefix("nightly-") {
                        revisions.insert(nightly.rsplit('-').next().unwrap_or(nightly).to_owned());
                    } else {
                        unknown_revision = true;
                    }
                }
                Err(_) => unknown_revision = true,
            }
        }
        // One walk of each folder serves its unit size, its row's bytes and its kept size.
        let mut trees = entries
            .iter()
            .map(|entry| Tree::build(&entry.path))
            .collect::<Result<Vec<_>, Error>>()?;
        let mut seen = HashSet::new();
        let retained = self.retained_compiler_bytes(&mut seen)?;
        let mut units: Vec<_> = entries
            .iter()
            .zip(&trees)
            .map(|(entry, tree)| {
                let last_used = entry.last_used()?;
                let tier = if entry.relative.starts_with("rust")
                    && entry.path.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .starts_with(crate::rust_toolchain::VERSION)
                    })
                    && native_enabled
                {
                    // Conservatively retain the whole toolchain set while a native mod is on.
                    Tier::Protected
                } else if entry.leftover() {
                    Tier::Expiring
                } else if [
                    "native/source",
                    "native/tools",
                    "native/internals-sync",
                    "native/prebuilt",
                ]
                .iter()
                .any(|root| entry.relative.starts_with(root))
                {
                    let Some(unit_revision) = entry.revision() else {
                        return Ok(Unit {
                            eligible: false,
                            bytes: tree.size(&mut seen),
                            last_used,
                            tier: Tier::Protected,
                        });
                    };
                    let tool = entry.relative.starts_with("native/tools");
                    let used = unknown_revision
                        || (tool && native_mods::MIXINS_REV.get(..12) == Some(unit_revision))
                        || revisions.iter().any(|revision| {
                            let length = revision.len().min(unit_revision.len());
                            revision.get(..length) == unit_revision.get(..length)
                        });
                    if used && entry.category == Category::PrebuiltGames {
                        Tier::Lru
                    } else if used {
                        Tier::Protected
                    } else {
                        Tier::Expiring
                    }
                } else if entry.relative.starts_with("rust")
                    || entry
                        .path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(".trash-"))
                {
                    Tier::Expiring
                } else {
                    Tier::Lru
                };
                Ok(Unit {
                    eligible: entry.access()?.check().is_ok(),
                    bytes: tree.size(&mut seen),
                    last_used,
                    tier,
                })
            })
            .collect::<Result<_, Error>>()?;
        if let Some(bytes) = retained {
            units.push(Unit {
                bytes,
                tier: Tier::Protected,
                eligible: false,
                last_used: None,
            });
        }
        let selected = policy::select(&units, max_bytes, now);
        let mut report = self.apply_retaining(Vec::new(), None, false, true)?;
        for row in &mut report.rows {
            row.bytes = allocated(&entries, &trees, row.category);
        }
        for (index, entry) in entries.iter().enumerate() {
            let row = report
                .rows
                .iter_mut()
                .find(|row| row.category == entry.category)
                .expect("the report has a row for every category");
            if selected.contains(&index) {
                before_clear(&entry.path);
                let result = if entry.path == self.root.join("native/target") {
                    self.lease.clear().map(|_| ())
                } else {
                    entry.clear()
                };
                match result {
                    // Only a cleared folder is walked again, to see what is left of it.
                    Ok(()) => trees[index] = Tree::build(&entry.path)?,
                    // A refusal comes before any deletion: the folder is as it was walked.
                    Err(
                        error @ (Error::Busy
                        | Error::UnprovenExclusion
                        | Error::IncompleteUnit
                        | Error::UnpublishedGame
                        | Error::ResourceDependent(_)
                        | Error::ReferencedProfile
                        | Error::UnprovenReferences),
                    ) => row.kept.push(Kept {
                        bytes: trees[index].size(&mut HashSet::new()),
                        reason: native_mods::cache_error(error),
                    }),
                    Err(error) => return Err(error),
                }
            }
            let remaining = trees[index].size(&mut HashSet::new());
            if !selected.contains(&index) && remaining > 0 {
                row.kept.push(Kept {
                    bytes: remaining,
                    reason: if !units[index].eligible {
                        native_mods::cache_error(
                            entry
                                .access()?
                                .check()
                                .err()
                                .unwrap_or(Error::UnprovenExclusion),
                        )
                    } else {
                        crate::i18n::tr("Protected, recently used, or last use unknown.").into()
                    },
                });
            }
        }
        for row in &mut report.rows {
            row.remaining_bytes = allocated(&entries, &trees, row.category);
            row.freed_bytes = row.bytes.saturating_sub(row.remaining_bytes);
        }
        report.total_bytes = report.rows.iter().map(|row| row.bytes).sum();
        report.include_retained_compiler(retained);
        Ok(report)
    }
}
