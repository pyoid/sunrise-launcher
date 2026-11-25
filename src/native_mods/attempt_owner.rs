//! Who owns a folder a build set aside: its lock lives as long as the launcher process that
//! made it, so another process can tell a folder in use from a leftover without a process id.
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

/// The lock file in a set-aside folder.
pub(crate) const FILE: &str = ".owner";

/// The build that set `folder` aside calls this once; the lock lives until the process exits,
/// and the OS drops it on a crash too.
pub(crate) fn claim(folder: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(folder.join(FILE))?;
    file.try_lock().map_err(io::Error::from)?;
    // Held until exit.
    std::mem::forget(file);
    Ok(())
}

/// A folder with no `.owner` is from a launcher before 1.3.5, whose owner the build lease
/// already proves gone. With one, its owner is gone only when the lock can be taken.
// Storage's cleanup of leftover folders asks this.
#[allow(dead_code)]
pub(crate) fn gone(folder: &Path) -> bool {
    let file = folder.join(FILE);
    match fs::symlink_metadata(&file) {
        Err(error) => error.kind() == io::ErrorKind::NotFound,
        Ok(meta) if !meta.is_file() => false,
        Ok(_) => File::open(file).is_ok_and(|file| file.try_lock().is_ok()),
    }
}

#[cfg(test)]
mod tests;
