//! What a stamp says about an unpacked slice: the index that listed its archive, the archive, and
//! every file it unpacked to with its size, modified time and SHA-256. The unpack checked the
//! archive against the signed index before the stamp was written, so the stamp's sums are the
//! signed index's, carried to each file.
use super::{write_whole, Refusal, Sdk, STAMP_SUFFIX};
use crate::i18n::{tr, trf};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use sunrise_launcher_smod::index::sha256_file;

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct Stamp {
    index: String,
    asset: String,
    files: BTreeMap<String, u64>,
    /// Modified times in nanoseconds since the epoch. A stamp from before they were recorded
    /// has none, and holds on sizes alone until [`record`] fills them in.
    #[serde(default)]
    times: BTreeMap<String, u64>,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
}

pub(super) fn stamp_path(into: &Path) -> PathBuf {
    let mut path = into.as_os_str().to_owned();
    path.push(STAMP_SUFFIX);
    PathBuf::from(path)
}

/// Every file under `dir`, `/`-separated relative to it, with its size and its modified time in
/// nanoseconds; `None` when it can't be read or holds anything but files and folders.
pub(in crate::native_mods) fn listing(dir: &Path) -> Option<BTreeMap<String, (u64, u64)>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (u64, u64)>) -> Option<()> {
        for entry in fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else if kind.is_file() {
                let relative = path.strip_prefix(root).ok()?;
                let name = relative
                    .components()
                    .map(|part| part.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()?
                    .join("/");
                let meta = entry.metadata().ok()?;
                let time = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
                out.insert(name, (meta.len(), u64::try_from(time.as_nanos()).ok()?));
            } else {
                return None;
            }
        }
        Some(())
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out)?;
    Some(out)
}

fn read(into: &Path) -> Option<Stamp> {
    serde_json::from_slice(&fs::read(stamp_path(into)).ok()?).ok()
}

/// Whether `into` is `name` as `sdk`'s index lists it: its stamp names this index and this
/// archive, and the folder holds exactly the files the stamp lists, each at its size and
/// modified time, so a file edited in place changes one of them. Nothing is hashed, except a
/// file whose time differs but whose size is the same: it holds only when its bytes are the
/// stamp's, and the stamp then takes the new time. A stamp with no times holds on sizes.
pub(super) fn stamp_holds(sdk: &Sdk, name: &str, into: &Path) -> bool {
    let Some(mut stamp) = read(into) else {
        return false;
    };
    let Some(now) = listing(into) else {
        return false;
    };
    let same_files = now.len() == stamp.files.len()
        && now
            .iter()
            .all(|(file, (size, _))| stamp.files.get(file) == Some(size));
    if stamp.index != sdk.sha256 || stamp.asset != name || !same_files {
        return false;
    }
    if stamp.times.is_empty() {
        return true;
    }
    let touched: Vec<_> = now
        .iter()
        .filter(|(file, (_, time))| stamp.times.get(*file) != Some(time))
        .collect();
    for (file, (_, time)) in &touched {
        let unchanged = stamp
            .hashes
            .get(*file)
            .is_some_and(|hash| sha256_file(&into.join(file)).is_ok_and(|have| have == *hash));
        if !unchanged {
            return false;
        }
        stamp.times.insert((*file).clone(), *time);
    }
    touched.is_empty() || write_stamp(into, &stamp).is_ok()
}

fn write_stamp(into: &Path, stamp: &Stamp) -> Result<(), Refusal> {
    write_whole(
        &stamp_path(into),
        &serde_json::to_vec(stamp).expect("a stamp serializes"),
    )
}

/// Stamps `into`, just unpacked from the archive `name` the index lists: every file's size,
/// time and SHA-256.
pub(super) fn record(sdk: &Sdk, name: &str, into: &Path) -> Result<(), Refusal> {
    let listed = listing(into).ok_or_else(|| {
        Refusal::Cache(trf(
            "{folder} holds something other than files and folders",
            &[("folder", &into.display().to_string())],
        ))
    })?;
    let mut stamp = Stamp {
        index: sdk.sha256.clone(),
        asset: name.to_owned(),
        files: BTreeMap::new(),
        times: BTreeMap::new(),
        hashes: BTreeMap::new(),
    };
    for (file, (size, time)) in listed {
        let hash = sha256_file(&into.join(&file)).map_err(Refusal::Cache)?;
        stamp.files.insert(file.clone(), size);
        stamp.times.insert(file.clone(), time);
        stamp.hashes.insert(file, hash);
    }
    write_stamp(into, &stamp)
}

/// A kept slice whose stamp predates the times and sums gets them recorded, once, from the
/// files the stamp already trusts by size.
pub(super) fn upgrade(sdk: &Sdk, name: &str, into: &Path) -> Result<(), Refusal> {
    if read(into).is_some_and(|stamp| stamp.times.is_empty() || stamp.hashes.is_empty()) {
        let _ = super::super::append_log("Recording checksums for the SDK files");
        record(sdk, name, into)?;
    }
    Ok(())
}

/// Checks each file of the slice `dir` against the SHA-256 its stamp recorded. A slice with no
/// stamp, or an old one without sums (a developer's own), has nothing to check against.
pub(in crate::native_mods) fn verify_slice(dir: &Path) -> io::Result<()> {
    let Some(stamp) = read(dir) else {
        return Ok(());
    };
    for (file, hash) in &stamp.hashes {
        if sha256_file(&dir.join(file)).map_err(io::Error::other)? != *hash {
            return Err(io::Error::other(tr(
                "The SDK files in the cache no longer match their checksums. Clear the build files on the Storage page, then build again.",
            )));
        }
    }
    Ok(())
}
