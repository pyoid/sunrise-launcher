//! Putting one file's bytes at a second path: a hard link where the file system allows it.
use std::{fs, io, path::Path};

/// How [`link_or_copy`] placed a file.
#[derive(Debug, PartialEq)]
pub(super) enum Placed {
    Linked,
    Copied,
}

/// Puts `from` at `to` as a new file: a hard link where the file system allows, else a clone or
/// copy. `to` is unlinked first, so a file already there is never written through. A link or a
/// folder in `from` is refused, never followed.
pub(super) fn link_or_copy(from: &Path, to: &Path) -> io::Result<Placed> {
    remove_file(to)?;
    let meta = fs::symlink_metadata(from)?;
    if !meta.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // Any link error (another volume, a file system with no links, too many links) falls back
    // to a copy, and a real fault comes back from the copy.
    if fs::hard_link(from, to).is_ok() {
        return Ok(Placed::Linked);
    }
    crate::preparer::clone_or_copy(from, to)?;
    // The mode carries the executable bits. Windows' permissions are only the read-only flag,
    // which a copy doesn't take: a read-only file there can't be flushed, replaced or removed.
    #[cfg(unix)]
    fs::set_permissions(to, meta.permissions())?;
    Ok(Placed::Copied)
}

/// Removes the file at `path`, if there is one. Windows refuses to remove a read-only file, as
/// a link into the read-only SDK package is: its flag is cleared first, which clears it on the
/// package's file too (the package's layout check looks at sizes and times, not the flag).
pub(super) fn remove_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.is_file() && meta.permissions().readonly() {
            let mut permissions = meta.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
        }
    }
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
