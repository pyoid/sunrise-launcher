//! One job per item on scoped threads, for the passes that touch every file of a snapshot.
use crate::native_mods::Cancellation;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex, PoisonError,
    },
};

/// File and worker failures keep their cause until the snapshot is reported to Play.
#[derive(Debug)]
pub(super) enum Error {
    File(std::path::PathBuf, std::io::Error),
    Input(String),
    Spawn(std::io::Error),
    Panic,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Input(error) => f.write_str(error),
            Self::Spawn(error) => write!(f, "could not start worker thread: {error}"),
            Self::Panic => f.write_str("a worker thread panicked"),
        }
    }
}

impl From<String> for Error {
    fn from(error: String) -> Self {
        Self::Input(error)
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}

/// The device's parallelism: the number of workers a pass uses.
pub(super) fn workers() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Runs `job` on every item with `workers` threads and returns the results in item order.
/// Each worker makes one `S` with `init` and reuses it for its items. The first error (or a
/// stop, checked before each item) stops every worker, and all of them are joined before this
/// returns. A panic in a worker is that error too: the scope would otherwise raise it again on
/// the caller's thread.
pub(super) fn run<T: Sync, S, R: Send>(
    items: &[T],
    workers: usize,
    stop: Option<&Cancellation>,
    init: impl Fn() -> S + Sync,
    job: impl Fn(&mut S, &T) -> Result<R, Error> + Sync,
) -> Result<Vec<R>, Error> {
    run_with_progress(items, workers, stop, None, init, job)
}

pub(super) fn run_with_progress<T: Sync, S, R: Send>(
    items: &[T],
    workers: usize,
    stop: Option<&Cancellation>,
    on_done: Option<&(dyn Fn(usize, usize) + Sync)>,
    init: impl Fn() -> S + Sync,
    job: impl Fn(&mut S, &T) -> Result<R, Error> + Sync,
) -> Result<Vec<R>, Error> {
    let cursor = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first = Mutex::new(None::<Error>);
    let fail = |error: Error| {
        failed.store(true, Ordering::Relaxed);
        first
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(error);
    };
    let work = || {
        let mut done = Vec::new();
        let mut state = init();
        while !failed.load(Ordering::Relaxed) {
            if let Some(Err(error)) = stop.map(Cancellation::check) {
                fail(error.into());
                break;
            }
            let at = cursor.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(at) else {
                break;
            };
            match job(&mut state, item) {
                Ok(result) => {
                    done.push((at, result));
                    if let Some(on_done) = on_done {
                        let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        on_done(n, items.len());
                    }
                }
                Err(error) => fail(error),
            }
        }
        done
    };
    let finished = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..workers.clamp(1, items.len().max(1)) {
            match std::thread::Builder::new().spawn_scoped(scope, || {
                catch_unwind(AssertUnwindSafe(&work)).unwrap_or_else(|_| {
                    fail(Error::Panic);
                    Vec::new()
                })
            }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    fail(Error::Spawn(error));
                    break;
                }
            }
        }
        let mut finished = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(done) => finished.extend(done),
                Err(_) => fail(Error::Panic),
            }
        }
        finished
    });
    if let Some(error) = first.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(error);
    }
    Ok(in_item_order(items.len(), finished))
}

fn in_item_order<R>(len: usize, finished: Vec<(usize, R)>) -> Vec<R> {
    let mut slots: Vec<Option<R>> = (0..len).map(|_| None).collect();
    for (at, result) in finished {
        slots[at] = Some(result);
    }
    slots
        .into_iter()
        .map(|slot| slot.expect("every item ran when no worker failed"))
        .collect()
}

#[cfg(test)]
mod tests;
