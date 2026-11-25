//! The Servers page: create a server in one dialog, then start, stop and watch it, give friends its
//! address, manage who may join, change its settings, back it up and update it, all with buttons. Every
//! action runs `sunrise-server` or calls the running server's control API (`sunrise_launcher::servers`).
use super::line_edit::{self, Key, LineEdit};
use super::*;
use crate::frame::Reach as _;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use sunrise_launcher::heads;
use sunrise_launcher::servers::{
    self, Applied, Backup, Entry, Fix, NewServer, Problem, Status, World,
};
use sunrise_launcher_theme::glass::radius;

#[path = "glass_server_dialog.rs"]
mod glass_server_dialog;
#[path = "glass_servers.rs"]
mod glass_servers;

mod hosting;
#[path = "servers_format.rs"]
mod servers_format;
use servers_format::{backup_label, bytes, uptime};

/// One consent boundary for every launcher-managed start/restart route.
fn run_managed_server(
    tx: &std::sync::mpsc::Sender<backend::Msg>,
    dir: &Path,
    args: &[&str],
    log: &dyn Fn(String),
) -> Result<(), Problem> {
    crate::jar_launch::with_consent(tx, |permission| {
        servers::run_managed(dir, args, log, permission)
    })
}

/// The servers this launcher started in this session, stopped when it closes unless they run as a
/// service (`stop_on_quit`).
static STARTED_HERE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// How often the open server's status and log are read, and the list's.
const POLL_OPEN: Duration = Duration::from_millis(1000);
const POLL_LIST: Duration = Duration::from_secs(3);

/// Lines of the server's log kept on the page.
const CONSOLE_KEEP: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Field {
    Name,
    Seed,
    Players,
    Command,
    AddPlayer,
    Motd,
    Port,
    Token,
    Email,
    /// The instance dialog's (`instances_page.rs`).
    InstanceName,
    InstanceGroup,
    /// Home's Multiplayer section's (`multiplayer_page.rs`).
    ServerName,
    ServerAddress,
    /// The Launch panel's (`launch_page.rs`).
    PreLaunch,
    Wrapper,
    LaunchEnv,
    PostExit,
    DeleteServer,
    /// Settings' accent as hex (`themes_page.rs`).
    AccentHex,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Tab {
    Overview,
    Players,
    Settings,
    Backups,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Overview, "Overview"),
    (Tab::Players, "Players"),
    (Tab::Settings, "Settings"),
    (Tab::Backups, "Backups"),
];

const MODES: [(&str, &str); 3] = [
    ("survival", "Survival"),
    ("creative", "Creative"),
    ("adventure", "Adventure"),
];

const DIFFICULTIES: [(&str, &str); 4] = [
    ("peaceful", "Peaceful"),
    ("easy", "Easy"),
    ("normal", "Normal"),
    ("hard", "Hard"),
];

/// What the background work reports.
pub(super) enum Event {
    PublicAddress {
        request: u64,
        result: Result<std::net::Ipv4Addr, String>,
    },
    Polled {
        dir: PathBuf,
        status: Box<Status>,
        logs: Option<(u64, Vec<String>)>,
    },
    Statuses(Vec<(PathBuf, Status)>),
    Line(String),
    Done(Result<String, Problem>),
    Created(Result<Entry, Problem>),
    Deleted {
        dir: PathBuf,
        result: Result<(), String>,
    },
    Details {
        dir: PathBuf,
        request: u64,
        config: Option<Value>,
        backups: Vec<Backup>,
        service: bool,
    },
    Applied {
        dir: PathBuf,
        result: Result<Applied, Problem>,
    },
    Pvp(Option<bool>),
    UpdateCheck(Result<Value, Problem>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicLookup {
    Idle,
    Loading(u64),
    Detected,
    Failed,
}

/// The Create dialog's answers.
pub(super) struct Create {
    new_world: bool,
    copy: Option<PathBuf>,
    worlds: Vec<(String, PathBuf)>,
    mode: &'static str,
    difficulty: &'static str,
    max_players: u32,
    friends_only: bool,
    check_accounts: bool,
    eula: bool,
    /// Create was pressed without the EULA ticked.
    eula_missing: bool,
    /// The glass dialog's friends let in, the first an operator (the light one types them in
    /// `Field::Players`).
    friends: Vec<String>,
}

#[derive(Clone)]
struct SettingsBatch {
    dir: PathBuf,
    changes: Vec<(String, String)>,
}

impl SettingsBatch {
    fn changes_port(&self) -> bool {
        self.changes.iter().any(|(key, _)| key == "server.port")
    }
}

#[derive(Default)]
struct SettingsOutcome {
    restart: BTreeSet<String>,
    /// True when a restart-requiring save succeeded while `running: true` against a stale
    /// non-running cached status, so the next `"running"` poll is catch-up rather than a restart.
    restart_needs_stop: bool,
    problem: Option<Problem>,
    /// Setting keys from the batch that produced `problem`; an unrelated later save keeps it.
    failed_keys: BTreeSet<String>,
    /// Recently saved values kept visible until a fresh settings read replaces them.
    saved: HashMap<String, String>,
}

impl Servers {
    /// Runs `job` on a thread with the page busy; its lines and result arrive as events.
    fn start_job(
        &mut self,
        label: impl Into<String>,
        job: impl FnOnce(&dyn Fn(String)) -> Result<String, Problem> + Send + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label.into());
        self.job_log.clear();
        self.problem = None;
        self.note = None;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let lines = tx.clone();
            let log = move |l: String| {
                let _ = lines.send(Event::Line(l));
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&log)))
                .unwrap_or_else(|panic| {
                    let error = (panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    Err(Problem::plain(trf(
                        "Could not run {program}: {error}",
                        &[("program", "sunrise-server"), ("error", &error)],
                    )))
                });
            let _ = tx.send(Event::Done(result));
        });
    }

    fn finish_job(&mut self, result: Result<String, Problem>) {
        self.busy = None;
        self.polled = None;
        match result {
            Ok(note) if !note.is_empty() => self.note = Some(note),
            Ok(_) => {}
            Err(problem) => self.problem = Some(problem),
        }
    }

    /// Work still owned by this launcher, not the lifetime of a dedicated server.
    pub(super) fn writing(&self) -> bool {
        self.busy.is_some() || self.creating || self.applying.is_some() || !self.queued.is_empty()
    }

    pub(super) fn location_busy(&self) -> bool {
        self.writing()
            || self
                .statuses
                .values()
                .any(|s| matches!(s.phase.as_str(), "running" | "starting" | "stopping"))
    }

    /// A stopped server's line in the list: "Survival · stopped", as the mockup's. The open
    /// server's mode is the one its settings show now, once read; the others' (and the open one's
    /// until then) come from their files.
    pub(super) fn stopped_line(&mut self, dir: &std::path::Path) -> String {
        let open = Some(self.shown("world", "game_mode"))
            .filter(|m| !m.is_empty() && self.open.as_deref() == Some(dir));
        let mode = open.or_else(|| {
            self.modes
                .entry(dir.to_path_buf())
                .or_insert_with(|| servers::game_mode(dir))
                .clone()
        });
        match mode {
            Some(mode) => {
                let label = MODES
                    .iter()
                    .find(|(id, _)| *id == mode)
                    .map_or(mode.clone(), |(_, label)| tr(label).to_string());
                trf("{mode} · stopped", &[("mode", &label)])
            }
            None => tr("Stopped").to_string(),
        }
    }
}

pub(super) struct Servers {
    list: Vec<Entry>,
    loaded: bool,
    pub(super) open: Option<PathBuf>,
    tab: Tab,
    statuses: HashMap<PathBuf, Status>,
    /// Each listed server's game mode from its `server.toml`, read once while it is listed.
    modes: HashMap<PathBuf, Option<String>>,
    polling: bool,
    polled: Option<Instant>,
    log: VecDeque<String>,
    log_next: u64,
    log_dir: Option<PathBuf>,
    console_scroll: Scrollbar,
    create_scroll: Scrollbar,
    /// The action under way, and what it printed.
    busy: Option<String>,
    job_log: VecDeque<String>,
    problem: Option<Problem>,
    note: Option<String>,
    pub(super) create: Option<Create>,
    creating: bool,
    config: Option<Value>,
    details_request: u64,
    /// Settings waiting for `config set`, and whether one runs.
    queued: VecDeque<SettingsBatch>,
    applying: Option<SettingsBatch>,
    /// A result stays with the server even when its page closes.
    settings_outcomes: HashMap<PathBuf, SettingsOutcome>,
    backups: Vec<Backup>,
    confirm_restore: Option<String>,
    confirm_delete: bool,
    service: Option<bool>,
    update: Option<Value>,
    /// `update --check` is under way.
    checking: bool,
    pvp: Option<bool>,
    texts: HashMap<Field, LineEdit>,
    focus: HashMap<Field, FocusHandle>,
    focused: Option<Field>,
    /// The field to put the keyboard in on the next frame.
    pub(super) focus_soon: Option<Field>,
    /// A suggested value shown selected: the first key typed replaces it.
    selected: Option<Field>,
    /// A field whose value was refused, and why: shown under it until it's edited.
    invalid: Option<(Field, &'static str)>,
    internet_address: bool,
    last_public_address: Option<std::net::Ipv4Addr>,
    public_lookup: PublicLookup,
    public_lookup_request: u64,
    public_lookup_in_flight: Option<u64>,
    faces: HashMap<String, Option<Vec<[u8; 4]>>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Servers {
    pub(super) fn new() -> Self {
        let (tx, rx) = crate::wake::channel();
        Self {
            list: Vec::new(),
            loaded: false,
            open: None,
            tab: Tab::Overview,
            statuses: HashMap::new(),
            modes: HashMap::new(),
            polling: false,
            polled: None,
            log: VecDeque::new(),
            log_next: 0,
            log_dir: None,
            console_scroll: Scrollbar::default(),
            create_scroll: Scrollbar::default(),
            busy: None,
            job_log: VecDeque::new(),
            problem: None,
            note: None,
            create: None,
            creating: false,
            config: None,
            details_request: 0,
            queued: VecDeque::new(),
            applying: None,
            settings_outcomes: HashMap::new(),
            backups: Vec::new(),
            confirm_restore: None,
            confirm_delete: false,
            service: None,
            update: None,
            checking: false,
            pvp: None,
            texts: HashMap::new(),
            focus: HashMap::new(),
            focused: None,
            focus_soon: None,
            selected: None,
            invalid: None,
            internet_address: false,
            last_public_address: None,
            public_lookup: PublicLookup::Idle,
            public_lookup_request: 0,
            public_lookup_in_flight: None,
            faces: HashMap::new(),
            tx,
            rx,
        }
    }

    pub(super) fn text(&mut self, f: Field) -> &mut LineEdit {
        self.texts.entry(f).or_default()
    }

    pub(super) fn value(&self, f: Field) -> &str {
        self.texts.get(&f).map_or("", LineEdit::text)
    }

    /// Whether this field has the keyboard, as of this frame.
    pub(super) fn has_focus(&self, f: Field) -> bool {
        self.focused == Some(f)
    }

    /// Which field has the keyboard, read every frame.
    pub(super) fn note_focus(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(h) = self.focus_soon.take().and_then(|f| self.focus.get(&f)) {
            window.focus(h, cx);
        }
        self.focused = self
            .focus
            .iter()
            .find(|(_, h)| h.is_focused(window))
            .map(|(f, _)| *f);
        if self.focused.is_some() && self.focused != self.selected {
            self.selected = None;
        }
    }

    fn status(&self) -> Status {
        self.open
            .as_ref()
            .and_then(|d| self.statuses.get(d).cloned())
            .unwrap_or_else(Status::unknown)
    }

    /// A setting from server.toml (as `config --json` gave it).
    fn setting(&self, section: &str, key: &str) -> Option<&Value> {
        self.config
            .as_ref()
            .and_then(|c| c.get(section)?.get(key))
            .filter(|v| !v.is_null())
    }

    fn share_address(&self, status: &Status) -> (Option<std::net::SocketAddr>, Option<u16>) {
        let saved = self
            .setting("server", "port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0);
        let saving_port = self
            .applying
            .iter()
            .chain(&self.queued)
            .any(|batch| self.open.as_ref() == Some(&batch.dir) && batch.changes_port());
        let address = match status.phase.as_str() {
            "running" => status.listener,
            "stopped" if !saving_port => saved.and_then(|port| {
                let bind = match self.setting("server", "bind") {
                    None => "",
                    Some(value) => value.as_str()?,
                };
                let ip: std::net::IpAddr = if bind.is_empty() {
                    std::net::Ipv4Addr::UNSPECIFIED.into()
                } else {
                    bind.parse().ok()?
                };
                Some(std::net::SocketAddr::new(ip.to_canonical(), port))
            }),
            _ => None,
        };
        (address, saved)
    }

    pub(super) fn reset_public_lookup(&mut self) {
        self.public_lookup = PublicLookup::Idle;
    }

    fn begin_public_lookup(&mut self) -> Option<u64> {
        if !self.internet_address || self.public_lookup_in_flight.is_some() {
            return None;
        }
        self.public_lookup_request += 1;
        let request = self.public_lookup_request;
        self.public_lookup_in_flight = Some(request);
        self.public_lookup = PublicLookup::Loading(request);
        Some(request)
    }

    fn finish_public_lookup(
        &mut self,
        request: u64,
        result: Result<std::net::Ipv4Addr, String>,
    ) -> bool {
        if self.public_lookup_in_flight != Some(request) {
            return false;
        }
        self.public_lookup_in_flight = None;
        if self.public_lookup == PublicLookup::Loading(request) {
            self.public_lookup = match result {
                Ok(ip) => {
                    self.last_public_address = Some(ip);
                    PublicLookup::Detected
                }
                Err(_) => PublicLookup::Failed,
            };
        }
        true
    }

    /// A setting as shown: what was just asked for, else what was recently saved awaiting a fresh
    /// read, else what server.toml holds.
    fn shown(&self, section: &str, key: &str) -> String {
        let full = format!("{section}.{key}");
        if let Some((_, value)) = self
            .queued
            .iter()
            .chain(self.applying.iter())
            .filter(|batch| self.open.as_ref() == Some(&batch.dir))
            .find_map(|batch| batch.changes.iter().find(|(key, _)| *key == full))
        {
            return value.clone();
        }
        if let Some(value) = self
            .open
            .as_ref()
            .and_then(|dir| self.settings_outcomes.get(dir))
            .and_then(|outcome| outcome.saved.get(&full))
        {
            return value.clone();
        }
        match self.setting(section, key) {
            Some(Value::String(s)) => s.clone(),
            None => String::new(),
            Some(v) => v.to_string(),
        }
    }

    fn queue_setting(&mut self, dir: PathBuf, key: &str, value: String) {
        if let Some(batch) = self.queued.iter_mut().find(|batch| batch.dir == dir) {
            batch.changes.retain(|(queued, _)| queued != key);
            batch.changes.push((key.to_owned(), value));
        } else {
            self.queued.push_back(SettingsBatch {
                dir,
                changes: vec![(key.to_owned(), value)],
            });
        }
    }

    /// Starts the oldest batch, even if its server is no longer open on the page.
    fn next_settings_batch(&mut self) -> Option<SettingsBatch> {
        if self.applying.is_some() {
            return None;
        }
        let batch = self.queued.pop_front()?;
        self.applying = Some(batch.clone());
        Some(batch)
    }

    /// Whether the displayed settings need re-reading now that this write has finished.
    fn finish_settings(&mut self, dir: &Path, result: Result<Applied, Problem>) -> bool {
        let finished = self.applying.take();
        self.modes.remove(dir);
        let cached_running = self
            .statuses
            .get(dir)
            .is_some_and(|status| status.phase == "running");
        let outcome = self.settings_outcomes.entry(dir.to_path_buf()).or_default();
        match result {
            Ok(applied) => {
                if let Some(batch) = finished {
                    for (key, value) in batch.changes {
                        outcome.failed_keys.remove(&key);
                        outcome.saved.insert(key, value);
                    }
                }
                if outcome.failed_keys.is_empty() {
                    outcome.problem = None;
                }
                if applied.running && !applied.restart.is_empty() {
                    outcome.restart.extend(applied.restart);
                    if !cached_running {
                        outcome.restart_needs_stop = true;
                    }
                }
            }
            Err(problem) => {
                outcome.problem = Some(problem);
                outcome.failed_keys = finished
                    .map(|batch| batch.changes.into_iter().map(|(key, _)| key).collect())
                    .unwrap_or_default();
            }
        }
        self.open.as_deref() == Some(dir) && !self.queued.iter().any(|batch| batch.dir == dir)
    }

    fn note_status(&mut self, dir: PathBuf, status: Status) -> bool {
        if self.statuses.get(&dir) == Some(&status) {
            return false;
        }
        if status.phase == "running" {
            let was_stopped = self
                .statuses
                .get(&dir)
                .is_some_and(|old| old.phase != "running");
            if let Some(outcome) = self.settings_outcomes.get_mut(&dir) {
                if outcome.restart_needs_stop {
                    // The status poll just caught up to the server that was already running
                    // when the save happened; keep the restart notice until a real restart.
                    outcome.restart_needs_stop = false;
                } else if was_stopped {
                    // A fresh start took that server's saved settings, not another server's.
                    outcome.restart.clear();
                }
            }
            if was_stopped && self.open.as_ref() == Some(&dir) {
                self.pvp = None;
            }
        } else if let Some(outcome) = self.settings_outcomes.get_mut(&dir) {
            outcome.restart_needs_stop = false;
        }
        self.statuses.insert(dir, status);
        true
    }

    fn dismiss_problem(&mut self, settings_for: Option<&Path>) {
        if let Some(dir) = settings_for {
            if let Some(outcome) = self.settings_outcomes.get_mut(dir) {
                outcome.problem = None;
                outcome.failed_keys.clear();
            }
        } else {
            self.problem = None;
        }
    }
}

/// Stops the servers this launcher started, unless they run in the background as a service.
pub(super) fn stop_on_quit() {
    for dir in STARTED_HERE.lock().unwrap().drain(..) {
        if servers::service_installed(&dir) {
            continue;
        }
        if let Some(api) = servers::Api::open(&dir) {
            let _ = api.call("POST", "/api/v1/stop", None);
        }
    }
}

/// Update closes the window to open the new launcher at once: a server is its own process and the
/// new window finds it running, so an update doesn't end anyone's game. It stays up when that new
/// window closes too, until it is stopped on the Servers page.
pub(super) fn keep_running_through_update() {
    STARTED_HERE.lock().unwrap().clear();
}

/// The player's own name, to let them in on a friends-only server: the Microsoft account that plays.
fn own_name(accounts: &Accounts) -> Option<String> {
    accounts.selected().map(|a| a.name.clone())
}

impl Launcher {
    // ----- Background work ------------------------------------------------------------------

    /// `sunrise-server <args>` on the open server, as a job.
    fn server_command(&mut self, label: &str, args: Vec<String>) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        let tx = self.tx.clone();
        self.servers.start_job(label.to_string(), move |log| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = match args.first().copied() {
                Some("start" | "restart") => {
                    if args[0] == "start" {
                        log(tr("A new world takes a minute the first time").into());
                    }
                    run_managed_server(&tx, &dir, &args, log)
                }
                _ => servers::run_streamed(&dir, &args, log),
            };
            result.map(|_| String::new())
        });
    }

    /// Reads the open server's settings, backups and service state again.
    fn load_details(&mut self) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        self.servers.details_request += 1;
        let request = self.servers.details_request;
        let tx = self.servers.tx.clone();
        std::thread::spawn(move || {
            let config = servers::config(&dir);
            let backups = servers::backups(&dir).unwrap_or_default();
            let service = servers::service_installed(&dir);
            let _ = tx.send(Event::Details {
                dir,
                request,
                config,
                backups,
                service,
            });
        });
    }

    pub(super) fn open_server(&mut self, dir: PathBuf) {
        if self.servers.open.as_ref() == Some(&dir) {
            return;
        }
        let s = &mut self.servers;
        s.statuses.remove(&dir);
        s.open = Some(dir);
        s.internet_address = false;
        s.reset_public_lookup();
        // Its settings may have changed while it was open.
        s.modes.clear();
        s.tab = Tab::Overview;
        s.config = None;
        s.backups.clear();
        s.service = None;
        s.update = None;
        s.pvp = None;
        s.problem = None;
        s.note = None;
        s.confirm_restore = None;
        s.confirm_delete = false;
        s.texts.remove(&Field::DeleteServer);
        s.texts.remove(&Field::Motd);
        s.texts.remove(&Field::Port);
        s.invalid = None;
        s.polled = None;
        self.load_details();
    }

    /// Called with the launcher's other work: what the background sent, and the next status poll.
    pub(super) fn servers_tick(&mut self) -> bool {
        // The Hosting page shows how friends reach the server beside it, whatever the tab.
        let showing = self.ui.page == crate::frame::Page::Hosting;
        let overview = showing && self.servers.open.is_some() && self.servers.tab == Tab::Overview;
        let mut changed = self.internet_tick(overview);
        if !overview && self.servers.public_lookup != PublicLookup::Idle {
            self.servers.reset_public_lookup();
            changed = true;
        }
        while let Ok(event) = self.servers.rx.try_recv() {
            changed |= self.servers_event(event);
        }
        if !showing || self.servers.polling || !self.servers.loaded {
            return changed;
        }
        let every = if self.servers.open.is_some() {
            POLL_OPEN
        } else {
            POLL_LIST
        };
        if self.servers.polled.is_some_and(|at| at.elapsed() < every) {
            return changed;
        }
        self.servers.polled = Some(Instant::now());
        self.servers.polling = true;
        let tx = self.servers.tx.clone();
        match self.servers.open.clone() {
            Some(dir) => {
                let since = if self.servers.log_dir.as_ref() == Some(&dir) {
                    self.servers.log_next
                } else {
                    0
                };
                let previous = self.servers.statuses.get(&dir).cloned();
                std::thread::spawn(move || {
                    let (status, logs) = servers::poll_hosting(&dir, previous.as_ref(), since);
                    let _ = tx.send(Event::Polled {
                        dir,
                        status: Box::new(status),
                        logs,
                    });
                });
            }
            None => {
                let dirs: Vec<PathBuf> = self.servers.list.iter().map(|e| e.dir.clone()).collect();
                std::thread::spawn(move || {
                    let all = dirs
                        .into_iter()
                        .map(|d| {
                            let s = servers::status(&d);
                            (d, s)
                        })
                        .collect();
                    let _ = tx.send(Event::Statuses(all));
                });
            }
        }
        changed
    }

    fn servers_event(&mut self, event: Event) -> bool {
        let s = &mut self.servers;
        match event {
            Event::PublicAddress { request, result } => s.finish_public_lookup(request, result),
            Event::Polled { dir, status, logs } => {
                s.polling = false;
                let mut changed = false;
                if let Some((next, lines)) = logs {
                    if s.log_dir.as_ref() != Some(&dir) {
                        s.log.clear();
                        s.log_dir = Some(dir.clone());
                        changed = true;
                    }
                    if !lines.is_empty() {
                        s.log.extend(lines);
                        while s.log.len() > CONSOLE_KEEP {
                            s.log.pop_front();
                        }
                        s.console_scroll.handle.scroll_to_bottom();
                        changed = true;
                    }
                    s.log_next = next;
                }
                let running = status.phase == "running";
                changed |= s.note_status(dir.clone(), *status);
                if running && s.open.as_ref() == Some(&dir) && s.pvp.is_none() {
                    self.read_pvp();
                    changed = true;
                }
                changed
            }
            Event::Statuses(all) => {
                s.polling = false;
                let mut changed = false;
                for (dir, status) in all {
                    if s.open.as_ref() != Some(&dir) {
                        changed |= s.note_status(dir, status);
                    }
                }
                changed
            }
            Event::Line(line) => {
                s.job_log.push_back(line);
                while s.job_log.len() > 6 {
                    s.job_log.pop_front();
                }
                true
            }
            Event::Done(result) => {
                s.finish_job(result);
                self.load_details();
                true
            }
            Event::Created(result) => {
                s.creating = false;
                match result {
                    Ok(entry) => {
                        s.create = None;
                        s.list = servers::list();
                        s.modes.clear();
                        self.open_server(entry.dir.clone());
                        self.servers.note = Some(format!(
                            "{} is ready. Press Start, then give your friends the address below.",
                            entry.name
                        ));
                    }
                    Err(p) => s.problem = Some(p),
                }
                true
            }
            Event::Details {
                dir,
                request,
                config,
                backups,
                service,
            } => {
                if s.open.as_ref() == Some(&dir) && s.details_request == request {
                    s.config = config;
                    let pending_for_dir = s.applying.as_ref().is_some_and(|b| b.dir == dir)
                        || s.queued.iter().any(|b| b.dir == dir);
                    if !pending_for_dir {
                        if let Some(outcome) = s.settings_outcomes.get_mut(&dir) {
                            outcome.saved.clear();
                        }
                    }
                    s.backups = backups;
                    s.service = Some(service);
                    true
                } else {
                    false
                }
            }
            Event::Deleted { dir, result } => {
                s.busy = None;
                s.confirm_delete = false;
                s.list = servers::list();
                s.modes.clear();
                s.statuses.remove(&dir);
                STARTED_HERE
                    .lock()
                    .unwrap()
                    .retain(|started| started != &dir);
                if !s.list.iter().any(|entry| entry.dir == dir) {
                    s.settings_outcomes.remove(&dir);
                    s.open = None;
                    s.reset_public_lookup();
                    self.side_servers = s.list.clone();
                }
                match result {
                    Ok(()) => s.note = Some("Server moved to the trash.".into()),
                    Err(e) => s.problem = Some(Problem::plain(e)),
                }
                true
            }
            Event::Applied { dir, result } => {
                if result.is_ok()
                    && s.open.as_ref() == Some(&dir)
                    && s.applying.as_ref().is_some_and(SettingsBatch::changes_port)
                {
                    s.details_request += 1;
                    if let Some(server) = s
                        .config
                        .as_mut()
                        .and_then(|c| c.get_mut("server")?.as_object_mut())
                    {
                        server.remove("port");
                    }
                }
                let refresh = s.finish_settings(&dir, result);
                self.apply_settings();
                if refresh {
                    self.load_details();
                }
                true
            }
            Event::Pvp(on) => {
                s.pvp = on;
                true
            }
            Event::UpdateCheck(result) => {
                s.checking = false;
                match result {
                    Ok(v) => s.update = Some(v),
                    Err(p) => s.problem = Some(p),
                }
                true
            }
            Event::Failed(e) => {
                s.problem = Some(Problem::plain(e));
                true
            }
        }
    }

    fn find_server_public_address(&mut self) {
        if self.ui.page != crate::frame::Page::Hosting
            || self.servers.open.is_none()
            || self.servers.tab != Tab::Overview
        {
            return;
        }
        let Some(request) = self.servers.begin_public_lookup() else {
            return;
        };
        let tx = self.servers.tx.clone();
        std::thread::spawn(move || {
            let result = servers::network::public_ipv4();
            let _ = tx.send(Event::PublicAddress { request, result });
        });
    }

    /// The `pvp` game rule, asked of the running game (it's a rule of the world, not a setting).
    fn read_pvp(&self) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        let tx = self.servers.tx.clone();
        std::thread::spawn(move || {
            let on = servers::Api::open(&dir)
                .and_then(|a| a.command("gamerule pvp").ok())
                .and_then(|out| {
                    let text = out.join(" ");
                    let text = text.trim_end();
                    if text.ends_with("true") {
                        Some(true)
                    } else if text.ends_with("false") {
                        Some(false)
                    } else {
                        None
                    }
                });
            let _ = tx.send(Event::Pvp(on));
        });
    }

    /// Queues a setting for `sunrise-server config set`; they go one batch at a time.
    fn set_setting(&mut self, key: &str, value: String) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        self.servers.queue_setting(dir, key, value);
        self.servers.note = None;
        self.apply_settings();
    }

    fn apply_settings(&mut self) {
        let Some(SettingsBatch { dir, changes }) = self.servers.next_settings_batch() else {
            return;
        };
        let tx = self.servers.tx.clone();
        std::thread::spawn(move || {
            let pairs: Vec<_> = changes
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect();
            let result = servers::set_config(&dir, &pairs);
            let _ = tx.send(Event::Applied { dir, result });
        });
    }

    /// A command for the running game, through the control API; its answer shows in the console.
    fn api_command(&mut self, line: String) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        let tx = self.servers.tx.clone();
        self.servers.log.push_back(format!("> {line}"));
        self.servers.console_scroll.handle.scroll_to_bottom();
        std::thread::spawn(move || {
            let result = servers::Api::open(&dir)
                .ok_or_else(|| "The server isn't running.".to_string())
                .and_then(|a| a.command(&line));
            if let Err(e) = result {
                let _ = tx.send(Event::Failed(e));
            }
        });
    }

    fn start_server(&mut self) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        STARTED_HERE.lock().unwrap().push(dir.clone());
        if let Some(outcome) = self.servers.settings_outcomes.get_mut(&dir) {
            outcome.restart.clear();
        }
        let tx = self.tx.clone();
        self.servers.start_job("Starting", move |log| {
            if let Some(api) = servers::Api::open(&dir) {
                if let Ok(status) = api.call("GET", "/api/v1/status", None) {
                    if status["phase"] == "stopped"
                        && api.call("POST", "/api/v1/start", None).is_ok()
                    {
                        return Ok(String::new());
                    }
                }
            }
            log("A new world takes a minute the first time".into());
            run_managed_server(&tx, &dir, &["start"], log).map(|_| String::new())
        });
    }

    fn stop_server(&mut self) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        self.servers.start_job("Stopping", move |log| {
            // The running server's API first; when it can't be reached or refuses, the CLI,
            // whose error is the job's.
            let via_api = servers::Api::open(&dir)
                .is_some_and(|api| api.call("POST", "/api/v1/stop", None).is_ok());
            if via_api {
                return Ok(String::new());
            }
            servers::run_streamed(&dir, &["stop"], log).map(|_| String::new())
        });
    }

    fn restart_server(&mut self) {
        let Some(dir) = self.servers.open.clone() else {
            return;
        };
        if let Some(outcome) = self.servers.settings_outcomes.get_mut(&dir) {
            outcome.restart.clear();
        }
        let tx = self.tx.clone();
        self.servers.start_job("Restarting", move |log| {
            // The running server's API first; when it can't be reached or refuses, the CLI,
            // whose error is the job's.
            let via_api = servers::Api::open(&dir)
                .is_some_and(|api| api.call("POST", "/api/v1/restart", None).is_ok());
            if via_api {
                return Ok(String::new());
            }
            run_managed_server(&tx, &dir, &["restart"], log).map(|_| String::new())
        });
    }

    fn fix(&mut self, fix: Fix, cx: &mut Context<Self>) {
        self.servers.problem = None;
        match fix {
            Fix::UsePort(port) => {
                self.set_setting("server.port", port.to_string());
                self.servers.texts.remove(&Field::Port);
                self.servers.note = Some(format!(
                    "The server now uses port {port}. Press Start again."
                ));
            }
            Fix::Prepare => match self.selected().cloned() {
                Some(tag) if backend::is_prepared(&tag) => {
                    self.servers
                        .start_job("Copying the game files", move |log| {
                            log(format!("Copying the game files prepared for Sunrise {tag}"));
                            servers::set_up(&tag)
                                .map(|_| "The game files are ready. Press Start again.".to_string())
                                .map_err(Problem::plain)
                        });
                }
                _ => {
                    self.servers.create = None;
                    self.go(Page::Play);
                    if !self.preparing {
                        self.prepare(backend::MinecraftFrom::Install);
                    }
                }
            },
            Fix::AgreeEula => {
                if let Some(c) = &mut self.servers.create {
                    c.eula = true;
                    c.eula_missing = false;
                } else if let Some(dir) = &self.servers.open {
                    if let Err(e) = backend::agree_eula(dir) {
                        self.servers.problem = Some(Problem::plain(e));
                    }
                }
            }
            Fix::ShowLog => {
                self.servers.tab = Tab::Overview;
                if let Some(dir) = &self.servers.open {
                    crate::glass_toast::open(cx, &dir.join("logs"));
                }
            }
        }
    }

    // ----- Creating -------------------------------------------------------------------------

    fn open_create(&mut self, cx: &mut Context<Self>) {
        self.focus_handle(Field::Name, cx);
        self.servers.focus_soon = Some(Field::Name);
        let worlds = servers::player_worlds(&backend::game_home());
        let name = if self.servers.list.is_empty() {
            "My server".to_string()
        } else {
            format!("Server {}", self.servers.list.len() + 1)
        };
        // The glass dialog starts with an empty name (its placeholder says the default) and the
        // player as the first friend.
        let glass = self.theme.glass;
        let own = own_name(&self.accounts).unwrap_or_default();
        self.servers
            .text(Field::Name)
            .set(if glass { String::new() } else { name });
        self.servers.selected = (!glass).then_some(Field::Name);
        self.servers.text(Field::Seed).set(String::new());
        self.servers
            .text(Field::Players)
            .set(if glass { String::new() } else { own.clone() });
        let friends = if glass && !own.is_empty() {
            vec![own]
        } else {
            Vec::new()
        };
        // Nothing to run it with yet: said as the form opens, not after it is filled in.
        self.servers.problem = servers::create_blocked(self.selected().map(String::as_str));
        self.servers.create_scroll = Scrollbar::default();
        self.servers.create = Some(Create {
            new_world: true,
            copy: worlds.first().map(|w| w.1.clone()),
            worlds,
            mode: "survival",
            difficulty: "normal",
            max_players: 10,
            friends_only: true,
            check_accounts: true,
            eula: false,
            eula_missing: false,
            friends,
        });
    }

    fn create_server(&mut self) {
        if self.servers.creating {
            return;
        }
        let Some(c) = &mut self.servers.create else {
            return;
        };
        if !c.eula {
            c.eula_missing = true;
            return;
        }
        let Some(c) = self.servers.create.as_ref() else {
            return;
        };
        let world = match (&c.copy, c.new_world) {
            (Some(dir), false) => World::Copy(dir.clone()),
            _ => World::New {
                seed: self.servers.value(Field::Seed).to_string(),
            },
        };
        let mut players = c.friends.clone();
        for p in self.servers.value(Field::Players).split([',', ' ']) {
            if !p.is_empty() && !players.iter().any(|q| q == p) {
                players.push(p.to_string());
            }
        }
        let name = self.servers.value(Field::Name).trim().to_string();
        let spec = NewServer {
            name: if !name.is_empty() {
                name
            } else if self.servers.list.is_empty() {
                "My server".into()
            } else {
                format!("Server {}", self.servers.list.len() + 1)
            },
            world,
            game_mode: c.mode.into(),
            difficulty: c.difficulty.into(),
            max_players: c.max_players,
            friends_only: c.friends_only,
            check_accounts: c.check_accounts,
            players,
            eula: c.eula,
        };
        let tag = self.selected().cloned();
        let tx = self.servers.tx.clone();
        self.servers.creating = true;
        self.servers.problem = None;
        self.servers.job_log.clear();
        std::thread::spawn(move || {
            let lines = tx.clone();
            let log = move |l: String| {
                let _ = lines.send(Event::Line(l));
            };
            let _ = tx.send(Event::Created(servers::create(&spec, tag.as_deref(), &log)));
        });
    }

    // ----- Building blocks ------------------------------------------------------------------

    pub(super) fn focus_handle(&mut self, f: Field, cx: &mut Context<Self>) -> FocusHandle {
        self.servers
            .focus
            .entry(f)
            .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true))
            .clone()
    }

    /// A one-line text field: typing, paste, Backspace, Enter to confirm, Escape, Tab to the next.
    pub(super) fn field(
        &mut self,
        f: Field,
        placeholder: &'static str,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let handle = self.focus_handle(f, cx);
        let focused = self.servers.focused == Some(f);
        let selected = focused && self.servers.selected == Some(f);
        let invalid = self.servers.invalid.is_some_and(|(g, _)| g == f);
        self.servers.text(f).show(((width - 24.) / 7.5) as usize);
        // Home's Add server line is in the glass look already; the other fields follow with
        // their pages.
        // The dark appearance draws every field in the glass look.
        let profile_field = matches!(f, Field::InstanceName | Field::InstanceGroup);
        let glass = t.glass;
        let base = if profile_field {
            crate::frame::input(focused)
                .when(invalid, |d| {
                    d.border_color(gpui::rgb(crate::paint::palette().danger))
                })
                .h(px(crate::tokens::FIELD))
                .px(px(12.))
                .text_size(px(crate::tokens::BODY))
                .text_color(gpui::rgb(crate::paint::palette().text))
        } else if glass {
            // The mods filter and the log search sit by buttons: the mockup's small `FIELD`, as
            // tall as they are.
            crate::glass_ui::field(focused, invalid)
                .when(
                    matches!(
                        f,
                        Field::Motd
                            | Field::Port
                            | Field::Token
                            | Field::AddPlayer
                            | Field::DeleteServer
                    ),
                    |d| {
                        d.h(px(36.))
                            .px(px(12.))
                            .rounded(px(crate::glass_ui::rad(radius::CONTROL)))
                    },
                )
                // The server creation dialog's are the large `FIELD_LG`: 48 px, 15 px text, white
                // at 6 %, 10 % while typed in.
                .when(!invalid && matches!(f, Field::Name | Field::Seed), |d| {
                    d.h(px(48.))
                        .text_size(px(15.))
                        .bg(crate::glass_ui::white(if focused { 0.1 } else { 0.06 }))
                })
                // The accent's hex: `h-9 w-[96px] rounded-[10px] px-3 text-[13px] font-medium`.
                .when(f == Field::AccentHex, |d| {
                    d.h(px(36.))
                        .px(px(12.))
                        .rounded(px(crate::glass_ui::rad(radius::CONTROL)))
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                })
                // The friend field sits in its chip, on the chip's fill.
                .when(matches!(f, Field::Players), |d| {
                    d.h_full().px(px(14.)).bg(gpui::transparent_black())
                })
                // The server's command line: the field is the line's middle, on its fill, in mono.
                .when(f == Field::Command, |d| {
                    d.h_full()
                        .px_0()
                        .rounded_none()
                        .bg(gpui::transparent_black())
                        .font_family(theme::MONO)
                        .text_size(px(13.))
                })
        } else {
            div()
                .flex()
                .items_center()
                .h(px(theme::FIELD))
                .px(px(12.))
                .overflow_hidden()
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(if invalid {
                    t.danger
                } else if focused {
                    t.accent
                } else {
                    t.rule
                })
                .bg(t.surface)
                .text_size(px(theme::UI.px))
        };
        let line_theme = if profile_field {
            crate::mods::field_theme(t)
        } else {
            t
        };
        let field = base
            .id(SharedString::from(format!("field-{f:?}")))
            .track_focus(&handle)
            .seen()
            .w(px(width))
            .cursor_text()
            .child(line_edit::content(
                self.servers.text(f),
                &line_theme,
                focused,
                selected,
                f == Field::Token,
                placeholder,
            ))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                // A click puts the caret in: the suggestion is no longer selected.
                if matches!(ev, ClickEvent::Mouse(_)) {
                    this.servers.selected = None;
                }
                if let Some(h) = this.servers.focus.get(&f) {
                    window.focus(h, cx);
                }
                cx.notify();
            }))
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key != "enter" && this.servers.invalid.is_some_and(|(g, _)| g == f)
                {
                    this.servers.invalid = None;
                }
                // A suggestion is selected: what's typed or pasted replaces it.
                let selected = this.servers.selected.take() == Some(f);
                if selected {
                    this.servers.text(f).select_all();
                }
                match line_edit::handle(this.servers.text(f), ev, cx) {
                    Key::Enter if matches!(f, Field::InstanceName | Field::InstanceGroup) => {
                        this.save_instance(cx)
                    }
                    Key::Enter => this.field_enter(f),
                    Key::Escape => {
                        this.servers.confirm_delete = false;
                        if this.servers.create.is_some() && !this.servers.creating {
                            this.servers.create = None;
                        }
                        this.instance_escape();
                        window.blur(cx);
                    }
                    // Tab and the rest go on to the window: Tab moves the focus.
                    Key::Pass => return,
                    _ => {}
                }
                cx.stop_propagation();
                cx.notify();
            }));
        line_edit::mouse(field, cx, move |this| this.servers.text(f))
    }

    fn field_enter(&mut self, f: Field) {
        match f {
            // The glass dialog's friend field adds the name typed.
            Field::Players if self.theme.glass => self.add_create_friend(),
            Field::Name | Field::Seed | Field::Players => self.create_server(),
            Field::Command => {
                let line = self.servers.text(Field::Command).take();
                let line = line.trim().trim_start_matches('/').to_string();
                if !line.is_empty() {
                    self.api_command(line);
                }
            }
            Field::AddPlayer => self.add_player(),
            Field::Motd => {
                let motd = self.servers.value(Field::Motd).trim().to_string();
                if motd.is_empty() {
                    self.servers.problem =
                        Some(Problem::plain("Type the line players see under the name."));
                } else {
                    self.set_setting("server.motd", motd);
                }
            }
            Field::Port => self.save_port(),
            Field::Token => self.save_token(),
            Field::Email => self.invite_friend(),
            Field::InstanceName | Field::InstanceGroup | Field::DeleteServer | Field::AccentHex => {
            }
            Field::PreLaunch | Field::Wrapper | Field::LaunchEnv | Field::PostExit => {
                self.save_launch()
            }
            Field::ServerName | Field::ServerAddress => self.add_server(),
        }
    }

    fn save_port(&mut self) {
        match self.servers.value(Field::Port).trim().parse::<u16>() {
            Ok(port) if port >= 1024 => self.set_setting("server.port", port.to_string()),
            _ => {
                self.servers.invalid = Some((
                    Field::Port,
                    "Use a number from 1024 to 65535 (25565 is Minecraft's own).",
                ))
            }
        }
    }

    fn add_player(&mut self) {
        let name = self.servers.value(Field::AddPlayer).trim().to_string();
        let valid = (3..=16).contains(&name.len())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            self.servers.invalid = Some((
                Field::AddPlayer,
                "A Minecraft name has 3 to 16 letters, digits or underscores.",
            ));
            return;
        }
        self.servers.text(Field::AddPlayer).set(String::new());
        self.server_command(
            &format!("Letting {name} in"),
            vec!["whitelist".into(), "add".into(), name],
        );
    }

    /// A player's face, from the default skin their UUID picks (the server knows no other).
    pub(super) fn head(&mut self, uuid: &str, size: f32) -> Div {
        let resources = servers::game_files(&servers::runtime_root(), "26.3");
        let face = self
            .servers
            .faces
            .entry(uuid.to_string())
            .or_insert_with(|| heads::default_face(&resources, uuid))
            .clone();
        let cell = size / 8.;
        let mut d = div()
            .flex_none()
            .flex()
            .flex_col()
            .size(px(size))
            .rounded(px(if self.theme.glass {
                crate::glass_ui::rad(size / 4.).min(size / 2.)
            } else {
                3.
            }))
            .overflow_hidden()
            .bg(self.theme.plate);
        if let Some(pixels) = face {
            for row in pixels.chunks(8) {
                d = d.child(div().flex().children(row.iter().map(|p| {
                    div().size(px(cell)).bg(Hsla::from(gpui::Rgba {
                        r: p[0] as f32 / 255.,
                        g: p[1] as f32 / 255.,
                        b: p[2] as f32 / 255.,
                        a: 1.,
                    }))
                })));
            }
        }
        d
    }

    // ----- The page -------------------------------------------------------------------------

    fn add_server_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await {
                if let Some(dir) = paths.pop() {
                    let _ = this.update(cx, |this, cx| {
                        this.add_existing(dir);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// A folder with a server.toml joins the list; a vanilla server folder is set up first (its
    /// server.properties becomes server.toml; its world, lists and EULA stay).
    fn add_existing(&mut self, dir: PathBuf) {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Server".into());
        let tag = self.selected().cloned();
        let tx = self.servers.tx.clone();
        self.servers.creating = true;
        self.servers.problem = None;
        std::thread::spawn(move || {
            let result = (|| {
                if servers::program().is_none() {
                    let tag =
                        tag.ok_or_else(|| Problem::plain("Install a Sunrise version first."))?;
                    servers::set_up(&tag).map_err(Problem::plain)?;
                }
                if !dir.join("server.toml").is_file() {
                    servers::run(&dir, &["init", "--yes"])?;
                }
                let entry = Entry {
                    name,
                    dir,
                    host: None,
                };
                servers::add(entry.clone()).map_err(Problem::plain)?;
                Ok(entry)
            })();
            let _ = tx.send(Event::Created(result));
        });
    }

    /// The server's details came back without its settings: its sunrise-server predates
    /// `config` (and `backups --json`).
    fn older_sunrise(&self) -> bool {
        self.servers.service.is_some() && self.servers.config.is_none()
    }

    // ----- The Create dialog ----------------------------------------------------------------
}

/// The state, in a few words.
fn phase_label(status: &Status) -> String {
    if let Some(busy) = &status.update_busy {
        return trf("Updating: {step}", &[("step", busy)]);
    }
    match status.phase.as_str() {
        "running" if status.crashes > 0 => tr("Running · restarted after a crash").into(),
        "running" if status.paused => tr("Running · paused while nobody plays").into(),
        "running" => tr("Running").into(),
        "starting" => tr("Starting").into(),
        "restarting" => tr("Restarting").into(),
        "stopping" => tr("Stopping").into(),
        "unknown" => tr("Server status unavailable").into(),
        _ => tr("Stopped").into(),
    }
}

/// A setting's name for people.
fn setting_name(key: &str) -> &str {
    match key {
        "server.motd" => "the message in the server list",
        "server.port" => "the port",
        "server.max_players" => "most players",
        "server.view_distance" => "view distance",
        "server.simulation_distance" => "simulation distance",
        "server.online_mode" => "account checks",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_uses_the_live_bind_without_saved_settings_and_clears_an_unknown_status() {
        let mut servers = Servers::new();
        let dir = PathBuf::from("server");
        servers.open = Some(dir.clone());
        servers.config = Some(serde_json::json!({"server": {"port": 25567}}));
        let old = "127.0.0.1:25566".parse().unwrap();
        assert_eq!(
            servers.share_address(&servers.status()),
            (None, Some(25567))
        );
        servers.note_status(
            dir.clone(),
            Status {
                phase: "running".into(),
                listener: Some(old),
                ..Status::default()
            },
        );
        assert_eq!(
            servers.share_address(&servers.status()),
            (Some(old), Some(25567))
        );
        servers.config = None;
        assert_eq!(servers.share_address(&servers.status()), (Some(old), None));
        servers.config = Some(serde_json::json!({"server": {"port": 25567}}));
        servers.note_status(dir.clone(), Status::unknown());
        assert_eq!(
            servers.share_address(&servers.status()),
            (None, Some(25567))
        );
        let restarted = "0.0.0.0:25567".parse().unwrap();
        servers.note_status(
            dir.clone(),
            Status {
                phase: "running".into(),
                listener: Some(restarted),
                ..Status::default()
            },
        );
        assert_eq!(
            servers.share_address(&servers.status()),
            (Some(restarted), Some(25567))
        );
        servers.note_status(dir.clone(), Status::stopped());
        assert_eq!(
            servers.share_address(&servers.status()),
            (Some(restarted), Some(25567))
        );
        servers.queue_setting(dir, "server.port", "25568".into());
        assert_eq!(
            servers.share_address(&servers.status()),
            (None, Some(25567))
        );
    }

    #[test]
    fn a_dismissed_lookup_remains_in_flight_and_cannot_enable_copy() {
        let mut servers = Servers::new();
        servers.internet_address = true;
        let first = servers.begin_public_lookup().unwrap();
        servers.reset_public_lookup();
        assert!(servers.begin_public_lookup().is_none());
        let ip = "192.0.2.42".parse().unwrap();
        assert!(servers.finish_public_lookup(first, Ok(ip)));
        assert_eq!(servers.public_lookup, PublicLookup::Idle);
        assert!(servers.last_public_address.is_none());
        let second = servers.begin_public_lookup().unwrap();
        assert!(servers.finish_public_lookup(second, Ok(ip)));
        assert_eq!(servers.public_lookup, PublicLookup::Detected);
        assert_eq!(servers.last_public_address, Some(ip));
    }

    #[test]
    fn a_running_server_is_not_a_launcher_write_but_its_hosting_workers_are() {
        let mut servers = Servers::new();
        servers.statuses.insert(
            PathBuf::from("server"),
            Status {
                phase: "running".into(),
                ..Status::default()
            },
        );
        assert!(servers.location_busy());
        assert!(!servers.writing());
        servers.creating = true;
        assert!(servers.writing());
        servers.creating = false;
        servers.busy = Some("Restoring a backup".into());
        assert!(servers.writing());
        servers.busy = None;
        let server = PathBuf::from("server");
        servers.queue_setting(server.clone(), "server.port", "25567".into());
        assert!(servers.next_settings_batch().is_some());
        assert!(servers.writing());
        servers.finish_settings(&server, Err(Problem::plain("save failed")));
        assert!(!servers.writing());
    }

    #[test]
    fn an_update_waits_for_a_hosting_restore_until_the_worker_reports_completion() {
        use crate::launcher_update::Wait;
        let mut servers = Servers::new();
        servers.statuses.insert(
            PathBuf::from("server"),
            Status {
                phase: "running".into(),
                ..Status::default()
            },
        );
        let (finish, wait_for_finish) = std::sync::mpsc::channel();
        servers.start_job("Restore a backup", move |_| {
            wait_for_finish.recv().unwrap();
            Ok(String::new())
        });
        let ready = SelfUpdate::Ready(Release {
            tag: "v1.3.0".into(),
            name: String::new(),
            body: String::new(),
            date: String::new(),
            time: String::new(),
            published_at: String::new(),
            asset: None,
            prerelease: false,
            nightly: None,
        });
        let restart = |hosting: &Servers| {
            ready
                .ready_to_restart(true, true, || {
                    Wait::for_activity(false, false, hosting, false)
                })
                .is_some()
        };
        assert!(!restart(&servers));
        finish.send(()).unwrap();
        let Event::Done(result) = servers.rx.recv_timeout(Duration::from_secs(10)).unwrap() else {
            panic!("expected the Hosting worker's completion event");
        };
        result.as_ref().unwrap();
        servers.finish_job(result);
        assert!(restart(&servers), "a running server must not block restart");
    }

    #[test]
    fn queued_settings_drain_to_their_own_servers_without_an_open_page() {
        let (one, two) = (PathBuf::from("one"), PathBuf::from("two"));
        let mut servers = Servers::new();
        servers.open = Some(one.clone());
        servers.queue_setting(one.clone(), "server.motd", "saving".into());
        let first = servers.next_settings_batch().unwrap();
        assert_eq!(first.dir, one);
        servers.queue_setting(one.clone(), "server.motd", "old edit".into());
        servers.queue_setting(one.clone(), "server.motd", "latest edit".into());
        servers.open = Some(two.clone());
        assert_eq!(servers.shown("server", "motd"), "");
        servers.queue_setting(two.clone(), "server.motd", "other server".into());
        assert_eq!(servers.shown("server", "motd"), "other server");
        servers.open = None;
        assert!(servers.next_settings_batch().is_none());
        let done = || {
            Ok(Applied {
                applied: vec![],
                restart: vec!["server.motd".into()],
                running: true,
            })
        };
        assert!(!servers.finish_settings(&first.dir, done()));
        assert!(servers.writing());
        for (dir, motd) in [(one, "latest edit"), (two, "other server")] {
            let batch = servers.next_settings_batch().unwrap();
            assert_eq!(batch.dir, dir);
            assert_eq!(
                batch.changes,
                [(String::from("server.motd"), String::from(motd))]
            );
            assert!(servers.writing());
            assert!(!servers.finish_settings(&batch.dir, done()));
        }
        assert!(servers.next_settings_batch().is_none());
        assert!(!servers.writing());
    }

    #[test]
    fn running_settings_remain_visible_on_reopen_without_mutating_the_snapshot() {
        let one = PathBuf::from("one");
        let two = PathBuf::from("two");
        let mut servers = Servers::new();
        let snapshot = serde_json::json!({"world": "not an object"});
        servers.config = Some(snapshot.clone());
        servers.open = Some(one.clone());
        servers.queue_setting(one.clone(), "world.view_distance", "9".into());
        assert!(servers.next_settings_batch().is_some());
        assert_eq!(servers.config.as_ref(), Some(&snapshot));
        servers.open = Some(two);
        assert_eq!(servers.shown("world", "view_distance"), "");
        servers.open = None;
        servers.open = Some(one.clone());
        servers.config = Some(serde_json::json!({"world": {"view_distance": 8}}));
        assert_eq!(servers.shown("world", "view_distance"), "9");
        servers.queue_setting(one, "world.view_distance", "10".into());
        assert_eq!(servers.shown("world", "view_distance"), "10");
    }

    #[test]
    fn settings_completion_keeps_notices_and_mode_invalidation_on_the_written_server() {
        let one = PathBuf::from("one");
        let two = PathBuf::from("two");
        let mut servers = Servers::new();
        let apply = |servers: &mut Servers, result| {
            servers.open = Some(one.clone());
            servers.queue_setting(one.clone(), "server.port", "25577".into());
            let batch = servers.next_settings_batch().unwrap();
            servers.open = Some(two.clone());
            assert!(!servers.finish_settings(&batch.dir, result));
            assert!(!servers.settings_outcomes.contains_key(&two));
            assert!(servers.problem.is_none());
        };
        servers.modes.insert(one.clone(), Some("survival".into()));
        servers.modes.insert(two.clone(), Some("creative".into()));
        apply(
            &mut servers,
            Ok(Applied {
                applied: vec![],
                restart: vec!["server.port".into()],
                running: true,
            }),
        );
        assert!(!servers.modes.contains_key(&one));
        assert!(servers.modes.contains_key(&two));
        apply(
            &mut servers,
            Err(Problem {
                message: "port is unavailable".into(),
                fix: Some(Fix::UsePort(25578)),
            }),
        );
        servers.open = Some(one.clone());
        let outcome = &servers.settings_outcomes[&one];
        assert!(outcome.restart.contains("server.port"));
        let problem = outcome.problem.as_ref().unwrap();
        assert_eq!(problem.message, "port is unavailable");
        assert!(matches!(problem.fix.as_ref(), Some(Fix::UsePort(25578))));
        servers.dismiss_problem(Some(&one));
        assert!(servers.settings_outcomes[&one].problem.is_none());
    }

    #[test]
    fn saved_value_stays_visible_while_another_batch_is_queued_without_mutating_snapshot() {
        let dir = PathBuf::from("one");
        let mut servers = Servers::new();
        let snapshot = serde_json::json!({"world": {"view_distance": 8, "difficulty": "normal"}});
        servers.open = Some(dir.clone());
        servers.config = Some(snapshot.clone());

        // Start saving view_distance 8 -> 9, then queue an unrelated difficulty edit.
        servers.queue_setting(dir.clone(), "world.view_distance", "9".into());
        let first = servers.next_settings_batch().unwrap();
        servers.queue_setting(dir.clone(), "world.difficulty", "hard".into());

        // First batch finishes while the second is still queued: snapshot stays untouched,
        // yet view_distance still shows 9 so a follow-up `+` submits 10 rather than 9.
        assert!(!servers.finish_settings(
            &first.dir,
            Ok(Applied {
                applied: vec!["world.view_distance".into()],
                restart: vec![],
                running: false,
            })
        ));
        assert_eq!(servers.config.as_ref(), Some(&snapshot));
        assert_eq!(servers.shown("world", "view_distance"), "9");
        assert_eq!(servers.shown("world", "difficulty"), "hard");

        // A newer queued edit to view_distance sits on top of the saved value.
        servers.queue_setting(dir.clone(), "world.view_distance", "10".into());
        assert_eq!(servers.shown("world", "view_distance"), "10");
    }

    #[test]
    fn unrelated_successful_save_keeps_earlier_failed_save_error_until_fixed_or_dismissed() {
        let one = PathBuf::from("one");
        let two = PathBuf::from("two");
        let mut servers = Servers::new();
        servers.open = Some(one.clone());

        // Port save fails while difficulty save is queued on the same server.
        servers.queue_setting(one.clone(), "server.port", "25577".into());
        let port_batch = servers.next_settings_batch().unwrap();
        servers.queue_setting(one.clone(), "world.difficulty", "hard".into());
        servers.open = Some(two);
        assert!(
            !servers.finish_settings(&port_batch.dir, Err(Problem::plain("port is unavailable")))
        );

        // Unrelated difficulty save succeeds: the port error stays visible.
        let diff_batch = servers.next_settings_batch().unwrap();
        assert!(!servers.finish_settings(
            &diff_batch.dir,
            Ok(Applied {
                applied: vec!["world.difficulty".into()],
                restart: vec![],
                running: false,
            })
        ));
        servers.open = Some(one.clone());
        assert_eq!(
            servers.settings_outcomes[&one]
                .problem
                .as_ref()
                .map(|p| p.message.as_str()),
            Some("port is unavailable")
        );

        // Saving server.port successfully clears the port error.
        servers.queue_setting(one.clone(), "server.port", "25578".into());
        let retry_port = servers.next_settings_batch().unwrap();
        assert!(servers.finish_settings(
            &retry_port.dir,
            Ok(Applied {
                applied: vec!["server.port".into()],
                restart: vec![],
                running: false,
            })
        ));
        assert!(servers.settings_outcomes[&one].problem.is_none());
    }

    #[test]
    fn late_running_poll_after_stale_stopped_status_keeps_restart_notice_until_real_restart() {
        let dir = PathBuf::from("one");
        let mut servers = Servers::new();
        servers.open = Some(dir.clone());
        let status_with = |phase: &str| Status {
            phase: phase.into(),
            ..Status::unknown()
        };

        // Cached status is still stale "stopped" when a port save reports running=true.
        servers.note_status(dir.clone(), status_with("stopped"));
        servers.queue_setting(dir.clone(), "server.port", "25577".into());
        let batch = servers.next_settings_batch().unwrap();
        assert!(servers.finish_settings(
            &batch.dir,
            Ok(Applied {
                applied: vec![],
                restart: vec!["server.port".into()],
                running: true,
            })
        ));
        assert!(servers.settings_outcomes[&dir]
            .restart
            .contains("server.port"));

        // The next poll catches up to "running" for the same game: restart notice must stay.
        assert!(servers.note_status(dir.clone(), status_with("running")));
        assert!(
            servers.settings_outcomes[&dir]
                .restart
                .contains("server.port"),
            "catch-up running poll must not clear restart notice"
        );

        // Once the server actually stops and starts again, the notice clears.
        assert!(servers.note_status(dir.clone(), status_with("stopped")));
        assert!(servers.settings_outcomes[&dir]
            .restart
            .contains("server.port"));
        assert!(servers.note_status(dir.clone(), status_with("running")));
        assert!(servers.settings_outcomes[&dir].restart.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn managed_start_and_restart_require_consent_before_running_the_supervisor() {
        const CHILD: &str = "SUNRISE_MANAGED_CONSENT_TEST";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "servers_page::tests::managed_start_and_restart_require_consent_before_running_the_supervisor", "--nocapture"])
                .env(CHILD, "1")
                .env("SUNRISE_LAUNCHER_HOME", root.path().join("launcher"))
                .env("SUNRISE_HOME", root.path().join("cache"))
                .status().unwrap();
            assert!(status.success());
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let bin = servers::runtime_root().join("current");
        std::fs::create_dir_all(&bin).unwrap();
        let supervisor = bin.join("sunrise-server");
        let engine = bin.join("sunrise-engine");
        // Answers `engine-command --json` as the real supervisor does; anything else would start the server.
        let reply = serde_json::json!({
            "format": 1,
            "program": engine,
            "args": ["--resources", root.path(), "--control-socket", root.path().join(".sunrise/engine.json"), "--dedicated", "--nogui"],
            "cwd": root.path(),
            "env": {"SUNRISE_NO_AUDIO": "1"},
            "resources": root.path(),
        });
        std::fs::write(
            &supervisor,
            format!("#!/bin/sh\nif [ \"$3\" = engine-command ]; then printf '%s' '{reply}'; exit 0; fi\nprintf ran > \"$2/ran\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&supervisor, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&engine, "never executed").unwrap();
        std::fs::write(root.path().join("sunrise-resources.json"), "{}").unwrap();
        let config = format!(
            "[engine]\nresources = {:?}\n",
            root.path().to_str().unwrap()
        );
        std::fs::write(root.path().join("server.toml"), config).unwrap();
        let package = root
            .path()
            .join(sunrise_launcher_smod::mod_list::DIR)
            .join("needs_jar");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("sunrise-mod.toml"), format!(
            "id='needs_jar'\nname='Needs jar'\nversion='1'\napi='0.2'\ncapabilities=['registries']\n[[jar]]\nid='fixture'\nversion='1'\nsha512='{}'\n", "a".repeat(128)
        )).unwrap();
        std::fs::write(
            root.path().join(sunrise_launcher_smod::mod_list::FILE),
            "mod.needs_jar:true\n",
        )
        .unwrap();
        for action in ["start", "restart"] {
            let (tx, rx) = std::sync::mpsc::channel();
            let dir = root.path().to_path_buf();
            let job = std::thread::spawn(move || run_managed_server(&tx, &dir, &[action], &|_| {}));
            let message = rx.recv_timeout(Duration::from_secs(10));
            let backend::Msg::JarConsent { jars, reply } = message.unwrap() else {
                panic!("expected jar consent")
            };
            assert_eq!(jars.len(), 1);
            assert_eq!(jars[0].id(), "fixture");
            reply.send(false).unwrap();
            assert!(job.join().unwrap().is_err());
            assert!(!root.path().join("ran").exists());
        }
    }

    #[test]
    fn backups_read_for_people() {
        let b = |name: &str| Backup {
            name: name.into(),
            bytes: 1,
            modified: None,
        };
        assert_eq!(
            backup_label(&b("2026-10-19_11-28-57-manual.zip")),
            ("2026-10-19 11:28:57 UTC".into(), "Made by hand".into())
        );
        assert_eq!(
            backup_label(&b("2026-10-19_11-28-57-before-v2026.10.20.zip")).1,
            "Before updating to v2026.10.20"
        );
        assert_eq!(
            backup_label(&b("2026-10-19_13-35-36-before-v2026-10-19-10.zip")).1,
            "Before updating to v2026.10.19.10"
        );
        assert_eq!(
            backup_label(&b("2026-10-14_17-46-40-before-update.zip")).1,
            "Before an update"
        );
        assert_eq!(
            backup_label(&b("2026-10-19_11-28-57-auto.zip")).1,
            "Automatic"
        );
        assert_eq!(backup_label(&b("odd.zip")), ("odd".into(), String::new()));
        assert_eq!(bytes(5_292_542), "5.3 MB");
        assert_eq!(uptime(3725), "1 h 02 min");
    }
}
