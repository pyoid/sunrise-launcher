//! Leftover native attempt folders (`native/.nb-*`, `native/.native-attempt-*`).
use super::*;
use crate::native_mods::{self, attempt_owner::gone};

impl Storage {
    /// Removes each attempt folder whose owner is proven gone and returns the bytes removed.
    /// Holding the build lease (`self.lease`) proves that no other launcher is in a build; a folder
    /// from this launcher's own unproved build carries an owner lock that keeps it. A folder that
    /// can't be removed stays, and `retained_compiler_bytes` still counts and shows it.
    pub(super) fn remove_leftover_attempts(&self) -> Result<u64, Error> {
        let native = self.root.join("native");
        if checked(&native, true)?.is_none() {
            return Ok(0);
        }
        let native_dir = self.directory.open_dir_nofollow("native")?;
        let mut freed = 0u64;
        for entry in fs::read_dir(&native)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_str().is_some_and(|name| {
                name.starts_with(native_mods::ATTEMPT_PREFIX)
                    || name.starts_with(native_mods::OLD_ATTEMPT_PREFIX)
            }) {
                continue;
            }
            let path = entry.path();
            if !gone(&path) {
                continue;
            }
            #[cfg(target_os = "linux")]
            match process_uses(&path) {
                Ok(false) => {}
                Ok(true) => continue,
                Err(error) => {
                    crate::launcher_log::line(format!(
                        "Kept the leftover build folder {}: the processes couldn't be read ({error}).",
                        path.display()
                    ));
                    continue;
                }
            }
            let bytes = Tree::build(&path).map_or(0, |tree| tree.size(&mut HashSet::new()));
            // A folder in use, locked or refused stays: it is still counted and shown.
            // Clearing empties the folder down to its cache locks; the folder then goes.
            if Self::clear_entry(&self.directory, &native_dir, Path::new(&name)).is_ok()
                && fs::remove_dir_all(&path).is_ok()
            {
                freed += bytes;
                // What proved its build over differs by system: say which proof it was.
                crate::launcher_log::line(format!(
                    "Removed the leftover build folder {}: no build holds the build lease or its owner lock{}.",
                    path.display(),
                    if cfg!(target_os = "linux") {
                        ", and no process has it open"
                    } else {
                        " (on this system a compiler that left its process tree isn't looked for further)"
                    }
                ));
            }
        }
        Ok(freed)
    }
}

/// Whether any process of this user has `folder` as its cwd, exe or root, or holds a file in it
/// open. Any other failure to read `/proc` is an error: the folder is then not proven unused.
#[cfg(target_os = "linux")]
fn process_uses(folder: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let ours = fs::metadata("/proc/self")?.uid();
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if !process
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let process = process.path();
        match fs::metadata(&process) {
            Ok(meta) if meta.uid() != ours => continue,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        // A process that ends while it is read fails the read in more ways than NotFound (ESRCH
        // among them): an error from a process that is gone afterwards is no use of the folder.
        // One this user can't read (EACCES: it isn't dumpable, or it changes credentials as it
        // execs) is no compiler a build started: those run as the player and stay readable.
        let ended = |error: std::io::Error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) || !process.exists()
            {
                Ok(())
            } else {
                Err(error)
            }
        };
        let mut links = vec![
            process.join("cwd"),
            process.join("exe"),
            process.join("root"),
        ];
        match fs::read_dir(process.join("fd")) {
            Ok(fds) => {
                for fd in fds {
                    match fd {
                        Ok(fd) => links.push(fd.path()),
                        Err(error) => ended(error)?,
                    }
                }
            }
            Err(error) => {
                ended(error)?;
                continue;
            }
        }
        for link in links {
            match fs::read_link(&link) {
                Ok(target) if target.starts_with(folder) => return Ok(true),
                Ok(_) => {}
                // The process ended, or (exe of a kernel thread) has no such link.
                Err(error) => ended(error)?,
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
