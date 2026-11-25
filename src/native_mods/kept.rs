//! Mod libraries kept between native builds, so a one-mod change compiles one mod.
//!
//! The SDK tool already reuses a mod's library while its inputs are unchanged: in its build
//! folder (`prebuilt-build/api-<key>` or `rustc-<key>`) each library sits beside a `.fingerprint`
//! of the mod's source, entry and linked libraries. Every attempt runs in a fresh private
//! workspace ([`super::workspace`]); before the kept build folder that folder always started empty
//! and every mod compiled again. Tool contract 2 has no way to hand it a library, so this is
//! done from the launcher.
//!
//! - Only the launcher writes the store, after [`super::workspace::Workspace::verify_outputs`],
//!   and only for a mod library whose bytes it verified there (`Library`). With it go the two
//!   files the tool needs to see it as up to date: its fingerprint and, on Windows, its import
//!   library. The whole-set patch library is never kept. A registry-route build's own outputs
//!   are not copied into this store, because the kept build folder (`native/k/<key>`,
//!   shared by every profile on a game build) keeps them between builds; this supersedes the
//!   earlier rule that nothing of a registry-route build is kept. `verify_outputs` still checks
//!   every library before it is installed.
//! - The store is outside every workspace. A build gets fresh copies, never a path into it or a
//!   link, and each copy's SHA-256 is checked as it is made. Anything missing, changed or linked
//!   is a miss: that mod compiles as before.
//! - A mod's key is everything that decides its library: the SDK (its signed index, which names
//!   every slice's digest, and the tool program), the route, the toolchain, what `version_for`
//!   hashes for the build except the other mods, the mod's own files, and the keys of the mods it
//!   links to. The tool checks its fingerprint again on top.
//! - A key can't tell two builds of the same inputs apart, so an entry also records the library
//!   and build identity of each mod it linked against. It is reused only beside exactly those:
//!   once a library it links to was rebuilt, the mod is rebuilt too.
use super::workspace::Library;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use sunrise_launcher_smod::index::sha256_file;

/// The store's folder under the native build folder.
pub(super) const FOLDER: &str = "kept-mods";

/// Bumped when what an entry holds, or how it is used, changes.
const FORMAT: &str = "kept-mods-v3";

/// How many mod libraries are kept; the least recently used go first.
const ENTRIES: usize = 64;

/// A blob no entry names is removed only once it is this old: another attempt may have stored
/// it and not written its index yet.
const GRACE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What every mod's key shares: the build a library is made for, apart from the other mods.
pub(super) struct Build<'a> {
    /// The SHA-256 of the SDK's signed `sdk.json`, which names each slice's digest.
    pub sdk: &'a str,
    /// The SDK tool program's SHA-256.
    pub tool: &'a str,
    pub route: &'a str,
    pub revision: &'a str,
    pub mixins: &'a str,
    pub staging: &'a str,
    pub profile: &'a str,
    pub rust: &'a str,
    pub rust_target: &'a str,
    pub platform: &'a str,
    pub target: &'a str,
    pub mixins_from_source: bool,
}

/// One mod as its key sees it.
pub(super) struct Mod<'a> {
    pub id: &'a str,
    /// The digest of the mod's own files and its uses table.
    pub files: String,
    /// The mods in this build it links to.
    pub links: Vec<&'a str>,
}

fn hash_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in parts {
        // Length-prefixed: text moved from one part to the next is another key.
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    format!("{:x}", hash.finalize())
}

impl Build<'_> {
    fn key(&self) -> String {
        hash_parts([
            FORMAT,
            self.sdk,
            self.tool,
            self.route,
            self.revision,
            self.mixins,
            self.staging,
            self.profile,
            self.rust,
            self.rust_target,
            self.platform,
            self.target,
            if self.mixins_from_source {
                "source"
            } else {
                "pinned"
            },
        ])
    }
}

/// Each mod's key, given in dependency order: a mod's key covers the keys of the mods it links
/// to, so a changed mod changes the key of every mod linking to it. A mod linking to one without
/// a key (one that comes later) has none: it is neither reused nor kept.
pub(super) fn keys(build: &Build<'_>, mods: &[Mod<'_>]) -> BTreeMap<String, String> {
    let base = build.key();
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    for m in mods {
        let mut links: Vec<_> = m.links.clone();
        links.sort_unstable();
        links.dedup();
        let mut parts = vec![base.as_str(), m.id, m.files.as_str()];
        let Some(linked) = links
            .iter()
            .map(|id| keys.get(*id).map(|key| format!("{id}={key}")))
            .collect::<Option<Vec<String>>>()
        else {
            continue;
        };
        parts.extend(linked.iter().map(String::as_str));
        keys.insert(m.id.to_owned(), hash_parts(parts));
    }
    keys
}

/// A mod library as a mod linking to it was built against.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Linked {
    /// The library's SHA-256, as the launcher verified it.
    sha256: String,
    /// The build identity the tool recorded for it.
    identity: String,
}

/// An entry: the tool's build folder, the library and the tool's files beside it.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    format: String,
    folder: String,
    /// The library's file name, one of `files`.
    library: String,
    /// Its build identity, for the mods linking to it.
    identity: String,
    /// File name to SHA-256: the library first checked by the launcher, then the tool's files.
    files: BTreeMap<String, String>,
    /// Each mod it links to, as this library was built against it.
    links: BTreeMap<String, Linked>,
}

pub(super) struct Kept {
    store: PathBuf,
    grace: std::time::Duration,
}

impl Kept {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            store: root.join(FOLDER),
            grace: GRACE,
        }
    }

    fn index_path(&self, key: &str) -> PathBuf {
        self.store.join(format!("{key}.json"))
    }

    fn blobs(&self) -> PathBuf {
        self.store.join("blobs")
    }

    /// Copies the kept library of each mod in `keys` into the attempt's `work` folder, where the
    /// tool looks for it. Returns the mods put there, in `mods`' order. A mod whose entry is
    /// missing or doesn't verify is left out whole, and so is every mod linking to it, or whose
    /// entry was built against another library of a mod it links to.
    pub(super) fn seed(
        &self,
        work: &Path,
        mods: &[Mod<'_>],
        keys: &BTreeMap<String, String>,
    ) -> Vec<String> {
        let mut seeded: BTreeMap<&str, Linked> = BTreeMap::new();
        let mut order = Vec::new();
        for m in mods {
            let Some(links) = m
                .links
                .iter()
                .map(|id| Some((id.to_string(), seeded.get(id)?.clone())))
                .collect::<Option<BTreeMap<_, _>>>()
            else {
                continue;
            };
            let Some(key) = keys.get(m.id) else {
                continue;
            };
            if let Some(linked) = self.seed_one(work, key, &links) {
                seeded.insert(m.id, linked);
                order.push(m.id.to_owned());
            }
        }
        order
    }

    /// Seeds one entry if it was built against exactly `links`; returns its library as the mods
    /// linking to it see it.
    fn seed_one(&self, work: &Path, key: &str, links: &BTreeMap<String, Linked>) -> Option<Linked> {
        let path = self.index_path(key);
        let Ok(Some(index)) = read_index(&path) else {
            return None;
        };
        if index.links != *links {
            return None;
        }
        let folder = work.join("prebuilt-build").join(&index.folder);
        let mut copied = Vec::new();
        let whole = (|| -> io::Result<()> {
            fs::create_dir_all(&folder)?;
            for (name, sha256) in &index.files {
                let blob = self.blobs().join(sha256);
                let meta = fs::symlink_metadata(&blob)?;
                if !meta.is_file() || crate::native_cache::linked(&meta) {
                    return Err(invalid());
                }
                let to = folder.join(name);
                // Never over a file the attempt has already: kept entries don't overlap. A build
                // folder kept from the last build holds the same bytes already: nothing to copy.
                if fs::symlink_metadata(&to).is_ok() {
                    if sha256_file(&to).is_ok_and(|have| have == *sha256) {
                        continue;
                    }
                    return Err(invalid());
                }
                copied.push(to.clone());
                fs::copy(&blob, &to)?;
                // Checked as copied: the attempt reads exactly these bytes.
                if sha256_file(&to).map_err(io::Error::other)? != *sha256 {
                    return Err(invalid());
                }
            }
            Ok(())
        })();
        if whole.is_err() {
            // A miss: the tool must not see part of an entry.
            for file in copied {
                let _ = fs::remove_file(file);
            }
            return None;
        }
        // Used now: the least recently used entries are the ones pruned.
        let _ = fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()));
        Some(Linked {
            sha256: index.files[&index.library].clone(),
            identity: index.identity,
        })
    }

    /// Keeps each mod library of this build that the launcher verified (`libraries`), under its
    /// mod's key, with the libraries of this build it links to (`mods`). Returns how many were
    /// kept. A library outside the tool's own API or engine build folder (the registry route) is
    /// never kept, nor is the patch library, nor a mod linking to a library not verified here.
    pub(super) fn save(
        &self,
        work: &Path,
        built: &super::tool::Built,
        libraries: &[Library],
        mods: &[Mod<'_>],
        keys: &BTreeMap<String, String>,
    ) -> io::Result<usize> {
        let builds = work.join("prebuilt-build");
        // Each mod's library in this build as the launcher verified it.
        let verified: BTreeMap<&str, (&Library, Linked)> = built
            .records
            .iter()
            .filter(|record| matches!(record.kind, super::tool::LibraryKind::Mod))
            .filter_map(|record| {
                let library = libraries.iter().find(|l| l.name == record.file)?;
                let linked = Linked {
                    sha256: library.sha256.clone(),
                    identity: record.identity.clone(),
                };
                Some((record.mod_id.as_str(), (library, linked)))
            })
            .collect();
        let mut kept = 0;
        for m in mods {
            let Some(key) = keys.get(m.id) else {
                continue;
            };
            let Some((library, own)) = verified.get(m.id) else {
                continue;
            };
            let Some(links) = m
                .links
                .iter()
                .map(|id| Some((id.to_string(), verified.get(id)?.1.clone())))
                .collect::<Option<BTreeMap<_, _>>>()
            else {
                continue;
            };
            let source = crate::backend::plain(library.source.clone());
            let Some(folder) = source.parent() else {
                continue;
            };
            let Some(folder_name) = folder
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| ours(name))
            else {
                continue;
            };
            if folder.parent() != Some(builds.as_path()) {
                continue;
            }
            // Without its fingerprint the tool compiles the mod anyway: nothing to keep. The
            // import library may be absent.
            let regular = |file: &Path| {
                fs::symlink_metadata(file)
                    .is_ok_and(|meta| meta.is_file() && !crate::native_cache::linked(&meta))
            };
            let (fingerprint, import) = sidecars(&library.name);
            if !regular(&folder.join(&fingerprint)) {
                continue;
            }
            // Only the bytes `verify_outputs` checked: hashed before they are stored, and the
            // stored copy hashed again (`store_blob`).
            if sha256_file(&source).map_err(io::Error::other)? != library.sha256 {
                return Err(invalid());
            }
            let mut files = BTreeMap::new();
            files.insert(
                library.name.clone(),
                self.store_blob(&source, Some(&library.sha256))?,
            );
            files.insert(
                fingerprint.clone(),
                self.store_blob(&folder.join(&fingerprint), None)?,
            );
            if let Some(import) = import {
                let file = folder.join(&import);
                match fs::symlink_metadata(&file) {
                    Ok(_) if regular(&file) => {
                        files.insert(import, self.store_blob(&file, None)?);
                    }
                    Ok(_) => return Err(invalid()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            let index = Index {
                format: FORMAT.into(),
                folder: folder_name.to_owned(),
                library: library.name.clone(),
                identity: own.identity.clone(),
                files,
                links,
            };
            // Another attempt replacing the same entry at the same moment can win the rename
            // (Windows refuses a rename over a file being renamed): theirs is as good.
            if self.write_index(key, &index).is_ok() {
                kept += 1;
            }
        }
        self.prune();
        Ok(kept)
    }

    fn write_index(&self, key: &str, index: &Index) -> io::Result<()> {
        let mut staged = tempfile::Builder::new()
            .prefix(".index-")
            .tempfile_in(&self.store)?;
        io::Write::write_all(
            &mut staged,
            &serde_json::to_vec(index).map_err(io::Error::other)?,
        )?;
        // A rename replaces the index whole: a reader sees the old entry or the new one.
        let mut staged = staged;
        for _ in 0..5 {
            match staged.persist(self.index_path(key)) {
                Ok(_) => return Ok(()),
                Err(error) => staged = error.file,
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(invalid())
    }

    /// Stores `file` under its SHA-256, which it returns; with `expected`, only bytes hashing
    /// to it. A blob already there is used only when it is a regular file with those bytes, and
    /// is then dated now, so a concurrent prune keeps it until the index naming it is written.
    /// Anything else there is replaced whole.
    fn store_blob(&self, file: &Path, expected: Option<&str>) -> io::Result<String> {
        let blobs = self.blobs();
        fs::create_dir_all(&blobs)?;
        let staged = tempfile::Builder::new()
            .prefix(".incoming-")
            .tempfile_in(&blobs)?;
        fs::copy(file, staged.path())?;
        let sha256 = sha256_file(staged.path()).map_err(io::Error::other)?;
        if expected.is_some_and(|expected| expected != sha256) {
            return Err(invalid());
        }
        let blob = blobs.join(&sha256);
        // Dated now, not with the copied file's time, for the grace before pruning.
        let now = std::time::SystemTime::now();
        staged.as_file().set_modified(now)?;
        // The staged copy replaces anything else there in one rename (a link itself, never its
        // target). A refused rename fails this save, which costs only the next build's time.
        let intact = fs::symlink_metadata(&blob)
            .is_ok_and(|meta| meta.is_file() && !crate::native_cache::linked(&meta))
            && sha256_file(&blob).is_ok_and(|digest| digest == sha256);
        if intact {
            // On Windows a blob another attempt is copying out refuses this; a prune that then
            // takes it costs only a miss.
            let _ = fs::File::options()
                .write(true)
                .open(&blob)
                .and_then(|existing| existing.set_modified(now));
            return Ok(sha256);
        }
        staged.persist(&blob).map_err(|error| error.error)?;
        Ok(sha256)
    }

    /// Keeps the [`ENTRIES`] most recently used entries and the blobs they name.
    fn prune(&self) {
        let Ok(read) = fs::read_dir(&self.store) else {
            return;
        };
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = read
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name.ends_with(".json")
            })
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (_, old) in entries.drain(ENTRIES.min(entries.len())..) {
            let _ = fs::remove_file(old);
        }
        let mut named = BTreeSet::new();
        for (_, path) in &entries {
            match read_index(path) {
                Ok(Some(index)) => named.extend(index.files.into_values()),
                // An index that can't be read can't be used either: its blobs go with it.
                _ => {
                    let _ = fs::remove_file(path);
                }
            }
        }
        let Ok(blobs) = fs::read_dir(self.blobs()) else {
            return;
        };
        for blob in blobs.flatten() {
            let name = blob.file_name().to_string_lossy().into_owned();
            let young = blob
                .metadata()
                .and_then(|meta| meta.modified())
                .map_or(true, |modified| {
                    modified.elapsed().map_or(true, |age| age < self.grace)
                });
            // A blob being stored by another attempt starts with a dot; a young unnamed one may
            // be waiting for its index.
            if !name.starts_with('.') && !named.contains(&name) && !young {
                let _ = fs::remove_file(blob.path());
            }
        }
    }
}

/// The tool's files beside library `name` that tell it the library is up to date: its
/// fingerprint (`compile_mod`), and on Windows the import library a mod linking to it needs.
fn sidecars(name: &str) -> (String, Option<String>) {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    (
        format!("{stem}.fingerprint"),
        cfg!(windows).then(|| format!("lib{name}.a")),
    )
}

/// The tool's API and engine route build folders; the registry route builds elsewhere.
fn ours(folder: &str) -> bool {
    (folder.starts_with("api-") || folder.starts_with("rustc-"))
        && crate::browser::safe_name(folder) == Some(folder)
}

fn read_index(path: &Path) -> io::Result<Option<Index>> {
    match fs::read(path) {
        Ok(bytes) => parse_index(&bytes).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_index(bytes: &[u8]) -> io::Result<Index> {
    let index: Index = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let names_ok = !index.files.is_empty()
        && index.files.iter().all(|(name, sha256)| {
            crate::browser::safe_name(name) == Some(name.as_str())
                && sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    if index.format != FORMAT
        || !names_ok
        || !index.files.contains_key(&index.library)
        || !ours(&index.folder)
    {
        return Err(invalid());
    }
    Ok(index)
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid kept native build")
}

#[cfg(test)]
mod tests;
