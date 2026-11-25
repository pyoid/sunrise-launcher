//! The kept build folder of one game build: SDK inputs and the writable paths of a native compile.
use super::{
    attempt_owner,
    links::{self, link_or_copy, Placed},
    sdk_cache::{listing, verify_slice, Package},
    tool, Path,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, PathBuf},
};
use sunrise_launcher_smod::{
    index::{library_identity::LibraryIdentity, sha256, sha256_file},
    manifest::Manifest,
};

/// A folder set aside: this prefix and 6 random characters. Short, because Windows' path limit
/// counts every folder above the deepest file the SDK's build writes (#396).
pub(crate) const ATTEMPT_PREFIX: &str = ".nb-";
/// The folder launchers before 1.3.1 named attempts with, still found and counted on disk.
pub(crate) const OLD_ATTEMPT_PREFIX: &str = ".native-attempt-";
/// The SDK's work folder inside the kept folder.
const WORK: &str = "w";
/// The kept folders, under the native folder; one per game build and what built it.
const KEPT: &str = "kept";
/// Written into the kept folder while a compiler's tree isn't proven gone.
const WORKER: &str = ".worker";
/// Changes when what a kept folder holds would no longer fit the launcher that wrote it.
const KEY_VERSION: &str = "1";
/// A kept folder's name: 12 characters of the game's revision, a dash and 6 of the hash. Short,
/// as the tool's build folder sits below it and Windows' path limit counts every character.
const KEY_LENGTH: usize = 19;
/// The deepest file a Windows build writes under the work folder, as #396 found it: rustc's
/// import definition for a raw-dylib DLL, in Cargo's target folder under the SDK's build key.
const DEEPEST: &str = r"prebuilt-build\94256510f74e-f618d2e4182eb19e\target\x86_64-pc-windows-gnu\release\deps\rustcgsxNAs\api-ms-win-core-backgroundtask-l1-1-0.dll_imports_indirect.def";
/// Windows' `MAX_PATH`, its terminating NUL included, which mingw's `dlltool` doesn't get around.
const MAX_PATH: usize = 260;

/// How long the deepest path of a build in `native`'s kept folder would be, when it would pass
/// Windows' limit.
pub(super) fn too_deep(native: &Path) -> Option<usize> {
    let kept = native.as_os_str().len() + 1 + KEPT.len() + 1 + KEY_LENGTH;
    let length = kept + 1 + WORK.len() + 1 + DEEPEST.len();
    (length >= MAX_PATH).then_some(length)
}

/// What decides whether two builds may share a kept folder.
pub(super) struct KeyParts<'a> {
    pub(super) revision: &'a str,
    /// The signed index's digest, empty for a developer's own SDK.
    pub(super) signed_sdk: &'a str,
    pub(super) route: &'a str,
    pub(super) target: &'a str,
    pub(super) platform: &'a str,
    pub(super) rust_target: &'a str,
    pub(super) mixins_from_source: bool,
}

/// A kept folder's name. The tool's own key inside it (`kept_build`) carries the slice files and
/// the tool's commit, so the tool is no part of this one.
pub(super) fn key(parts: &KeyParts<'_>) -> String {
    let hashed = [
        KEY_VERSION,
        parts.signed_sdk,
        parts.route,
        parts.target,
        parts.platform,
        parts.rust_target,
        crate::rust_toolchain::VERSION,
        if parts.mixins_from_source {
            "source"
        } else {
            "release"
        },
    ]
    .join("\0");
    let revision = parts.revision.get(..12).unwrap_or(parts.revision);
    format!("{revision}-{}", &sha256(hashed.as_bytes())[..6])
}

/// The kept folder of one game build, held under its lock for as long as this lives.
pub(super) struct Workspace {
    dir: PathBuf,
    _lock: crate::native_cache::Locked,
    downloads: downloads::Downloads,
}

/// What [`Workspace::open`] found.
#[derive(Debug, PartialEq)]
pub(super) enum Opened {
    Fresh,
    Reused,
    /// The last build here couldn't prove its workers stopped: its files are in this folder
    /// and this build starts clean.
    SetAside(PathBuf),
}

/// One SDK library, with the exact basename it is installed under and the verified
/// bytes. A registry symlink alias is copied as a regular file under its original name.
#[derive(Debug)]
pub(super) struct Library {
    pub(super) name: String,
    pub(super) source: std::path::PathBuf,
    pub(super) sha256: String,
    /// The source is a file no build rewrites (the SDK package's runtime, the standard library,
    /// the MinGW runtime), so an install links it instead of copying it.
    pub(super) shared: bool,
}

impl Library {
    pub(super) fn copy_into(&self, destination: &Path) -> io::Result<()> {
        let output = destination.join(&self.name);
        if self.shared {
            links::link_or_copy(&self.source, &output)?;
        } else {
            // Unlinked first: a carried file of this name may be a link into the base.
            match fs::remove_file(&output) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            crate::preparer::clone_or_copy(&self.source, &output)?;
        }
        // After a link this reads the source's own bytes, so it still proves them.
        if sha256_file(&output).is_ok_and(|digest| digest == self.sha256) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

impl Workspace {
    /// Opens the kept folder `key` under `native`, once any other build of it is done: this
    /// calls `waiting` once when it has to wait. A folder whose last build never proved its
    /// workers stopped is set aside and the build starts in an empty one.
    pub(super) fn open(
        native: &Path,
        key: &str,
        cancellation: &tool::Cancellation,
        waiting: &dyn Fn(),
    ) -> Result<(Self, Opened), String> {
        let native = crate::backend::plain(native.canonicalize().map_err(|e| e.to_string())?);
        // The lock sits outside the folder: an open handle inside it would block setting it aside.
        let locks = native.join("build-locks");
        fs::create_dir_all(&locks).map_err(|e| e.to_string())?;
        let mut said = false;
        let lock = loop {
            let file = fs::File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(locks.join(format!("{key}.lock")))
                .map_err(|e| e.to_string())?;
            match crate::native_cache::locked(file, crate::native_cache::LockMode::Exclusive) {
                Ok(lock) => break lock,
                Err(crate::native_cache::Error::Busy) => {}
                Err(error) => return Err(super::cache_error(error)),
            }
            cancellation.check()?;
            if !std::mem::replace(&mut said, true) {
                waiting();
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        };
        let dir = native.join(KEPT).join(key);
        let opened = if dir.join(WORKER).exists() {
            let aside = tempfile::Builder::new()
                .prefix(ATTEMPT_PREFIX)
                .rand_bytes(6)
                .tempdir_in(&native)
                .map_err(|e| e.to_string())?
                .keep();
            fs::remove_dir(&aside).map_err(|e| e.to_string())?;
            fs::rename(&dir, &aside).map_err(|e| e.to_string())?;
            // This process just failed to prove the tree gone: Storage must not delete it.
            attempt_owner::claim(&aside).map_err(|e| e.to_string())?;
            Opened::SetAside(aside)
        } else if dir.exists() {
            Opened::Reused
        } else {
            Opened::Fresh
        };
        match fs::remove_dir_all(dir.join("tmp")) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.to_string()),
            _ => {}
        }
        fs::create_dir_all(dir.join("tmp")).map_err(|e| e.to_string())?;
        crate::storage::mark_used(&dir).map_err(super::cache_error)?;
        Ok((
            Self {
                dir,
                _lock: lock,
                downloads: downloads::Downloads::default(),
            },
            opened,
        ))
    }

    pub(super) fn path(&self) -> &Path {
        &self.dir
    }

    /// The folder the SDK builds in.
    pub(super) fn work(&self) -> PathBuf {
        self.path().join(WORK)
    }

    /// Where a profile's mods are staged for the tool, the same path every build: the tool
    /// names its own stage folder after it.
    pub(super) fn mods(&self, profile: &Path) -> PathBuf {
        self.path()
            .join("mods")
            .join(&sha256(profile.as_os_str().as_encoded_bytes())[..12])
    }

    pub(super) fn isolate(&self, command: &mut std::process::Command) -> io::Result<()> {
        let path = self.path();
        for dir in ["cargo", "target", "tmp"] {
            fs::create_dir_all(path.join(dir))?;
        }
        let tmp = path.join("tmp");
        command
            .env("CARGO_HOME", path.join("cargo"))
            .env("CARGO_TARGET_DIR", path.join("target"))
            .env("TMPDIR", &tmp)
            .env("TMP", &tmp)
            .env("TEMP", &tmp);
        if self.offline() {
            // The fetch the tool runs before cargo then only resolves the lock locally.
            command.env("CARGO_NET_OFFLINE", "true");
        }
        Ok(())
    }

    /// From here on an escaped descendant can outlive the launcher or SDK, and can keep writing
    /// into this folder. The next build of it sets it aside unless a proof ([`Self::worker_finished`])
    /// came first.
    pub(super) fn allow_worker(&mut self) -> io::Result<()> {
        fs::write(self.dir.join(WORKER), std::process::id().to_string())
    }

    /// A terminal OS proof, not successful compilation by itself, lets the folder be reused.
    pub(super) fn worker_finished(&mut self) -> io::Result<()> {
        match fs::remove_file(self.dir.join(WORKER)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// A kept folder `key` under `parent`, as a test needs it, with nothing to wait for.
    #[cfg(test)]
    pub(super) fn scratch(parent: &Path, key: &str) -> Self {
        let waiting = tool::Cancellation::default();
        Self::open(parent, key, &waiting, &|| {})
            .expect("a scratch folder opens")
            .0
    }

    /// Lays the SDK's slices (`from`, kept in the shared cache) out in this folder, once: by hard
    /// links where the file system allows, else copies, and read-only, so a build script can't
    /// change a cached file by accident. Each later build only checks every file's size and
    /// modified time against what was laid out, and lays the package out again, with a line to
    /// `log`, when one differs. A developer's own SDK (no `signed_sdk`) is laid out each time.
    pub(super) fn lay_out_package(
        &self,
        from: &Package,
        signed_sdk: Option<&str>,
        log: &dyn Fn(&str),
    ) -> io::Result<Package> {
        let root = self.dir.join("package");
        let private = Package::at(&root);
        let stamp = self.dir.join("package.stamp");
        let laid = fs::read(&stamp)
            .ok()
            .and_then(|text| serde_json::from_slice::<Laid>(&text).ok());
        if let (Some(laid), Some(source)) = (&laid, signed_sdk) {
            if laid.source == source {
                if listing(&root).is_some_and(|now| now == laid.files) {
                    return Ok(private);
                }
                log("The SDK files in this build folder changed: laying them out again");
            }
        }
        // Never current while it is being laid out.
        match fs::remove_file(&stamp) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        match fs::remove_dir_all(&root) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        fs::create_dir(&root)?;
        for (from, to) in [
            (&from.runtime, &private.runtime),
            (&from.api, &private.api),
            (&from.engine, &private.engine),
        ] {
            match fs::symlink_metadata(from) {
                Ok(meta) if meta.is_dir() && !crate::native_cache::linked(&meta) => {
                    // The full check, against the sums the SDK cache recorded when it
                    // unpacked what the signed index lists: before a link makes it the build's.
                    verify_slice(from)?;
                    place_slice(from, to)?;
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
                Ok(_) => return Err(invalid()),
            }
        }
        if let Some(source) = signed_sdk {
            let laid = Laid {
                source: source.to_owned(),
                files: listing(&root).ok_or_else(invalid)?,
            };
            fs::write(
                &stamp,
                serde_json::to_vec(&laid).expect("a stamp serializes"),
            )?;
        }
        Ok(private)
    }

    pub(super) fn import_log(&self) -> io::Result<String> {
        let work = self.work();
        let log = work.join("build.log");
        match regular_under(self.path(), &log) {
            Ok(()) => Ok(tool::tail(fs::File::open(log)?)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(err) => Err(err),
        }
    }

    /// Bind the exact SDK path and bytes of every published library, including ordinary unstamped
    /// dependencies. Only the registry route can add Cargo output or Cargo-home libraries.
    pub(super) fn verify_outputs<'a>(
        &self,
        built: &tool::Built,
        package: &Package,
        rust: &crate::rust_toolchain::Toolchain,
        expected_route: Option<tool::CheckRoute>,
        revision: &str,
        mods: impl Iterator<Item = &'a Manifest>,
    ) -> io::Result<Vec<Library>> {
        let work = self.work();
        if expected_route.is_some_and(|route| route != built.route) {
            return Err(invalid());
        }
        // Both SDK runtime slices use this filename; installation renames the server image.
        let binary = crate::backend::plain(built.binary.clone());
        let runtime_binary = binary == package.runtime.join("bin").join(crate::backend::CLIENT)
            && regular_under(self.path(), &binary).is_ok();
        match built.route {
            tool::CheckRoute::PrebuiltApi if !runtime_binary => return Err(invalid()),
            tool::CheckRoute::Prebuilt
                if !runtime_binary
                    && (!binary.starts_with(&work)
                        || regular_under(self.path(), &binary).is_err()) =>
            {
                return Err(invalid())
            }
            _ => (),
        }
        let wanted: BTreeMap<_, _> = mods.map(|m| (m.id.as_str(), m.version.as_str())).collect();
        let used: BTreeSet<_> = built.mods.iter().map(String::as_str).collect();
        if used.len() != built.mods.len()
            || !used.iter().copied().eq(wanted.keys().copied())
            || built.libraries.is_empty()
        {
            return Err(invalid());
        }
        let std_lib = rust.target_libdir();
        let mingw = rust.mingw_bin();
        // Registry compilation returns the merged package's executable and places the
        // stamped patch in <target>/<triple>/release. The latter binds the one allowed
        // Cargo target root without recalculating the SDK's private build key.
        let whole = work.join("prebuilt-package");
        let registry_target = if built.route == tool::CheckRoute::Prebuilt
            && binary == whole.join("bin").join(crate::backend::CLIENT)
        {
            let patch = built
                .records
                .iter()
                .find(|row| matches!(row.kind, tool::LibraryKind::Patch))
                .ok_or_else(invalid)?;
            let patch = built
                .libraries
                .iter()
                .find(|file| {
                    file.file_name()
                        .is_some_and(|name| name == patch.file.as_str())
                })
                .ok_or_else(invalid)?;
            let patch = crate::backend::plain(patch.clone());
            let builds = work.join("prebuilt-build");
            let release = patch
                .strip_prefix(&builds)
                .ok()
                .and_then(Path::parent)
                .ok_or_else(invalid)?;
            let folders: Vec<_> = release.components().collect();
            match folders.as_slice() {
                [Component::Normal(key), Component::Normal(target), Component::Normal(triple), Component::Normal(release)]
                    if *target == "target"
                        && *triple == rust.host.target
                        && *release == "release" =>
                {
                    Some(builds.join(key).join("target"))
                }
                _ => return Err(invalid()),
            }
        } else {
            None
        };
        let cargo_home = self.path().join("cargo");
        let joined_runtime = whole.join("deps");
        let stamped_names: BTreeSet<_> =
            built.records.iter().map(|row| row.file.as_str()).collect();
        let mut files = BTreeMap::new();
        for supplied in &built.libraries {
            // SDK directory scans can return canonical-prefixed Windows paths. The plan
            // otherwise uses the launcher's plain form; this does not resolve any links.
            let file = crate::backend::plain(supplied.clone());
            let name = file
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(crate::browser::safe_name)
                .ok_or_else(invalid)?;
            let dynamic = [".so", ".dll", ".dylib"]
                .iter()
                .any(|suffix| name.ends_with(suffix));
            let versioned_so = name
                .rsplit_once(".so.")
                .is_some_and(|(base, version)| !base.is_empty() && !version.is_empty());
            let stamped = stamped_names.contains(name);
            let dependency = !stamped && registry_target.is_some();
            if !dynamic && !(dependency && versioned_so) {
                return Err(invalid());
            }
            let runtime =
                || file.starts_with(&package.runtime) && regular_under(self.path(), &file).is_ok();
            let standard = || {
                (name.starts_with("libstd-") || name.starts_with("std-"))
                    && std_lib.as_ref().is_some_and(|dir| {
                        file == dir.join(name) && regular_under(dir, &file).is_ok()
                    })
            };
            let gnu = || {
                matches!(name, "libgcc_s_seh-1.dll" | "libwinpthread-1.dll")
                    && file == mingw.join(name)
                    && regular_under(&mingw, &file).is_ok()
            };
            let produced = (stamped && file.starts_with(&work))
                || (!stamped
                    && built.route == tool::CheckRoute::Prebuilt
                    && file.starts_with(&joined_runtime));
            // When true, the first branch below is the one taken.
            let shared = dynamic && !produced && (runtime() || standard() || gnu());
            let source = if dynamic
                && ((produced && regular_under(self.path(), &file).is_ok())
                    || runtime()
                    || standard()
                    || gnu())
            {
                file.clone()
            } else if let Some(target) = registry_target.as_ref().filter(|_| dependency) {
                registry_library(self.path(), [target, &cargo_home], &file)?
            } else {
                return Err(invalid());
            };
            let sha256 = sha256_file(&source).map_err(io::Error::other)?;
            let library = Library {
                name: name.to_owned(),
                source,
                sha256,
                shared,
            };
            if files.insert(name.to_ascii_lowercase(), library).is_some() {
                return Err(invalid());
            }
        }
        let mut stamped = BTreeSet::new();
        let mut recorded = BTreeSet::new();
        let mut patch = false;
        // Each library row's identity, by mod id, and the native mods each one says it links.
        let mut identities = BTreeMap::new();
        let mut linked = Vec::new();
        for row in &built.records {
            let Some(file) = files
                .get(&row.file.to_ascii_lowercase())
                .filter(|file| file.name == row.file)
            else {
                return Err(invalid());
            };
            let id = row.mod_id.as_str();
            let version = match row.kind {
                tool::LibraryKind::Mod if recorded.insert(id) => {
                    wanted.get(id).copied().ok_or_else(invalid)?
                }
                tool::LibraryKind::Patch
                    if built.route == tool::CheckRoute::Prebuilt
                        && id == "sunrise_patch"
                        && !patch =>
                {
                    patch = true;
                    "0"
                }
                _ => return Err(invalid()),
            };
            let rust = crate::rust_toolchain::VERSION;
            // Since sunrise#5136 the identity also names the mod's files and the identity of each
            // native mod the library links; SDKs from before it stamp only the five fields.
            let identity_matches = match LibraryIdentity::parse(&row.identity) {
                Some(identity) => {
                    linked.push(identity.dependencies().ok_or_else(invalid)?);
                    identity.build == revision
                        && identity.id == id
                        && identity.version == version
                        && identity.rust == rust
                        && identity.panic == "unwind"
                }
                None => row.identity == format!("{revision} {id} {version} {rust} unwind"),
            };
            if !stamped.insert(&row.file) || !identity_matches || file.sha256 != row.sha256 {
                return Err(invalid());
            }
            if matches!(row.kind, tool::LibraryKind::Mod) {
                identities.insert(id, row.identity.as_str());
            }
        }
        if recorded != used {
            return Err(invalid());
        }
        // A library links only mods of this build, each as the identity this build recorded for it.
        for (id, digest) in linked.into_iter().flatten() {
            if identities
                .get(id)
                .is_none_or(|identity| sha256(identity.as_bytes()) != digest)
            {
                return Err(invalid());
            }
        }
        Ok(files.into_values().collect())
    }
}

/// What [`Workspace::lay_out_package`] laid out, and from which signed SDK.
#[derive(serde::Serialize, serde::Deserialize)]
struct Laid {
    source: String,
    files: BTreeMap<String, (u64, u64)>,
}

/// Places the files of the slice `from` at `to`, linked or copied, each read-only. The tool names
/// its build folder from each slice file's path, length and modified time (`kept_build`): a
/// link keeps the time, and a copy gets it set back, or no kept library would be found again.
fn place_slice(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if crate::native_cache::linked(&meta) {
            return Err(invalid());
        }
        let to = to.join(entry.file_name());
        if meta.is_dir() {
            place_slice(&entry.path(), &to)?;
        } else if meta.is_file() {
            if link_or_copy(&entry.path(), &to)? == Placed::Copied {
                times_handle(&to)?.set_modified(meta.modified()?)?;
            }
            let mut permissions = fs::metadata(&to)?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&to, permissions)?;
        } else {
            return Err(invalid());
        }
    }
    Ok(())
}

/// A handle that may set `file`'s times even when the file is read-only: on Windows it asks
/// for attribute writes only, elsewhere setting times needs ownership, not write permission.
fn times_handle(file: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES
        fs::File::options().access_mode(0x100).open(file)
    }
    #[cfg(not(windows))]
    {
        fs::File::open(file)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native SDK output")
}

// The SDK preserves linked library basenames (e.g. libx.so -> libx.so.1). Only a
// final file symlink within the registry's exact roots is allowed. Keep the resolved
// checked source, not the symlink, and verify the published copy's digest again.
fn registry_library(
    workspace: &Path,
    roots: [&Path; 2],
    path: &Path,
) -> io::Result<std::path::PathBuf> {
    let allowed = |file: &Path| roots.iter().any(|root| file.starts_with(root));
    if !allowed(path) {
        return Err(invalid());
    }
    checked_under(workspace, path.parent().ok_or_else(invalid)?, true)?;
    let source = if fs::symlink_metadata(path)?.file_type().is_symlink() {
        crate::backend::plain(path.canonicalize()?)
    } else {
        path.to_owned()
    };
    if !allowed(&source) {
        return Err(invalid());
    }
    regular_under(workspace, &source)?;
    Ok(source)
}

// Refuse reparse points and links at every component, including the root. The compiler never
// receives the installation destination; the launcher copies these bytes out of the work tree.
fn regular_under(root: &Path, file: &Path) -> io::Result<()> {
    checked_under(root, file, false)
}

fn checked_under(root: &Path, file: &Path, directory: bool) -> io::Result<()> {
    let tail = file.strip_prefix(root).map_err(|_| invalid())?;
    let mut path = root.to_path_buf();
    let mut parts = tail.components().peekable();
    if parts.peek().is_none() {
        return Err(invalid());
    }
    let meta = fs::symlink_metadata(root)?;
    if !meta.is_dir() || crate::native_cache::linked(&meta) {
        return Err(invalid());
    }
    while let Some(std::path::Component::Normal(part)) = parts.next() {
        path.push(part);
        let meta = fs::symlink_metadata(&path)?;
        if crate::native_cache::linked(&meta) {
            return Err(invalid());
        }
        if parts.peek().is_none() {
            return if (directory && meta.is_dir()) || (!directory && meta.is_file()) {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        if !meta.is_dir() {
            return Err(invalid());
        }
    }
    Err(invalid())
}

mod downloads;

#[cfg(test)]
mod tests;
