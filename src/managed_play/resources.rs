//! Managed Play reads the exact prepared tree saved with its generation. The ordinary
//! preparation cache remains available for a later Prepare, without changing an active game.
use super::{artifact, digest_file, optional_bytes, plain_file, pool, require_dir, unsettled};
use crate::{
    backend, game_log::launcher as launcher_log, native_mods::Cancellation, preparer,
    process::leases::ProfileHandoff, profile_txn::durable,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

const ROOT: &str = "launcher-prepared";
const EVIDENCE: &str = "data-file.json";
const CURRENT: &str = "current.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Resources {
    primary: [u8; 32],
    accelerator: Option<[u8; 32]>,
}

impl Resources {
    pub(super) fn capture(
        base: &str,
        stage: &crate::profile_txn::Staging<'_>,
    ) -> Result<Self, String> {
        let root = backend::version_dir(base);
        let preparation = backend::prepare_lock(base)?;
        match Self::current(&root)? {
            Some(resources) => Ok(resources),
            None => Self::prepare(base, &preparation, Some(stage.handoff())),
        }
    }

    fn current(root: &Path) -> Result<Option<Self>, String> {
        let Some(resources) = optional_bytes(&receipt(root)?)?
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
        else {
            return Ok(None);
        };
        match backend::check_play_output(root, &resources.path_at(root)) {
            Ok(()) => Ok(Some(resources)),
            Err(backend::Unprepared::Incomplete | backend::Unprepared::ServerOnly) => Ok(None),
            Err(issue) => Err(issue.message(|| unsettled!())),
        }
    }

    /// Select current resources for first Play or advance an old profile on the same base.
    /// The version lock and the admitted profile are inherited by any preparer children.
    pub(super) fn for_play(
        base: &str,
        profile: ProfileHandoff<'_>,
        log: &dyn Fn(String),
    ) -> Result<Self, String> {
        let preparation = backend::prepare_lock(base)?;
        if let Some(resources) = Self::current(&backend::version_dir(base))? {
            return Ok(resources);
        }
        let handoff = preparation.handoff(Some(profile));
        if !backend::check_prepared(base) {
            backend::reuse_prepared(&backend::versions_dir(), base);
            if !backend::check_prepared(base) {
                let minecraft = backend::Settings::load()
                    .minecraft_dir
                    .unwrap_or_else(|| preparer::minecraft_dir(None));
                let (command, minecraft) = backend::preparer_staged(
                    base,
                    &minecraft,
                    backend::MinecraftFrom::Install,
                    log,
                    &handoff,
                )?;
                backend::sounds_staged(base, &minecraft, log, &handoff);
                backend::set_prepare_step(backend::PrepareStep::Prepare);
                preparer::run_staged(command, &handoff, log)?;
            }
        }
        Self::seal_prepared(base, &preparation, &handoff, None, None)
    }

    /// Finish the shared preparation under its existing lock, before a profile needs it.
    pub(crate) fn prepare(
        base: &str,
        preparation: &preparer::Preparation,
        profile: Option<ProfileHandoff<'_>>,
    ) -> Result<Self, String> {
        Self::seal_prepared(base, preparation, &preparation.handoff(profile), None, None)
    }

    /// [`Self::prepare`] when the caller already probed `missing-extras` unlocked (startup).
    pub(crate) fn prepare_with_missing(
        base: &str,
        preparation: &preparer::Preparation,
        profile: Option<ProfileHandoff<'_>>,
        missing: Option<preparer::Extras>,
    ) -> Result<Self, String> {
        Self::seal_prepared(
            base,
            preparation,
            &preparation.handoff(profile),
            None,
            Some(missing),
        )
    }

    /// [`Self::prepare`], stopping at the next step once `stop` is set.
    pub(crate) fn prepare_cancellable(
        base: &str,
        preparation: &preparer::Preparation,
        profile: Option<ProfileHandoff<'_>>,
        stop: Option<&crate::native_mods::Cancellation>,
    ) -> Result<Self, String> {
        let handoff = match stop {
            Some(stop) => preparation.handoff_cancellable(profile, stop),
            None => preparation.handoff(profile),
        };
        Self::seal_prepared(base, preparation, &handoff, stop, None)
    }

    fn seal_prepared(
        base: &str,
        preparation: &preparer::Preparation,
        handoff: &preparer::Handoff<'_>,
        stop: Option<&crate::native_mods::Cancellation>,
        probed_missing: Option<Option<preparer::Extras>>,
    ) -> Result<Self, String> {
        let check = || stop.map_or(Ok(()), crate::native_mods::Cancellation::check);
        check()?;
        backend::need_base_prepared(base, Some(preparation))?;
        match probed_missing {
            None => backend::complete_assets_staged(base, handoff),
            Some(Some(lacking)) if !lacking.is_empty() => {
                backend::complete_assets_lacking_staged(base, handoff, lacking);
            }
            Some(_) => {}
        }
        check()?;
        let root = backend::version_dir(base);
        Self::seal(&root, &backend::resources(base), stop, |resources| {
            check()?;
            let Some(binary) = preparer::binary(&root) else {
                return Ok(None);
            };
            launcher_log::play_detail(crate::i18n::tr("Preparing the game"));
            let step = launcher_log::step("Packing game data cache");
            match preparer::data_file(&binary, &root, resources, Some(handoff)) {
                Ok(Some(true)) => {
                    check()?;
                    let digest = digest_file(&resources.join(preparer::DATA_FILE))
                        .ok()
                        .flatten();
                    step.done("");
                    Ok(digest)
                }
                Ok(_) => {
                    step.done("already present");
                    Ok(None)
                }
                Err(error) if stop.is_some_and(crate::native_mods::Cancellation::is_stopped) => {
                    step.failed(&error);
                    Err(error)
                }
                Err(error) => {
                    step.failed(&error);
                    crate::elog!("{error}");
                    Ok(None)
                }
            }
        })
    }

    pub(crate) fn invalidate(root: &Path) -> io::Result<()> {
        durable::retire(&receipt(root).map_err(io::Error::other)?)
    }

    pub(super) fn path(&self, base: &str) -> PathBuf {
        self.path_at(&backend::version_dir(base))
    }

    fn path_at(&self, root: &Path) -> PathBuf {
        parent(root).join(key(self.primary)).join("resources")
    }

    pub(super) fn check_play_output(&self, base: &str) -> Result<(), backend::Unprepared> {
        let root = backend::version_dir(base);
        backend::check_play_output(&root, &self.path_at(&root))
    }

    pub(super) fn verify(&self, base: &str) -> Result<(), String> {
        let root = backend::version_dir(base);
        let path = self.path(base);
        for dir in [
            &root,
            &root.join(".local"),
            &parent(&root),
            path.parent().expect("snapshot parent"),
        ] {
            require_dir(dir)?;
        }
        launcher_log::play_detail(crate::i18n::tr("Checking the game files"));
        let meter = launcher_log::PlayMeter::new(|done, total| {
            crate::i18n::trf(
                "Checking the game files: {done} of {total}",
                &[("done", &done.to_string()), ("total", &total.to_string())],
            )
        });
        let on_done = |done, total| meter.tick(done, total);
        let actual = logged("Verifying game files", || {
            Ok(
                match artifact::tree_with_progress(
                    &path,
                    true,
                    pool::workers(),
                    None,
                    Some(&on_done),
                )? {
                    Some((digest, files)) => (Some(digest), files),
                    None => (None, 0),
                },
            )
        })?;
        if actual != Some(self.primary) {
            return Err(setup_error(&path));
        }
        Ok(())
    }

    pub(super) fn allow_accelerator(&self, base: &str) -> bool {
        self.accelerator.is_some_and(|digest| {
            digest_file(&self.path(base).join(preparer::DATA_FILE))
                .is_ok_and(|found| found == Some(digest))
        })
    }

    // When the source is prepared, flush a private primary tree and publish it without
    // replacing anything. Build the optional cache against the final path: the game caches
    // the canonical resource root inside it. A kill after exposure but before its evidence
    // write leaves a usable primary tree and an untrusted accelerator, never the reverse.
    fn seal(
        root: &Path,
        source: &Path,
        stop: Option<&Cancellation>,
        accelerator: impl FnOnce(&Path) -> Result<Option<[u8; 32]>, String>,
    ) -> Result<Self, String> {
        Self::seal_with(root, source, pool::workers(), stop, accelerator)
    }

    // Each pass (hash, copy, flush) runs on `workers` threads and joins them before the next.
    fn seal_with(
        root: &Path,
        source: &Path,
        workers: usize,
        stop: Option<&Cancellation>,
        accelerator: impl FnOnce(&Path) -> Result<Option<[u8; 32]>, String>,
    ) -> Result<Self, String> {
        let relative = source.strip_prefix(root).map_err(|_| setup_error(source))?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(setup_error(source));
        }
        for ancestor in source.ancestors().take_while(|path| *path != root) {
            require_dir(ancestor)?;
        }
        require_dir(root)?;
        launcher_log::play_detail(crate::i18n::tr("Sealing the game files"));
        // One monotonic count across hashing, copying and flushing so the count never restarts.
        let seal_meter = launcher_log::PlayMeter::new(|done, total| {
            crate::i18n::trf(
                "Sealing the game files: {done} of {total}",
                &[("done", &done.to_string()), ("total", &total.to_string())],
            )
        });
        let on_hash = |done, total: usize| seal_meter.tick(done, total.saturating_mul(3));
        let (primary, hashed_files) = logged("Sealing game files: hashing", || {
            Ok(
                match artifact::tree_with_progress(source, true, workers, stop, Some(&on_hash))? {
                    Some((digest, files)) => ((Some(digest), files), files),
                    None => ((None, 0), 0),
                },
            )
        })?;
        let Some(primary) = primary else {
            return Err(setup_error(source));
        };
        let parent = parent(root);
        fs::create_dir_all(&parent).map_err(|e| at(&parent, e))?;
        require_dir(&parent)?;
        let snapshot = parent.join(key(primary));
        let resources = snapshot.join("resources");
        let evidence = snapshot.join(EVIDENCE);
        let exists = snapshot.exists();
        if exists {
            require_dir(&snapshot)?;
        }
        let accelerator = if resources.is_dir() {
            optional_bytes(&evidence)?.and_then(|bytes| serde_json::from_slice(&bytes).ok())
        } else {
            // A move from .local flushes both directories, including the new store's entry.
            let stage = tempfile::Builder::new()
                .prefix(".prepared-")
                .tempdir_in(root.join(".local"))
                .map_err(|error| at(root, error))?;
            let candidate = stage.path().join("resources");
            copy_and_flush(source, &candidate, workers, stop, &seal_meter, hashed_files)?;
            let (from, to) = if exists {
                (candidate.as_path(), &resources)
            } else {
                (stage.path(), &snapshot)
            };
            durable::move_new_directory(from, to).map_err(|e| at(to, e))?;
            let digest = accelerator(&resources)?;
            if let Some(digest) = digest {
                write_receipt(&evidence, &digest)?;
            }
            digest
        };
        let prepared = Self {
            primary,
            accelerator,
        };
        write_receipt(&parent.join(CURRENT), &prepared)?;
        Ok(prepared)
    }
}

pub(super) fn generated_cache(name: &OsStr) -> bool {
    name == preparer::DATA_FILE
        || name.to_str().is_some_and(|name| {
            name.strip_prefix("sunrise-data.bin.")
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|c| c.is_ascii_digit()))
        })
}

fn parent(root: &Path) -> PathBuf {
    root.join(".local").join(ROOT)
}
fn receipt(root: &Path) -> Result<PathBuf, String> {
    let parent = parent(root);
    if parent.exists() {
        for dir in [root, &root.join(".local"), &parent] {
            require_dir(dir)?;
        }
    }
    Ok(parent.join(CURRENT))
}
// These receipts only cache selection or accelerator evidence. A crash can lose them; the
// primary tree is already durable and a profile's independent digest is checked at cutoff.
fn write_receipt(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().expect("snapshot directory"))
        .map_err(|e| at(path, e))?;
    serde_json::to_writer(&mut staged, value).map_err(|e| e.to_string())?;
    staged
        .persist(path)
        .map(|_| ())
        .map_err(|e| at(path, e.error))
}
fn key(digest: [u8; 32]) -> String {
    format!("{:x}", sha2::digest::Output::<Sha256>::from(digest))
}

/// Start and end lines in the launcher log for one pass over a snapshot's files, with how many.
fn logged<T>(
    what: &str,
    run: impl FnOnce() -> Result<(T, usize), pool::Error>,
) -> Result<T, pool::Error> {
    let step = crate::game_log::launcher::step(what);
    match run() {
        Ok((value, files)) => {
            step.done(format!("{files} files"));
            Ok(value)
        }
        Err(error) => {
            step.failed(error.to_string());
            Err(error)
        }
    }
}

/// Copy and flush one staged tree after its hashes have been counted by the same meter.
fn copy_and_flush(
    source: &Path,
    candidate: &Path,
    workers: usize,
    stop: Option<&Cancellation>,
    seal_meter: &launcher_log::PlayMeter,
    hashed_files: usize,
) -> Result<(), pool::Error> {
    let on_copy = |done, total: usize| {
        seal_meter.tick(
            hashed_files.saturating_add(done),
            hashed_files.saturating_add(total.saturating_mul(2)),
        )
    };
    let copied = logged("Sealing game files: copying", || {
        let copied = copy_tree_with(source, candidate, true, 0, workers, stop, Some(&on_copy))?;
        let files = copied.files.len();
        Ok((copied, files))
    })?;
    let copied_files = copied.files.len();
    let on_flush = |done, total: usize| {
        seal_meter.tick(
            hashed_files
                .saturating_add(copied_files)
                .saturating_add(done),
            hashed_files
                .saturating_add(copied_files)
                .saturating_add(total),
        )
    };
    logged("Sealing game files: flushing", || {
        flush_copied(&copied, workers, stop, Some(&on_flush))?;
        Ok(((), copied_files))
    })
}

pub(super) fn copy_tree(
    source: &Path,
    dest: &Path,
    skip_accelerator: bool,
    depth: usize,
) -> Result<(), String> {
    copy_tree_with(
        source,
        dest,
        skip_accelerator,
        depth,
        pool::workers(),
        None,
        None,
    )
    .map(drop)
    .map_err(String::from)
}

/// What a copy made: every directory in creation order, then every file.
struct Copied {
    dirs: Vec<PathBuf>,
    files: Vec<(PathBuf, PathBuf)>,
}

/// Create every directory first, in order, then copy the files on `workers` threads. The
/// workers are joined before this returns, so a caller may remove `dest` after an error.
fn copy_tree_with(
    source: &Path,
    dest: &Path,
    skip_accelerator: bool,
    depth: usize,
    workers: usize,
    stop: Option<&Cancellation>,
    on_done: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<Copied, pool::Error> {
    let mut copied = Copied {
        dirs: Vec::new(),
        files: Vec::new(),
    };
    lay_out(source, dest, skip_accelerator, depth, stop, &mut copied)?;
    pool::run(
        &copied.files,
        workers,
        stop,
        on_done,
        || (),
        |_, (src, dst)| {
            let meta = fs::symlink_metadata(src).map_err(|e| file_error(src, e))?;
            if !plain_file(&meta) || !meta.is_file() {
                return Err(setup_error(src).into());
            }
            preparer::clone_or_copy(src, dst).map_err(|e| file_error(dst, e))
        },
    )?;
    Ok(copied)
}

fn lay_out(
    source: &Path,
    dest: &Path,
    skip_accelerator: bool,
    depth: usize,
    stop: Option<&Cancellation>,
    copied: &mut Copied,
) -> Result<(), pool::Error> {
    if depth > 64 {
        return Err(setup_error(source).into());
    }
    if let Some(stop) = stop {
        stop.check()?;
    }
    require_dir(source)?;
    fs::create_dir(dest).map_err(|e| file_error(dest, e))?;
    copied.dirs.push(dest.to_owned());
    for entry in fs::read_dir(source).map_err(|e| file_error(source, e))? {
        let entry = entry.map_err(|e| file_error(source, e))?;
        let name = entry.file_name();
        if skip_accelerator && generated_cache(&name) {
            continue;
        }
        let src = entry.path();
        let dst = dest.join(name);
        let meta = fs::symlink_metadata(&src).map_err(|e| file_error(&src, e))?;
        if !plain_file(&meta) {
            return Err(setup_error(&src).into());
        }
        if meta.is_dir() {
            lay_out(&src, &dst, false, depth + 1, stop, copied)?;
        } else if meta.is_file() {
            copied.files.push((src, dst));
        } else {
            return Err(setup_error(&src).into());
        }
    }
    Ok(())
}

/// The flush guarantee of `durable::flush_tree`, as its own pass over the copy: every file
/// first, then every directory, all before the snapshot is renamed into place.
fn flush_copied(
    copied: &Copied,
    workers: usize,
    stop: Option<&Cancellation>,
    on_done: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<(), pool::Error> {
    pool::run(
        &copied.files,
        workers,
        stop,
        on_done,
        || (),
        |_, (_, dst)| durable::flush_tree(dst).map_err(|e| file_error(dst, e)),
    )?;
    pool::run(
        &copied.dirs,
        workers,
        stop,
        || (),
        |_, dir| durable::flush_directory(dir).map_err(|e| file_error(dir, e)),
    )
    .map(drop)
}

fn file_error(path: &Path, error: io::Error) -> pool::Error {
    pool::Error::File(path.to_owned(), error)
}

fn setup_error(path: &Path) -> String {
    format!("{}: {}", path.display(), unsettled!())
}
fn at(path: &Path, error: io::Error) -> String {
    format!("{}: {error}", path.display())
}

#[cfg(test)]
mod tests;
