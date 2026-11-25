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
    match fs::remove_file(to) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
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
    fs::set_permissions(to, meta.permissions())?;
    Ok(Placed::Copied)
}

#[cfg(test)]
mod tests;
