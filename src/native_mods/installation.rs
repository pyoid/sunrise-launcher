//! Publish native installations before exposing their completion marker.
use super::*;
use std::io;

#[allow(clippy::too_many_arguments)]
pub(super) fn install(
    base: &str,
    version: &str,
    target: Target,
    binary: &Path,
    libraries: &[workspace::Library],
    about: &serde_json::Value,
    mods: &[Bundle],
    cancellation: &Cancellation,
) -> Result<(), String> {
    let _publication = crate::run_leases::installation().map_err(cache_error)?;
    let from = backend::version_dir(base);
    let dir = target.directory(version);
    let parent = dir.parent().expect("build cache parent");
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new()
        .prefix(".native-publish-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let partial = stage.path().join("build");
    fs::create_dir(&partial).map_err(|e| e.to_string())?;
    copy_base(&from, &partial, target, target == Target::Client)?;
    let installed = partial.join(target.binary());
    // The built game is always a new file, never a link: the kept build folder it came from may
    // be rewritten in place by the next build. A file the base carried under this name is
    // unlinked first, so the copy never writes through a link into the base.
    match fs::remove_file(&installed) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(format!("The built game: {e}"))
        }
        _ => {}
    }
    crate::preparer::clone_or_copy(binary, &installed)
        .and_then(|()| fs::set_permissions(&installed, fs::metadata(binary)?.permissions()))
        .map_err(|e| format!("The built game: {e}"))?;
    // The prebuilt game's libraries load from beside the executable. Those from a shared,
    // read-only package are linked.
    for library in libraries {
        library.copy_into(&partial).map_err(|error| {
            let _ = append_log(&error.to_string());
            tr("A verified native library couldn't be copied. The build wasn't installed.")
                .to_owned()
        })?;
    }
    fs::write(partial.join("native-mods.json"), about.to_string()).map_err(|e| e.to_string())?;
    for bundle in mods {
        copy_bundle(
            bundle,
            &partial
                .join(sunrise_launcher_smod::mod_list::NATIVE_DIR)
                .join(&bundle.manifest.id),
        )?;
    }
    cancellation.check()?;
    // A completed tag may already be journaled by another profile. Publish only
    // a new directory; a failed proof or occupied destination never changes it.
    crate::run_leases::publish_build(&partial, &dir).map_err(cache_error)
}

/// Whether `name`, a top-level entry of a version folder, may change in place after an install
/// (the game and the preparer rewrite these), so a build copies it instead of linking it. The
/// same names `run_leases::publish_at` treats as mutable: a new one there must be added here.
pub(super) fn mutable_in_place(name: &std::ffi::OsStr) -> bool {
    [
        ".local",
        "resources",
        ".sunrise-cache",
        "cache",
        "logs",
        "crash-reports",
    ]
    .iter()
    .any(|mutable| name == *mutable)
}

/// Copies folder `from` to `to` with every regular file a hard link where the filesystem allows
/// (a copy otherwise); anything else, such as a symlink, is copied as `copy_dir` does.
pub(super) fn link_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            links::link_or_copy(&entry.path(), &target)?;
        } else {
            crate::preparer::clone_or_copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}
