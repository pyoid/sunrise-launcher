//! Automatic cleanup uses the same capability-bounded inventory as explicit Clear.
use super::*;
use crate::backend;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
static TOAST: Mutex<Option<String>> = Mutex::new(None);
pub fn take_toast() -> Option<String> {
    TOAST.lock().unwrap_or_else(|e| e.into_inner()).take()
}
fn announce(report: &Report) {
    if let Some(message) = freed_message(report) {
        *TOAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
    }
}

impl Storage {
    /// Run inside the existing build lease, never briefly releasing its exclusion.
    pub(crate) fn after_build(cache: &Cache) -> Result<(), Error> {
        let root = backend::build_dir().canonicalize()?;
        let storage = Self {
            directory: Dir::open_ambient_dir(&root, cap_std::ambient_authority())?,
            root,
            lease: cache.share()?,
            data: backend::data_dir(),
            game: backend::game_home(),
        };
        storage.stamp_build()
    }

    pub fn evict(&self) -> Result<Report, Error> {
        let settings = backend::Settings::load();
        let limit = settings
            .cache_limit_gib
            .unwrap_or(policy::DEFAULT_LIMIT_GIB);
        let report = self.evict_inventory(
            SystemTime::now(),
            (limit != 0).then(|| limit.saturating_mul(1 << 30)),
        )?;
        announce(&report);
        Ok(report)
    }
}

/// A lifecycle trigger is best effort, never a retry of a busy unit.
/// Storage::open acquires the exclusive build lock before inventory: concurrent
/// triggers cannot both measure and free the same unit.
pub fn request_cleanup() {
    #[cfg(not(test))]
    std::thread::spawn(|| run_background_cleanup(Storage::open, |line| eprintln!("{line}")));
}

/// Stamp the shared build caches before releasing the lease, even on a failed build.
pub(crate) fn after_build_logged(cache: &Cache, warn: impl Fn(&str)) {
    if let Err(error) = Storage::after_build(cache) {
        warn(&crate::native_mods::cache_error(error));
    }
}

/// A minute after the launcher starts, once: the launch-time trigger, off the UI thread.
pub fn cleanup_after_start() {
    #[cfg(not(test))]
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        run_background_cleanup(Storage::open, |line| eprintln!("{line}"));
    });
}

pub(super) fn run_background_cleanup(
    open: impl FnOnce() -> Result<Storage, Error>,
    mut log: impl FnMut(&str),
) {
    let result = open().and_then(|storage| {
        let started = Instant::now();
        log("storage cleanup: start");
        let report = storage.evict()?;
        let freed_bytes: u64 = report.rows.iter().map(|row| row.freed_bytes).sum();
        log(&format!(
            "storage cleanup: end ({} ms, {freed_bytes} bytes freed)",
            started.elapsed().as_millis()
        ));
        Ok(())
    });
    match result {
        Ok(()) => (),
        Err(Error::Busy) => log("storage cleanup: skipped (build lease busy)"),
        Err(error) => log(&crate::native_mods::cache_error(error)),
    }
}

pub fn cleanup() -> Result<Report, Error> {
    Storage::open()?.evict()
}

pub fn freed_message(report: &Report) -> Option<String> {
    let bytes: u64 = report.rows.iter().map(|row| row.freed_bytes).sum();
    (bytes > 0).then(|| {
        crate::i18n::trf(
            "Freed {size} of old caches",
            &[("size", &super::format_bytes(bytes))],
        )
    })
}
