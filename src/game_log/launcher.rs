//! The launcher's own log: every line goes, scrubbed, to a rotating file in the logs folder and to
//! an in-memory ring the Logs panel reads. Words in log lines are English: the log is for bug
//! reports, not for the player to read in their language.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use sunrise_launcher_telemetry::scrub::Scrubber;

/// How many of the newest lines the ring keeps.
const RING_LINES: usize = 5000;
/// The size at which `launcher.log` is rotated.
const FILE_LIMIT: u64 = 1 << 20;
/// How many rotated files are kept: `launcher.1.log` to `launcher.3.log`.
const OLD_FILES: u32 = 3;
/// What replaces a secret.
const SECRET: &str = "<secret>";
/// Query, form and JSON keys whose values are secrets. A key matches as the end of a longer
/// name (`access_token`, `refresh_token`, `id_token` all end in `token`).
const SECRET_KEYS: [&str; 6] = ["code", "token", "client_secret", "password", "key", "sig"];

/// The newest lines, numbered by the revision at which each was logged.
struct Ring {
    lines: VecDeque<String>,
    /// How many lines were ever pushed.
    revision: u64,
}

impl Ring {
    const fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            revision: 0,
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() == RING_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.revision += 1;
    }

    /// The lines after revision `since` that are still kept, and the newest revision.
    fn since(&self, since: u64) -> (u64, Vec<String>) {
        let first = self.revision - self.lines.len() as u64;
        let skip = since.saturating_sub(first).min(self.lines.len() as u64) as usize;
        (
            self.revision,
            self.lines.iter().skip(skip).cloned().collect(),
        )
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

/// `dir/launcher.log` and its rotated files. A file that can't be opened or written is left
/// alone from then on, so a full disk never slows or breaks the launcher.
struct LogFile {
    dir: PathBuf,
    file: Option<File>,
    size: u64,
    limit: u64,
}

impl LogFile {
    fn open(dir: &Path, limit: u64) -> Self {
        let file = (|| {
            fs::create_dir_all(dir).ok()?;
            let folder = fs::symlink_metadata(dir).ok()?;
            if !folder.is_dir() || linked(&folder) {
                return None;
            }
            let path = dir.join("launcher.log");
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() && !linked(&meta) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                _ => return None,
            }
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            options.open(path).ok()
        })();
        let size = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            dir: dir.to_owned(),
            file,
            size,
            limit,
        }
    }

    fn write(&mut self, line: &str) {
        if self.file.is_none() {
            return;
        }
        if self.size >= self.limit {
            self.rotate();
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if writeln!(file, "{line}").is_err() {
            self.file = None;
            return;
        }
        self.size += line.len() as u64 + 1;
    }

    /// Moves `launcher.log` to `launcher.1.log` and each older file one up, dropping the oldest,
    /// then starts a new `launcher.log`.
    fn rotate(&mut self) {
        // Windows can't rename an open file, nor onto an existing one.
        self.file = None;
        let name = |n: u32| match n {
            0 => self.dir.join("launcher.log"),
            n => self.dir.join(format!("launcher.{n}.log")),
        };
        for n in (0..OLD_FILES).rev() {
            let _ = fs::remove_file(name(n + 1));
            let _ = fs::rename(name(n), name(n + 1));
        }
        *self = Self::open(&self.dir, self.limit);
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring::new());
static FILE: Mutex<Option<LogFile>> = Mutex::new(None);
/// The scrubber, built on the first line after `init`.
static SCRUBBER: Mutex<Option<Arc<Scrubber>>> = Mutex::new(None);
/// What `hide` was told, kept for a scrubber that isn't built yet.
static HIDDEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A lock that a panic elsewhere can't turn into a panic here.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the file: `dir/launcher.log`, rotated at 1 MiB into launcher.1.log .. launcher.3.log
/// (oldest dropped). Called once by `main()` before anything else logs, for the window and the
/// command line alike. Without it (tests, other tools) lines go only to the ring.
pub fn init(dir: &Path) {
    {
        let mut file = lock(&FILE);
        if file.is_some() {
            return;
        }
        *file = Some(LogFile::open(dir, FILE_LIMIT));
    }
    line(format!(
        "Sunrise launcher {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
}

/// The logs folder `init` was given.
pub fn dir() -> Option<PathBuf> {
    lock(&FILE).as_ref().map(|f| f.dir.clone())
}

/// One line: "HH:MM:SS.mmm text" (local time, chrono), scrubbed, into the ring and the file.
/// Multi-line text becomes several lines. Never panics; a file error is ignored after the first.
pub fn line(text: impl AsRef<str>) {
    let text = scrub(text.as_ref());
    let stamp = chrono::Local::now().format("%H:%M:%S%.3f");
    let mut file = lock(&FILE);
    for text in text.lines() {
        let line = format!("{stamp} {text}");
        if let Some(file) = file.as_mut() {
            file.write(&line);
        }
        lock(&RING).push(line);
    }
}

type PlayDetailSink = std::sync::Arc<dyn Fn(String) + Send + Sync>;

thread_local! {
    static PLAY_DETAIL: RefCell<Option<PlayDetailSink>> = const { RefCell::new(None) };
}

/// The originating Play worker owns the sink. Meters created on it can report from any thread.
pub struct PlayDetailGuard {
    prev: Option<PlayDetailSink>,
    _origin: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for PlayDetailGuard {
    fn drop(&mut self) {
        PLAY_DETAIL.replace(self.prev.take());
    }
}

pub fn on_play_detail(sink: impl Fn(String) + Send + Sync + 'static) -> PlayDetailGuard {
    PlayDetailGuard {
        prev: PLAY_DETAIL.replace(Some(Arc::new(sink))),
        _origin: std::marker::PhantomData,
    }
}

/// Sends a Play step line to the window (when a sink is installed) and to `launcher.log`.
pub fn play_detail(text: impl Into<String>) {
    let text = text.into();
    line(&text);
    play_detail_ui_only(text);
}

/// Sends a Play step line to the window without adding another `launcher.log` line.
pub fn play_detail_ui_only(text: impl Into<String>) {
    let text = text.into();
    let sink = PLAY_DETAIL.with(|sink| sink.borrow().clone());
    if let Some(sink) = sink {
        sink(text);
    }
}

/// Throttles per-file progress updates from parallel workers so the window updates every ~250 ms
/// and `launcher.log` gets a line at most every ~2 s without flooding either.
pub struct PlayMeter {
    sink: Option<PlayDetailSink>,
    format: fn(usize, usize) -> String,
    state: Mutex<(Option<Instant>, Option<Instant>, usize)>,
}

impl PlayMeter {
    pub fn new(format: fn(usize, usize) -> String) -> Self {
        let sink = PLAY_DETAIL.with(|sink| sink.borrow().clone());
        Self {
            sink,
            format,
            state: Mutex::new((None, None, 0)),
        }
    }

    pub fn tick(&self, done: usize, total: usize) {
        if total == 0 {
            return;
        }
        let now = Instant::now();
        // Keep the count and its delivery ordered even when a worker is preempted.
        let mut state = lock(&self.state);
        let shown = done.min(total);
        if shown <= state.2 {
            return;
        }
        state.2 = shown;
        let emit_ui = self.sink.is_some()
            && (shown == total
                || state.0.is_none_or(|t| {
                    now.saturating_duration_since(t) >= Duration::from_millis(250)
                }));
        let emit_log = state
            .1
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(2));
        if emit_ui || emit_log {
            let text = (self.format)(shown, total);
            if emit_log {
                state.1 = Some(now);
                line(&text);
            }
            if let Some(sink) = self.sink.as_ref().filter(|_| emit_ui) {
                state.0 = Some(now);
                sink(text);
            }
        }
    }
}

/// A long step. Logs "{what}: started" now; `done` logs "{what}: done in {duration}{: detail}",
/// `failed` logs "{what}: failed after {duration}: {why}", and a Step dropped without either
/// logs "{what}: ended after {duration} with no result". Duration as "850 ms" under a second,
/// else "12.3 s". Plain words, so any font shows them and a search finds them.
pub fn step(what: impl Into<String>) -> Step {
    let what = what.into();
    line(format!("{what}: started"));
    Step {
        what,
        started: Instant::now(),
        finished: false,
    }
}

/// A step that has started and not yet said how it ended.
#[must_use = "a step that is dropped logs that it ended without a result"]
pub struct Step {
    what: String,
    started: Instant,
    finished: bool,
}

impl Step {
    /// Progress inside the step.
    pub fn note(&self, text: impl AsRef<str>) {
        line(format!("{}: {}", self.what, text.as_ref()));
    }

    pub fn done(mut self, detail: impl AsRef<str>) {
        self.finished = true;
        let detail = detail.as_ref();
        let sep = if detail.is_empty() { "" } else { ": " };
        line(format!(
            "{}: done in {}{sep}{detail}",
            self.what,
            duration_text(self.started.elapsed())
        ));
    }

    pub fn failed(mut self, why: impl AsRef<str>) {
        self.finished = true;
        line(format!(
            "{}: failed after {}: {}",
            self.what,
            duration_text(self.started.elapsed()),
            why.as_ref()
        ));
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if !self.finished {
            line(format!(
                "{}: ended after {} with no result",
                self.what,
                duration_text(self.started.elapsed())
            ));
        }
    }
}

fn duration_text(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(1) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{:.1} s", elapsed.as_secs_f64())
    }
}

/// The ring (newest 5000 lines): the lines after revision `since`, and the newest revision.
/// Revision counts every line ever logged, so a reader at revision r asks `since(r)`.
pub fn since(since: u64) -> (u64, Vec<String>) {
    lock(&RING).since(since)
}

/// Hide `term` from now on (an account name, a server address). Terms under 3 chars ignored.
/// The accounts are read often, so a term already hidden is skipped.
pub fn hide(term: impl Into<String>) {
    let term = term.into();
    let mut scrubber = lock(&SCRUBBER);
    let mut hidden = lock(&HIDDEN);
    if hidden.contains(&term) {
        return;
    }
    if let Some(built) = scrubber.as_mut() {
        *built = Arc::new((**built).clone().term(term.clone()));
    }
    hidden.push(term);
}

/// Account and world readers register private terms; the logger reads no player data itself.
fn scrubber() -> Arc<Scrubber> {
    let mut slot = lock(&SCRUBBER);
    if let Some(built) = slot.as_ref() {
        return built.clone();
    }
    let mut base = Scrubber::new();
    for term in lock(&HIDDEN).iter() {
        base = base.term(term);
    }
    let built = Arc::new(base);
    *slot = Some(built.clone());
    built
}

/// The scrub every shown, copied or saved line passes (the game's lines in the panel too).
pub fn scrub(text: &str) -> String {
    hide_secrets(&scrubber().scrub(text))
}

/// `text` with `SECRET` in place of what looks like a credential: a JWT, a bearer token, the
/// value of a key like `code` or `access_token` (as `k=v` or `"k":"v"`), and any long run of
/// base64url characters that mixes upper case, lower case and digits (lowercase hex digests
/// and ordinary words stay).
fn hide_secrets(text: &str) -> String {
    let mut out = text.to_owned();
    for key in SECRET_KEYS {
        let lower = out.to_ascii_lowercase();
        out = replace_spans(&out, |from| value_after_key(&lower, key, from));
    }
    let lower = out.to_ascii_lowercase();
    out = replace_spans(&out, |from| bearer_token(&lower, from));
    replace_spans(&out, |from| secret_run(&out, from))
}

/// `text` with each span `next` finds (given where to look from) replaced by `SECRET`.
fn replace_spans(text: &str, mut next: impl FnMut(usize) -> Option<Range<usize>>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    while let Some(span) = next(from) {
        out.push_str(&text[from..span.start]);
        out.push_str(SECRET);
        from = span.end;
    }
    out.push_str(&text[from..]);
    out
}

/// The value of the next `key` (as a whole word, in `lower`, ASCII-lowercased text) at or after
/// `from`, written `key=value`, `"key":"value"` or with escaped quotes. A bare `key: value` is
/// ordinary prose and stays.
fn value_after_key(lower: &str, key: &str, from: usize) -> Option<Range<usize>> {
    let bytes = lower.as_bytes();
    let quote = |b: &u8| matches!(b, b'"' | b'\'' | b'\\');
    let mut at = from;
    while let Some(found) = lower[at..].find(key) {
        let start = at + found;
        let mut i = start + key.len();
        at = i;
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let quoted = bytes.get(i).is_some_and(quote);
        while bytes.get(i).is_some_and(quote) {
            i += 1;
        }
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        match bytes.get(i) {
            Some(b'=') => {}
            Some(b':') if quoted => {}
            _ => continue,
        }
        i += 1;
        while bytes.get(i).is_some_and(|b| *b == b' ' || quote(b)) {
            i += 1;
        }
        let len = bytes[i..]
            .iter()
            .take_while(|b| {
                !quote(b)
                    && !matches!(b, b'&' | b' ' | b'\t' | b'\r' | b'\n' | b',' | b';')
                    && !matches!(b, b')' | b'}' | b']')
            })
            .count();
        if len > 0 && &lower[i..i + len] != SECRET {
            return Some(i..i + len);
        }
    }
    None
}

/// The token of the next `Bearer <token>` in `lower` at or after `from`.
fn bearer_token(lower: &str, from: usize) -> Option<Range<usize>> {
    const HEAD: &str = "bearer ";
    let mut at = from;
    while let Some(found) = lower[at..].find(HEAD) {
        let start = at + found + HEAD.len();
        at = start;
        let len = lower.as_bytes()[start..]
            .iter()
            .take_while(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
            })
            .count();
        if len > 0 {
            return Some(start..start + len);
        }
    }
    None
}

/// The next run of base64url characters (with `.` and `=`) at or after `from` that is a JWT
/// (`eyJ…` with two dots) or is 32 or more characters with an upper case letter, a lower case
/// letter and a digit.
fn secret_run(text: &str, from: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let in_run = |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'=');
    let mut i = from;
    while i < bytes.len() {
        if !in_run(&bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).is_some_and(in_run) {
            i += 1;
        }
        let run = &text[start..i];
        let jwt = run.contains("eyJ") && run.matches('.').count() >= 2;
        let long = run.len() >= 32
            && run.bytes().any(|b| b.is_ascii_uppercase())
            && run.bytes().any(|b| b.is_ascii_lowercase())
            && run.bytes().any(|b| b.is_ascii_digit());
        if jwt || long {
            return Some(start..i);
        }
    }
    None
}

/// `eprintln!` that also logs: `sunrise_launcher::elog!` (bin) / `crate::elog!` (lib).
#[macro_export]
macro_rules! elog {
    ($($t:tt)*) => {{
        let text = format!($($t)*);
        eprintln!("{text}");
        $crate::game_log::launcher::line(&text);
    }};
}

#[cfg(test)]
mod tests;
