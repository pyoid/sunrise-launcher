//! A Sunrise build's first start. The launcher before it (Sunrise's own, then the arismoko
//! fork's: `identity::EARLIER`) left the player's settings, accounts and instances; they are
//! copied into Sunrise's own folders, never moved, so the player keeps them and the earlier
//! launcher keeps working. The earlier launcher's folders are only read.
use crate::data_location::{self, Roots};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

static STARTUP_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The error from [`at_start`], if carrying over an earlier launcher's data failed.
pub fn startup_error() -> Option<String> {
    STARTUP_ERROR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// What a launcher folder holds that is the player's: settings, accounts (with the secrets file
/// of a computer without a keychain), the instances' names and icons, the servers made in the
/// launcher. Game builds, downloads, caches and logs are made again.
const LAUNCHER_STATE: [&str; 7] = [
    crate::paths::SETTINGS,
    "accounts.json",
    crate::account::SECRETS_FILE,
    "account-secrets.key",
    "instances.json",
    "servers.json",
    "servers",
];

/// What a game folder holds that the game makes again: its content cache (under the earlier
/// launcher's name or this one's), compiled scripts and downloads. Not copied: a Horizon game
/// folder's `.horizon-cache` is most of its files, and its entries are links into the earlier
/// launcher's game builds, which a copy refuses.
const GAME_CACHES: [&str; 4] = [".horizon-cache", ".sunrise-cache", "wasm-cache", "cache"];

/// The file that says the stage holds the whole copy, written before the first rename.
const COMPLETE: &str = "complete";

/// All a start whose carry-over failed makes in the launcher folder: the instances list's lock
/// and the caches. A launcher folder holding anything else is the player's and is never replaced,
/// even something this list doesn't know (a theme, a saved skin, a publisher pin).
const LEFT_BY_A_FAILED_START: [&str; 2] = [".instances.lock", "cache"];

/// Where a launcher folder holding only [`LEFT_BY_A_FAILED_START`] goes while the copy takes
/// its place.
const LEFTOVER: &str = ".carry-over-leftover";

/// Runs [`carry_over`] on the player's folders before anything reads them, unless a home
/// override or a moved Sunrise location decides where the data is. A failure is remembered in
/// [`startup_error`] and reported to stderr so startup skips creating an empty root and the
/// player can repair the earlier location and restart to retry.
pub fn at_start() -> Result<(), String> {
    *STARTUP_ERROR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let overridden = ["SUNRISE_LAUNCHER_HOME", "SUNRISE_HOME"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()));
    if crate::identity::EARLIER.is_empty()
        || overridden
        || !matches!(data_location::configured_roots(), Ok(None))
    {
        return Ok(());
    }
    let (Some(data), Some(config)) = (dirs::data_dir(), dirs::config_dir()) else {
        return Ok(());
    };
    if let Err(error) = carry_over(&data, &config) {
        crate::elog!("Could not copy the earlier launcher's data: {error}");
        *STARTUP_ERROR
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
        return Err(error);
    }
    Ok(())
}

/// Copies the data of the first earlier launcher that has any into this launcher's folders under
/// `data`, when neither this launcher's data folder nor its game folder exists yet. Earlier
/// launchers' folders are looked for under `data`, or where their `location.json` under `config`
/// moved them. Returns whether anything was copied.
///
/// Everything is copied into a folder beside the targets first and renamed into place at the
/// end, so a start that stops halfway copies again, from the beginning, next time.
pub fn carry_over(data: &Path, config: &Path) -> Result<bool, String> {
    let launcher = crate::identity::launcher_folder_in(data);
    let game = crate::identity::game_folder_in(data);
    let stage = launcher.with_file_name(".carry-over");
    let leftover = launcher.with_file_name(LEFTOVER);
    if present(&launcher)? || present(&game)? {
        // A copy that was finished but stopped between its two renames: the one folder that is
        // still in the stage goes where the game reads it. Refuse links at the stage root, the
        // marker, or either staged part before touching anything.
        if refuse_link(&stage)?.is_some() {
            let marker_meta = refuse_link(&stage.join(COMPLETE))?;
            refuse_link(&stage.join("launcher"))?;
            refuse_link(&stage.join("game"))?;
            if marker_meta.is_some_and(|m| m.is_file())
                || first_rename_done(&stage, &launcher, &game)
            {
                return resume(&stage, &launcher, &game, &leftover);
            }
            // A copy that reached both targets but could not remove its stage (a file appearing in
            // it mid-delete) left it beside them; nothing reads it, so it goes on a later start.
            if launcher.exists() && game.exists() {
                settle(&stage);
            }
        }
        // A start whose carry-over failed still makes the launcher folder for its lock and
        // caches. That alone is no reason to stop copying: anything else in it is.
        if present(&game)? || launcher_in_use(&launcher)? {
            return Ok(false);
        }
    }
    let mut source = None;
    for &(old_launcher, old_game, location) in crate::identity::EARLIER {
        if let Some(roots) = earlier_roots(data, config, old_launcher, old_game, location)? {
            source = Some(roots);
            break;
        }
    }
    let Some(source) = source else {
        return Ok(false);
    };
    if refuse_link(&stage)?.is_some() {
        fs::remove_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    }
    let staged = Roots {
        launcher: stage.join("launcher"),
        game: stage.join("game"),
        build: stage.join("launcher"),
    };
    fs::create_dir_all(&staged.launcher).map_err(|e| format!("{}: {e}", stage.display()))?;
    let mut quiet = |_: &str| {};
    for name in LAUNCHER_STATE {
        let from = source.launcher.join(name);
        if from.exists() {
            data_location::copy_tree(&from, &staged.launcher.join(name), &[], &mut quiet)?;
        }
    }
    data_location::copy_tree_except(&source.game, &staged.game, &[], &GAME_CACHES, &mut quiet)?;
    let target = Roots {
        launcher: launcher.clone(),
        game: game.clone(),
        build: launcher.clone(),
    };
    crate::relocation::prepare_paths(&staged, &source, &target)?;
    // What a failed start left in the launcher folder goes aside before the marker, so a stage
    // marked complete never meets a launcher folder it would have to step around. Another window
    // can write the player's files there while the copy runs: then the copy is dropped.
    let set_aside = match set_aside(&launcher, &leftover)? {
        Some(moved) => moved,
        None => {
            discard(&stage);
            return Ok(false);
        }
    };
    // From here the stage holds the whole copy: a start that finds it with one target in place
    // finishes the renames (`resume`) instead of leaving the other folder in it.
    // The marker is what a later start trusts: it must be on disk before a rename is.
    let marker = stage.join(COMPLETE);
    fs::File::create(&marker)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("{}: {e}", marker.display()))?;
    data_location::sync_dir(&stage)?;
    fs::rename(&staged.launcher, &launcher).map_err(|e| format!("{}: {e}", launcher.display()))?;
    fs::rename(&staged.game, &game).map_err(|e| format!("{}: {e}", game.display()))?;
    discard(&stage);
    if set_aside {
        remove_leftover(&leftover);
    }
    Ok(true)
}

/// Whether anything is at `path`, a broken link included.
fn present(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Whether `launcher` holds anything but what a failed start makes ([`LEFT_BY_A_FAILED_START`]),
/// so a copy must not replace it. A link there, even a broken one, is the player's doing.
fn launcher_in_use(launcher: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(launcher) {
        Ok(meta) if crate::native_cache::linked(&meta) || !meta.is_dir() => return Ok(true),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", launcher.display())),
    }
    let entries = fs::read_dir(launcher).map_err(|e| format!("{}: {e}", launcher.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", launcher.display()))?;
        if !LEFT_BY_A_FAILED_START
            .iter()
            .any(|name| entry.file_name() == *name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Moves a launcher folder that holds only what a failed start left to `leftover`, so the copy
/// can take its place. `Some(true)` when it was moved, `Some(false)` when there was none, and
/// `None` when it is the player's: then it stays where it is. It is checked again once moved, so
/// a file written into it just before the move puts it back.
fn set_aside(launcher: &Path, leftover: &Path) -> Result<Option<bool>, String> {
    if !present(launcher)? {
        return Ok(Some(false));
    }
    if launcher_in_use(launcher)? {
        return Ok(None);
    }
    if present(leftover)? {
        // Only ever what an earlier start set aside; anything else there is left alone.
        if launcher_in_use(leftover)? {
            return Ok(None);
        }
        fs::remove_dir_all(leftover).map_err(|e| format!("{}: {e}", leftover.display()))?;
    }
    fs::rename(launcher, leftover).map_err(|e| format!("{}: {e}", launcher.display()))?;
    if launcher_in_use(leftover)? {
        fs::rename(leftover, launcher).map_err(|e| format!("{}: {e}", launcher.display()))?;
        return Ok(None);
    }
    Ok(Some(true))
}

/// Removes what [`set_aside`] moved, once the copy is in its place. It is checked again first,
/// and a failure leaves it for a later start.
fn remove_leftover(leftover: &Path) {
    match launcher_in_use(leftover) {
        Ok(false) => {
            if let Err(error) = fs::remove_dir_all(leftover) {
                crate::elog!("Could not remove {}: {error}", leftover.display());
            }
        }
        Ok(true) => crate::elog!(
            "Kept {}: something besides caches was written to it.",
            leftover.display()
        ),
        Err(error) => crate::elog!("{error}"),
    }
}

/// Whether an unmarked stage (one a launcher before the marker, 1.0.0 or 1.0.1, left) is provably
/// a finished copy. Those launchers copied everything, and made `launcher/` in the stage first,
/// before renaming it into place and then `game/`: so a stage without `launcher/` but with
/// `game/`, beside a launcher folder and no game folder, stopped between the two renames.
/// Any other unmarked stage may be half a copy.
fn first_rename_done(stage: &Path, launcher: &Path, game: &Path) -> bool {
    launcher.is_dir()
        && !game.exists()
        && !stage.join("launcher").exists()
        && stage.join("game").is_dir()
}

/// Finishes a copy that stopped between its two renames: each folder still in the stage whose
/// target does not exist is renamed into place. A target that exists is never replaced, so saves
/// already there stay as they are, and what is left in the stage is removed once both are there.
fn resume(stage: &Path, launcher: &Path, game: &Path, leftover: &Path) -> Result<bool, String> {
    let mut moved = false;
    let mut set_aside_here = false;
    for (part, target) in [("launcher", launcher), ("game", game)] {
        let from = stage.join(part);
        if !from.is_dir() {
            continue;
        }
        // A launcher folder that a failed start made after the copy was marked complete holds
        // nothing of the player's: the copy goes in its place, not into a kept stage.
        if part == "launcher" && set_aside(target, leftover)? == Some(true) {
            set_aside_here = true;
        }
        if !present(target)? {
            fs::rename(&from, target).map_err(|e| format!("{}: {e}", target.display()))?;
            moved = true;
        }
    }
    if launcher.exists() && game.exists() {
        settle(stage);
    }
    if set_aside_here {
        remove_leftover(leftover);
    }
    Ok(moved)
}

/// Ends a stage whose copy is over. When something in it did not go into place (its target
/// already existed), the stage is kept as `.carry-over-kept-<UTC time>` beside it, with one line
/// saying where; only a stage with nothing left in it is removed.
fn settle(stage: &Path) {
    let left: Vec<&str> = ["launcher", "game"]
        .into_iter()
        .filter(|part| stage.join(part).is_dir())
        .collect();
    if left.is_empty() {
        return discard(stage);
    }
    let kept = stage.with_file_name(format!(
        ".carry-over-kept-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    match fs::rename(stage, &kept) {
        Ok(()) => crate::elog!(
            "Kept {} ({}): the data folder it was copied for already existed, so it was not replaced.",
            kept.display(),
            left.join(", ")
        ),
        Err(error) => crate::elog!("Could not keep {}: {error}", stage.display()),
    }
}

/// Removes the stage of a finished copy. A failure is reported and leaves it for the next start.
fn discard(stage: &Path) {
    if let Err(error) = fs::remove_dir_all(stage) {
        crate::elog!(
            "{}",
            crate::i18n::trf(
                "Could not remove {path}, left from copying the earlier launcher's data; the next start tries again: {error}",
                &[
                    ("path", &stage.display().to_string()),
                    ("error", &error.to_string()),
                ],
            )
        );
    }
}

/// Returns `path`'s own metadata (without following links), refusing symlinks and junctions.
fn refuse_link(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if crate::native_cache::linked(&meta) => Err(crate::i18n::trf(
            "Symbolic links and reparse points cannot be relocated: {path}",
            &[("path", &path.display().to_string())],
        )),
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// An earlier launcher's folders, when its launcher or game folder exists: where its
/// `location.json` moved them, else its default folders under `data`.
fn earlier_roots(
    data: &Path,
    config: &Path,
    launcher: &str,
    game: &str,
    location: &str,
) -> Result<Option<Roots>, String> {
    let moved = data_location::resolve_at(&config.join(location).join(crate::paths::LOCATION))?;
    let roots = moved.unwrap_or_else(|| Roots {
        launcher: data.join(launcher),
        game: data.join(game),
        build: data.join(launcher),
    });
    Ok((roots.launcher.is_dir() || roots.game.is_dir()).then_some(roots))
}

#[cfg(test)]
mod tests;
