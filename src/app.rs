//! Application state and navigation. Pure logic: input and network results
//! come in as messages; requests to fetch, open, or sign in go out as effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::cache::{ResourceKey, SnapshotStore};
use crate::canvas::{CanvasError, FetchStats};
use crate::html::{Link, Target};
use crate::library::{self, Library, LocalState};
use crate::model::{AssignmentDetail, AssignmentSummary, CanvasFile, CanvasFolder, Course};
use crate::submission::{self, FileChoice};

/// An in-flight read that started at most this long before an access may
/// satisfy that access (approved freshness rule). Older ones may not.
pub const REUSE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Saved copy from an earlier session, shown while syncing.
    Cache,
    /// Returned by a request made in this session.
    Network,
}

#[derive(Clone, Debug)]
pub enum SyncState {
    Idle,
    Syncing { request: u64, started: Instant },
    Failed(CanvasError),
}

#[derive(Clone, Debug)]
pub struct Resource<T> {
    pub data: Option<T>,
    pub origin: Origin,
    pub fetched_at: Option<DateTime<Utc>>,
    pub sync: SyncState,
    /// Bumped whenever `data` changes, for render caches.
    pub version: u64,
}

impl<T> Default for Resource<T> {
    fn default() -> Self {
        Resource {
            data: None,
            origin: Origin::Cache,
            fetched_at: None,
            sync: SyncState::Idle,
            version: 0,
        }
    }
}

#[derive(Debug)]
pub enum Payload {
    Courses(Vec<Course>),
    Announcements(Vec<crate::announcements::Summary>),
    Announcement(Box<crate::announcements::Detail>),
    Assignments(Vec<AssignmentSummary>),
    Detail(Box<AssignmentDetail>),
    History(crate::submission::History),
    Files(Box<CourseFilesData>),
    File(Box<CanvasFile>),
}

/// A course's Files area as the reader keeps it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CourseFilesData {
    pub folders: Vec<CanvasFolder>,
    /// `None` when the instructor hides Files from students.
    pub files: Option<Vec<CanvasFile>>,
}

/// One row of the Files screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FileRow<'a> {
    Folder(&'a CanvasFolder),
    File(&'a CanvasFile),
}

impl FileRow<'_> {
    pub fn name(&self) -> &str {
        match self {
            FileRow::Folder(f) => f.name.as_deref().unwrap_or("folder"),
            FileRow::File(f) => f.name(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferEvent {
    Progress { received: u64, total: Option<u64> },
    Done(PathBuf),
    Failed { message: String, needs_login: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    /// Waiting for a download slot.
    Queued {
        total: Option<u64>,
    },
    Running {
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
}

/// A file as the reader sees it locally.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    Missing,
    Saved,
    /// Canvas has a newer version than the saved copy.
    Changed,
    Queued,
    Running {
        received: u64,
        total: Option<u64>,
    },
    Failed(String),
    Locked,
}

/// A request to put a file in a folder, waiting for the file's details.
#[derive(Clone, Debug, PartialEq)]
struct Intent {
    dir: PathBuf,
    open: bool,
    /// Downloaded without being asked (assignment images): removed after 30 days unused.
    automatic: bool,
    /// Part of a batch or automatic: no notice when it finishes.
    quiet: bool,
}

/// Where a running download will appear, and what to do when it finishes.
#[derive(Clone, Debug, PartialEq)]
struct Placement {
    target: PathBuf,
    open: bool,
    automatic: bool,
    quiet: bool,
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Fetched {
        key: ResourceKey,
        request: u64,
        result: Result<(Payload, FetchStats), CanvasError>,
        at: DateTime<Utc>,
    },
    Tick,
    Resize(u16, u16),
    LoginFinished {
        ok: bool,
        message: String,
    },
    Transfer {
        file_id: String,
        event: TransferEvent,
    },
    Submitted {
        course_id: String,
        assignment_id: String,
        result: Result<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Fetch {
        key: ResourceKey,
        request: u64,
    },
    OpenUrl(String),
    Login,
    /// Download a file into the library at `target` (and install it there).
    Download {
        file: Box<CanvasFile>,
        target: PathBuf,
        automatic: bool,
    },
    /// Open a local file in its default app.
    OpenPath(PathBuf),
    /// Show a local folder in the file manager (created if missing).
    Reveal(PathBuf),
    Submit {
        course_id: String,
        assignment_id: String,
        files: Vec<FileChoice>,
    },
}

#[derive(Clone, Debug)]
pub struct SubmissionDraft {
    pub input: String,
    pub start_directory: String,
    pub suggestions: submission::PathSuggestions,
    pub files: Vec<FileChoice>,
    pub reviewing: bool,
    pub busy: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewKind {
    Courses,
    Announcements {
        course_id: String,
        course_name: String,
    },
    Announcement {
        course_id: String,
        course_name: String,
        announcement_id: String,
        title: String,
    },
    Assignments {
        course_id: String,
        course_name: String,
    },
    Detail {
        course_id: String,
        course_name: String,
        assignment_id: String,
        title: String,
    },
    /// A folder in a course's Files area (`None` = its root).
    Files {
        course_id: String,
        course_name: String,
        folder_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct View {
    pub kind: ViewKind,
    pub selected: usize,
    pub offset: usize,
    pub filter: String,
    pub filtering: bool,
    pub scroll: usize,
    /// Selected entry in an assignment's files & links panel.
    pub link: Option<usize>,
}

impl View {
    fn new(kind: ViewKind) -> Self {
        View {
            kind,
            selected: 0,
            offset: 0,
            filter: String::new(),
            filtering: false,
            scroll: 0,
            link: None,
        }
    }

    pub fn key(&self) -> ResourceKey {
        match &self.kind {
            ViewKind::Courses => ResourceKey::Courses,
            ViewKind::Announcements { course_id, .. } => {
                ResourceKey::Announcements(course_id.clone())
            }
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => ResourceKey::Announcement(course_id.clone(), announcement_id.clone()),
            ViewKind::Assignments { course_id, .. } => ResourceKey::Assignments(course_id.clone()),
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => ResourceKey::Detail(course_id.clone(), assignment_id.clone()),
            ViewKind::Files { course_id, .. } => ResourceKey::Files(course_id.clone()),
        }
    }
}

/// Wrapped description lines cached per assignment version and width.
pub struct DetailRender {
    pub key: (String, String),
    pub version: u64,
    pub width: u16,
    pub lines: Vec<String>,
}

pub struct HistoryRender {
    pub key: (String, String),
    pub version: u64,
    pub selected: usize,
    pub width: u16,
    pub lines: Vec<String>,
}

pub struct App {
    pub origin: String,
    store: Option<Arc<SnapshotStore>>,
    pub courses: Resource<Vec<Course>>,
    pub announcements: HashMap<String, Resource<Vec<crate::announcements::Summary>>>,
    pub announcement_details: HashMap<(String, String), Resource<crate::announcements::Detail>>,
    pub announcement_render: Option<DetailRender>,
    announcement_reads: std::collections::BTreeSet<(String, String)>,
    pub hidden_courses: std::collections::BTreeSet<String>,
    pub editing_home: bool,
    pub assignments: HashMap<String, Resource<Vec<AssignmentSummary>>>,
    pub details: HashMap<(String, String), Resource<AssignmentDetail>>,
    pub histories: HashMap<(String, String), Resource<submission::History>>,
    pub history_open: bool,
    pub history_selected: usize,
    pub history_scroll: usize,
    pub history_render: Option<HistoryRender>,
    pub stack: Vec<View>,
    next_request: u64,
    pub size: (u16, u16),
    pub notice: Option<String>,
    pub login_required: bool,
    pub last_stats: Option<(&'static str, FetchStats)>,
    pub quit: bool,
    pub dirty: bool,
    pub spinner: usize,
    pub detail_render: Option<DetailRender>,
    /// Request ids whose responses were applied since the runtime last drained this (for timing).
    pub applied: Vec<u64>,
    /// Start sign-in automatically if the first read at startup finds the session expired.
    auto_login: bool,
    pub course_files: HashMap<String, Resource<CourseFilesData>>,
    /// File metadata, read when an assignment that links to the file is opened.
    pub files: HashMap<String, Resource<CanvasFile>>,
    pub transfers: HashMap<String, Transfer>,
    /// Files waiting for their details before they can be placed.
    waiting: HashMap<String, Vec<Intent>>,
    /// Running downloads; the first placement is where the file is being installed.
    pending: HashMap<String, Vec<Placement>>,
    pub library: Option<Arc<Library>>,
    pub submission: Option<SubmissionDraft>,
}

fn matches_filter(text: &str, filter: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(&filter.to_lowercase())
}

/// `1.2 MB`, `340 KB`, `12 B` (1 KB = 1024 B).
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Percent-encode one URL path segment.
fn percent_encode(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl App {
    pub fn new(origin: &str, store: Option<Arc<SnapshotStore>>) -> Self {
        let announcement_reads = store
            .as_ref()
            .map(|s| s.announcement_reads())
            .unwrap_or_default();
        let hidden_courses = store
            .as_ref()
            .map(|s| s.hidden_courses())
            .unwrap_or_default();
        App {
            origin: origin.to_string(),
            store,
            courses: Resource::default(),
            announcements: HashMap::new(),
            announcement_details: HashMap::new(),
            announcement_render: None,
            announcement_reads,
            hidden_courses,
            editing_home: false,
            assignments: HashMap::new(),
            details: HashMap::new(),
            histories: HashMap::new(),
            history_open: false,
            history_selected: 0,
            history_scroll: 0,
            history_render: None,
            stack: vec![View::new(ViewKind::Courses)],
            next_request: 0,
            size: (80, 24),
            notice: None,
            login_required: false,
            last_stats: None,
            quit: false,
            dirty: true,
            spinner: 0,
            detail_render: None,
            applied: Vec::new(),
            auto_login: false,
            course_files: HashMap::new(),
            files: HashMap::new(),
            transfers: HashMap::new(),
            waiting: HashMap::new(),
            pending: HashMap::new(),
            library: None,
            submission: None,
        }
    }

    /// Enter the initial view.
    pub fn start(&mut self) -> Vec<Effect> {
        self.auto_login = true;
        self.enter_current(false)
    }

    pub fn view(&self) -> &View {
        self.stack.last().expect("view stack is never empty")
    }

    fn view_mut(&mut self) -> &mut View {
        self.stack.last_mut().expect("view stack is never empty")
    }

    fn sync_of(&self, key: &ResourceKey) -> Option<&SyncState> {
        match key {
            ResourceKey::Courses => Some(&self.courses.sync),
            ResourceKey::Announcements(c) => self.announcements.get(c).map(|r| &r.sync),
            ResourceKey::Assignments(c) => self.assignments.get(c).map(|r| &r.sync),
            ResourceKey::Announcement(c, a) => self
                .announcement_details
                .get(&(c.clone(), a.clone()))
                .map(|r| &r.sync),
            ResourceKey::Detail(c, a) => self.details.get(&(c.clone(), a.clone())).map(|r| &r.sync),
            ResourceKey::History(c, a) => {
                self.histories.get(&(c.clone(), a.clone())).map(|r| &r.sync)
            }
            ResourceKey::Files(c) => self.course_files.get(c).map(|r| &r.sync),
            ResourceKey::File(f) => self.files.get(f).map(|r| &r.sync),
        }
    }

    fn set_sync(&mut self, key: &ResourceKey, sync: SyncState) {
        match key {
            ResourceKey::Announcements(c) => {
                self.announcements.entry(c.clone()).or_default().sync = sync
            }
            ResourceKey::Announcement(c, a) => {
                self.announcement_details
                    .entry((c.clone(), a.clone()))
                    .or_default()
                    .sync = sync
            }

            ResourceKey::Courses => self.courses.sync = sync,
            ResourceKey::Assignments(c) => {
                self.assignments.entry(c.clone()).or_default().sync = sync
            }
            ResourceKey::Detail(c, a) => {
                self.details.entry((c.clone(), a.clone())).or_default().sync = sync
            }
            ResourceKey::History(c, a) => {
                self.histories
                    .entry((c.clone(), a.clone()))
                    .or_default()
                    .sync = sync
            }
            ResourceKey::Files(c) => self.course_files.entry(c.clone()).or_default().sync = sync,
            ResourceKey::File(f) => self.files.entry(f.clone()).or_default().sync = sync,
        }
    }

    pub fn current_syncing(&self) -> bool {
        let key = if self.history_open {
            if let ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } = &self.view().kind
            {
                ResourceKey::History(course_id.clone(), assignment_id.clone())
            } else {
                self.view().key()
            }
        } else {
            self.view().key()
        };
        matches!(self.sync_of(&key), Some(SyncState::Syncing { .. }))
    }

    /// Show a saved copy (if any and nothing newer is loaded), then request fresh data.
    fn enter_current(&mut self, explicit: bool) -> Vec<Effect> {
        let key = self.view().key();
        self.load_snapshot(&key);
        self.mark_current_announcement_read();
        self.dirty = true;
        self.request(key, explicit).into_iter().collect()
    }

    fn load_snapshot(&mut self, key: &ResourceKey) {
        let Some(store) = self.store.clone() else {
            return;
        };
        match key {
            ResourceKey::Courses if self.courses.data.is_none() => {
                if let Some((data, at)) = store.load(key) {
                    self.courses.data = Some(data);
                    self.courses.fetched_at = Some(at);
                    self.courses.origin = Origin::Cache;
                    self.courses.version += 1;
                }
            }
            ResourceKey::Announcements(c) => {
                let res = self.announcements.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Assignments(c) => {
                let res = self.assignments.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Announcement(c, a) => {
                let res = self
                    .announcement_details
                    .entry((c.clone(), a.clone()))
                    .or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Detail(c, a) => {
                let res = self.details.entry((c.clone(), a.clone())).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Files(c) => {
                let res = self.course_files.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::File(f) => {
                let res = self.files.entry(f.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::History(..) => {}
            ResourceKey::Courses => {}
        }
    }

    /// Every access needs a fresh read. A qualifying in-flight read (started
    /// within the reuse window) satisfies a non-explicit access.
    fn request(&mut self, key: ResourceKey, explicit: bool) -> Option<Effect> {
        if !explicit
            && let Some(SyncState::Syncing { started, .. }) = self.sync_of(&key)
            && started.elapsed() <= REUSE_WINDOW
        {
            return None;
        }
        self.next_request += 1;
        let request = self.next_request;
        self.set_sync(
            &key,
            SyncState::Syncing {
                request,
                started: Instant::now(),
            },
        );
        Some(Effect::Fetch { key, request })
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                self.dirty = true;
                self.notice = None;
                self.on_key(key)
            }
            Msg::Paste(value) => {
                if let Some(draft) = &mut self.submission
                    && !draft.reviewing
                    && !draft.busy
                {
                    let value = value.trim_end_matches(['\r', '\n']);
                    if value.contains(['\r', '\n']) {
                        self.notice = Some("Paste one file path at a time.".into());
                    } else {
                        let replace = draft.input == draft.start_directory
                            && Path::new(value.trim_matches('"')).is_absolute();
                        let size = value.chars().count()
                            + if replace {
                                0
                            } else {
                                draft.input.chars().count()
                            };
                        if size > 1024 {
                            return vec![];
                        }
                        if replace {
                            draft.input = value.to_string();
                        } else {
                            draft.input.push_str(value);
                        }
                        draft.suggestions.update(&draft.input);
                        self.dirty = true;
                    }
                }
                vec![]
            }
            Msg::Fetched {
                key,
                request,
                result,
                at,
            } => self.on_fetched(key, request, result, at),
            Msg::Tick => {
                if self.current_syncing() {
                    self.spinner = self.spinner.wrapping_add(1);
                    self.dirty = true;
                }
                vec![]
            }
            Msg::Resize(w, h) => {
                self.size = (w, h);
                self.dirty = true;
                vec![]
            }
            Msg::LoginFinished { ok, message } => {
                self.dirty = true;
                self.notice = Some(message);
                if ok {
                    self.login_required = false;
                    self.enter_current(true)
                } else {
                    vec![]
                }
            }
            Msg::Transfer { file_id, event } => {
                self.dirty = true;
                self.on_transfer(file_id, event)
            }
            Msg::Submitted {
                course_id,
                assignment_id,
                result,
            } => {
                self.submission = None;
                self.notice = Some(result.unwrap_or_else(|e| e));
                self.dirty = true;
                let mut effects = Vec::new();
                if let Some(e) =
                    self.request(ResourceKey::Detail(course_id.clone(), assignment_id), true)
                {
                    effects.push(e);
                }
                if let Some(e) = self.request(ResourceKey::Assignments(course_id), true) {
                    effects.push(e);
                }
                effects
            }
        }
    }

    fn on_fetched(
        &mut self,
        key: ResourceKey,
        request: u64,
        result: Result<(Payload, FetchStats), CanvasError>,
        at: DateTime<Utc>,
    ) -> Vec<Effect> {
        // Only the most recent request for a resource may update it.
        let current = matches!(self.sync_of(&key), Some(SyncState::Syncing { request: r, .. }) if *r == request);
        if !current {
            return vec![];
        }
        self.dirty = true;
        let first_result = std::mem::take(&mut self.auto_login);
        match result {
            Ok((payload, stats)) => {
                self.login_required = false;
                self.applied.push(request);
                if key == self.view().key()
                    || (self.history_open
                        && matches!(&key, ResourceKey::History(c, a)
                            if matches!(&self.view().kind, ViewKind::Detail { course_id, assignment_id, .. }
                                if c == course_id && a == assignment_id)))
                {
                    self.last_stats = Some((key.label(), stats));
                }
                let mut effects = Vec::new();
                match (payload, &key) {
                    (Payload::Courses(list), ResourceKey::Courses) => {
                        let r = &mut self.courses;
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Announcements(list), ResourceKey::Announcements(c)) => {
                        let r = self.announcements.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Assignments(list), ResourceKey::Assignments(c)) => {
                        let r = self.assignments.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Announcement(detail), ResourceKey::Announcement(c, a)) => {
                        let r = self
                            .announcement_details
                            .entry((c.clone(), a.clone()))
                            .or_default();
                        r.data = Some(*detail);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::Detail(detail), ResourceKey::Detail(c, a)) => {
                        let r = self.details.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(*detail);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.after_detail(c, a));
                    }
                    (Payload::History(history), ResourceKey::History(c, a)) => {
                        let r = self.histories.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(history);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let count = r.data.as_ref().map_or(0, |h| h.attempts.len());
                        self.history_selected = self.history_selected.min(count.saturating_sub(1));
                        self.history_scroll = 0;
                    }
                    (Payload::Files(data), ResourceKey::Files(c)) => {
                        let r = self.course_files.entry(c.clone()).or_default();
                        r.data = Some(*data);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::File(file), ResourceKey::File(f)) => {
                        let r = self.files.entry(f.clone()).or_default();
                        r.data = Some(*file);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        for intent in self.waiting.remove(f).unwrap_or_default() {
                            effects.extend(self.place_file(f, intent));
                        }
                    }
                    _ => self.set_sync(
                        &key,
                        SyncState::Failed(CanvasError::Invalid("mismatched response")),
                    ),
                }
                self.clamp_selection();
                self.mark_current_announcement_read();
                effects
            }
            Err(error) => {
                // Keep whatever was displayed; mark it outdated instead.
                let needs_login = error.needs_login();
                if let ResourceKey::File(f) = &key
                    && let Some(intents) = self.waiting.remove(f)
                    && intents.iter().any(|i| !i.automatic)
                {
                    self.notice = Some(format!(
                        "Could not read that file from Canvas ({}). Press o to open it in the browser.",
                        error.describe()
                    ));
                }
                self.set_sync(&key, SyncState::Failed(error));
                if needs_login {
                    self.login_required = true;
                    // A session found dead at startup goes straight to sign-in;
                    // later expiry waits for the user to press L.
                    if first_result {
                        return vec![Effect::Login];
                    }
                }
                vec![]
            }
        }
    }

    /// Indices of the current list view's rows after filtering.
    pub fn visible_rows(&self) -> Vec<usize> {
        let view = self.view();
        match &view.kind {
            ViewKind::Courses => self
                .courses
                .data
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter(|(_, c)| self.editing_home || !self.hidden_courses.contains(&c.id))
                .filter(|(_, c)| {
                    matches_filter(c.title(), &view.filter)
                        || matches_filter(c.course_code.as_deref().unwrap_or(""), &view.filter)
                })
                .map(|(i, _)| i)
                .collect(),
            ViewKind::Announcements { course_id, .. } => self
                .announcements
                .get(course_id)
                .and_then(|r| r.data.as_deref())
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter(|(_, a)| matches_filter(a.title(), &view.filter))
                .map(|(i, _)| i)
                .collect(),
            ViewKind::Assignments { course_id, .. } => self
                .assignments
                .get(course_id)
                .and_then(|r| r.data.as_deref())
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter(|(_, a)| matches_filter(a.title(), &view.filter))
                .map(|(i, _)| i)
                .collect(),
            ViewKind::Detail { .. } | ViewKind::Announcement { .. } => vec![],
            ViewKind::Files { .. } => (0..self.file_rows().len()).collect(),
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_rows().len();
        let view = self.view_mut();
        if view.selected >= len {
            view.selected = len.saturating_sub(1);
        }
    }

    fn page(&self) -> usize {
        usize::from(self.size.1.saturating_sub(4)).max(1)
    }

    fn is_list(&self) -> bool {
        !matches!(
            self.view().kind,
            ViewKind::Detail { .. } | ViewKind::Announcement { .. }
        )
    }

    fn move_selection(&mut self, delta: isize) {
        if self.is_list() {
            let len = self.visible_rows().len();
            let view = self.view_mut();
            if len == 0 {
                view.selected = 0;
                return;
            }
            let target = view.selected as isize + delta;
            view.selected = target.clamp(0, len as isize - 1) as usize;
        } else {
            let view = self.view_mut();
            view.scroll = (view.scroll as isize + delta).max(0) as usize;
        }
    }

    // ---- Files and the local library ----

    /// The course's display title, from the course list or any open view.
    fn course_title(&self, course_id: &str) -> String {
        if let Some(c) = self
            .courses
            .data
            .as_ref()
            .and_then(|l| l.iter().find(|c| c.id == course_id))
        {
            return c.title().to_string();
        }
        self.stack
            .iter()
            .find_map(|v| match &v.kind {
                ViewKind::Assignments {
                    course_id: c,
                    course_name,
                }
                | ViewKind::Detail {
                    course_id: c,
                    course_name,
                    ..
                }
                | ViewKind::Files {
                    course_id: c,
                    course_name,
                    ..
                } if c == course_id => Some(course_name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("Course {course_id}"))
    }

    /// The course's folder in the library.
    fn course_dir(&self, course_id: &str, course_name: &str) -> String {
        match self
            .courses
            .data
            .as_ref()
            .and_then(|l| l.iter().find(|c| c.id == course_id))
        {
            Some(c) => library::course_folder(c.course_code.as_deref(), c.title()),
            None if !course_name.is_empty() => library::safe_name(course_name),
            None => library::safe_name(&self.course_title(course_id)),
        }
    }

    pub fn folder(&self, course_id: &str, folder_id: &str) -> Option<&CanvasFolder> {
        self.course_files
            .get(course_id)?
            .data
            .as_ref()?
            .folders
            .iter()
            .find(|f| f.id == folder_id)
    }

    /// Local folder for a Canvas folder (`None` = the course's Files root).
    pub fn class_dir(
        &self,
        course_id: &str,
        course_name: &str,
        folder_id: Option<&str>,
    ) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let mut dir = lib.class_files_dir(&self.course_dir(course_id, course_name));
        if let Some(full) = folder_id
            .and_then(|id| self.folder(course_id, id))
            .and_then(|f| f.full_name.as_deref())
        {
            dir.extend(library::class_folder_parts(full));
        }
        Some(dir)
    }

    fn file_dir(&self, course_id: &str, course_name: &str, file: &CanvasFile) -> Option<PathBuf> {
        self.class_dir(course_id, course_name, file.folder_id.as_deref())
    }

    /// Local folder for an assignment's files.
    pub fn detail_dir(&self, course_id: &str, assignment_id: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let title = self
            .details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .map(|d| d.base.title().to_string())
            .or_else(|| {
                self.stack.iter().find_map(|v| match &v.kind {
                    ViewKind::Detail {
                        course_id: c,
                        assignment_id: a,
                        title,
                        ..
                    } if c == course_id && a == assignment_id => Some(title.clone()),
                    _ => None,
                })
            })?;
        let course = self.course_dir(course_id, &self.course_title(course_id));
        Some(lib.assignment_dir(&course, &title))
    }

    /// Where a downloaded file can be found locally.
    pub fn saved_path(&self, file_id: &str) -> Option<PathBuf> {
        self.library
            .as_ref()?
            .entry(file_id)?
            .paths
            .into_iter()
            .next()
    }

    /// Files and links in an assignment's description.
    pub fn links_of(&self, course_id: &str, assignment_id: &str) -> Vec<Link> {
        self.details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|d| d.description.as_deref())
            .map(|html| crate::html::links(html, &self.origin))
            .unwrap_or_default()
    }

    /// Best known details of a file: its own read, else a course listing.
    pub fn file_meta(&self, file_id: &str) -> Option<&CanvasFile> {
        self.files
            .get(file_id)
            .and_then(|r| r.data.as_ref())
            .or_else(|| {
                self.course_files
                    .values()
                    .filter_map(|r| r.data.as_ref()?.files.as_ref())
                    .flatten()
                    .find(|f| f.id == file_id)
            })
    }

    /// Local state of a file, for display. Uses the library index only (no disk access).
    pub fn local_state(&self, file: &CanvasFile) -> Local {
        match self.transfers.get(&file.id) {
            Some(Transfer::Queued { .. }) => return Local::Queued,
            Some(Transfer::Running { received, total }) => {
                return Local::Running {
                    received: *received,
                    total: *total,
                };
            }
            Some(Transfer::Failed(m)) => return Local::Failed(m.clone()),
            None => {}
        }
        let known = self
            .library
            .as_ref()
            .map(|l| l.known_state(&file.id, &file.version()));
        match known {
            Some(LocalState::Current) => Local::Saved,
            Some(LocalState::Changed) => Local::Changed,
            _ if file.locked_for_user == Some(true) || file.url.is_none() => Local::Locked,
            _ => Local::Missing,
        }
    }

    /// The course's top folder (`course files`).
    fn root_folder(data: &CourseFilesData) -> Option<&CanvasFolder> {
        data.folders.iter().find(|f| f.parent_folder_id.is_none())
    }

    /// Rows of the current Files screen: subfolders, then files, by name.
    /// A filter searches the whole course instead of the current folder.
    pub fn file_rows(&self) -> Vec<FileRow<'_>> {
        let view = self.view();
        let ViewKind::Files {
            course_id,
            folder_id,
            ..
        } = &view.kind
        else {
            return vec![];
        };
        let Some(data) = self
            .course_files
            .get(course_id)
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let Some(files) = data.files.as_ref() else {
            return vec![];
        };
        if !view.filter.is_empty() {
            let mut found: Vec<&CanvasFile> = files
                .iter()
                .filter(|f| matches_filter(f.name(), &view.filter))
                .collect();
            found.sort_by_cached_key(|f| f.name().to_lowercase());
            return found.into_iter().map(FileRow::File).collect();
        }
        let here = folder_id
            .clone()
            .or_else(|| Self::root_folder(data).map(|f| f.id.clone()));
        let mut folders: Vec<&CanvasFolder> = data
            .folders
            .iter()
            .filter(|f| {
                f.parent_folder_id.is_some()
                    && f.parent_folder_id == here
                    && f.hidden_for_user != Some(true)
            })
            .collect();
        folders.sort_by_cached_key(|f| f.name.as_deref().unwrap_or("").to_lowercase());
        let mut list: Vec<&CanvasFile> = files
            .iter()
            .filter(|f| here.is_none() || f.folder_id == here)
            .collect();
        list.sort_by_cached_key(|f| f.name().to_lowercase());
        folders
            .into_iter()
            .map(FileRow::Folder)
            .chain(list.into_iter().map(FileRow::File))
            .collect()
    }

    /// `Lectures / Week 1` for the current Files folder (empty at the root).
    pub fn folder_path(&self) -> String {
        let ViewKind::Files {
            course_id,
            folder_id: Some(id),
            ..
        } = &self.view().kind
        else {
            return String::new();
        };
        self.folder(course_id, id)
            .and_then(|f| f.full_name.as_deref())
            .map(|full| full.split('/').skip(1).collect::<Vec<_>>().join(" / "))
            .unwrap_or_default()
    }

    /// Download every file in a folder and its subfolders.
    fn download_folder(
        &mut self,
        course_id: &str,
        course_name: &str,
        folder_id: Option<&str>,
    ) -> Vec<Effect> {
        let Some(data) = self
            .course_files
            .get(course_id)
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        let Some(files) = data.files.as_ref() else {
            self.notice = Some("The instructor has hidden Files for this course.".into());
            return vec![];
        };
        let start = folder_id
            .map(str::to_string)
            .or_else(|| Self::root_folder(data).map(|f| f.id.clone()));
        let mut inside: Vec<Option<String>> = vec![start.clone()];
        let mut i = 0;
        while i < inside.len() {
            let parent = inside[i].clone();
            inside.extend(
                data.folders
                    .iter()
                    .filter(|f| f.parent_folder_id.is_some() && f.parent_folder_id == parent)
                    .map(|f| Some(f.id.clone())),
            );
            i += 1;
        }
        let chosen: Vec<CanvasFile> = files
            .iter()
            .filter(|f| start.is_none() || inside.contains(&f.folder_id))
            .cloned()
            .collect();
        let batch: Vec<(String, PathBuf)> = chosen
            .iter()
            .filter_map(|f| Some((f.id.clone(), self.file_dir(course_id, course_name, f)?)))
            .collect();
        if batch.is_empty() && !chosen.is_empty() {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        }
        self.download_batch(batch)
    }

    /// Place several files without a notice for each.
    fn download_batch(&mut self, batch: Vec<(String, PathBuf)>) -> Vec<Effect> {
        let total = batch.len();
        let mut effects = Vec::new();
        let mut bytes = 0u64;
        for (id, dir) in batch {
            let placed = self.place_file(
                &id,
                Intent {
                    dir,
                    open: false,
                    automatic: false,
                    quiet: true,
                },
            );
            for e in &placed {
                if let Effect::Download { file, .. } = e {
                    bytes += file.size.unwrap_or(0);
                }
            }
            effects.extend(placed);
        }
        let downloads = effects
            .iter()
            .filter(|e| matches!(e, Effect::Download { .. }))
            .count();
        let waiting = effects
            .iter()
            .filter(|e| matches!(e, Effect::Fetch { .. }))
            .count();
        self.notice = Some(match (downloads, waiting) {
            (0, 0) if total == 0 => "No files here.".into(),
            (0, 0) => format!("All {total} files are saved and up to date."),
            (d, 0) => format!("Downloading {d} of {total} files ({}).", human_size(bytes)),
            (d, w) => format!("Downloading {} of {total} files.", d + w),
        });
        effects
    }

    /// Put a file the user chose into `dir`, opening it afterwards if asked.
    fn place_from(&mut self, file_id: &str, dir: Option<PathBuf>, open: bool) -> Vec<Effect> {
        let Some(dir) = dir else {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        };
        self.place_file(
            file_id,
            Intent {
                dir,
                open,
                automatic: false,
                quiet: false,
            },
        )
    }

    /// Make a file appear in a folder: link an up-to-date local copy, or
    /// download it. Waits for the file's details first if they are unknown
    /// or being read again.
    fn place_file(&mut self, file_id: &str, intent: Intent) -> Vec<Effect> {
        let key = ResourceKey::File(file_id.to_string());
        let in_flight = matches!(self.sync_of(&key), Some(SyncState::Syncing { .. }));
        let meta = if in_flight {
            None
        } else {
            self.file_meta(file_id).cloned()
        };
        let Some(file) = meta else {
            let list = self.waiting.entry(file_id.to_string()).or_default();
            if !list.contains(&intent) {
                list.push(intent);
            }
            return if in_flight {
                vec![]
            } else {
                self.request(key, false).into_iter().collect()
            };
        };
        let name = file.name().to_string();
        if file.locked_for_user == Some(true) || file.url.is_none() {
            if !intent.automatic {
                let why = file
                    .lock_explanation
                    .as_deref()
                    .map(|e| crate::html::render(e, 200, "").join(" "))
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "Canvas does not offer it for download right now.".into());
                self.notice = Some(format!("{name} is locked. {why}"));
            }
            return vec![];
        }
        let Some(lib) = self.library.clone() else {
            if !intent.automatic {
                self.notice = Some("The file library is unavailable.".into());
            }
            return vec![];
        };
        let Some(target) = self.claim(&lib, &file, &intent.dir) else {
            if !intent.automatic {
                self.notice = Some(format!("No free name for {name} in its folder."));
            }
            return vec![];
        };
        let placement = Placement {
            target: target.clone(),
            open: intent.open,
            automatic: intent.automatic,
            quiet: intent.quiet,
        };
        if let Some(list) = self.pending.get_mut(file_id) {
            match list.iter_mut().find(|p| p.target == target) {
                Some(p) => {
                    p.open |= placement.open;
                    p.automatic &= placement.automatic;
                    p.quiet &= placement.quiet;
                }
                None => list.push(placement),
            }
            return vec![];
        }
        if lib.state(file_id, &file.version()) == LocalState::Current {
            match lib.reference(file_id, &target) {
                Ok(true) => {
                    if !intent.automatic {
                        lib.keep(file_id);
                    }
                    if intent.open {
                        return vec![Effect::OpenPath(target)];
                    }
                    if !intent.quiet {
                        self.notice = Some(format!("{name} is saved and up to date."));
                    }
                    return vec![];
                }
                Ok(false) => {} // removed meanwhile: download again
                Err(_) => {
                    if !intent.automatic {
                        self.notice = Some(format!("Could not place {name} in its folder."));
                    }
                    return vec![];
                }
            }
        }
        self.pending.insert(file_id.to_string(), vec![placement]);
        self.transfers
            .insert(file_id.to_string(), Transfer::Queued { total: file.size });
        vec![Effect::Download {
            file: Box::new(file),
            target,
            automatic: intent.automatic,
        }]
    }

    /// A path in `dir` for the file that is free or already this file's.
    /// Two different files never share a name; a file the reader did not
    /// put there is never replaced.
    fn claim(&self, lib: &Library, file: &CanvasFile, dir: &std::path::Path) -> Option<PathBuf> {
        let free = |p: &std::path::Path| -> bool {
            let owner = lib.owner_of(p).or_else(|| {
                self.pending
                    .iter()
                    .find(|(_, list)| list.iter().any(|x| x.target == p))
                    .map(|(id, _)| id.clone())
            });
            match owner {
                Some(o) => o == file.id,
                None => !p.exists(),
            }
        };
        let name = library::safe_name(file.name());
        let plain = dir.join(&name);
        if free(&plain) {
            return Some(plain);
        }
        (1..50).find_map(|n| {
            let tag = if n == 1 {
                file.id.clone()
            } else {
                format!("{}-{n}", file.id)
            };
            let candidate = dir.join(library::safe_name(&library::disambiguate(&name, &tag)));
            free(&candidate).then_some(candidate)
        })
    }

    fn on_transfer(&mut self, file_id: String, event: TransferEvent) -> Vec<Effect> {
        let name = |app: &App, fallback: Option<&PathBuf>| {
            app.file_meta(&file_id)
                .map(|f| f.name().to_string())
                .or_else(|| {
                    fallback
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "the file".into())
        };
        match event {
            TransferEvent::Progress { received, total } => {
                if self.pending.contains_key(&file_id) {
                    self.transfers
                        .insert(file_id, Transfer::Running { received, total });
                }
                vec![]
            }
            TransferEvent::Done(path) => {
                self.transfers.remove(&file_id);
                let placements = self.pending.remove(&file_id).unwrap_or_default();
                if let Some(lib) = self.library.clone() {
                    if placements.iter().any(|p| !p.automatic) {
                        lib.keep(&file_id);
                    }
                    for p in placements.iter().filter(|p| p.target != path) {
                        let _ = lib.reference(&file_id, &p.target);
                    }
                }
                if let Some(p) = placements.iter().find(|p| p.open) {
                    return vec![Effect::OpenPath(p.target.clone())];
                }
                if placements.iter().any(|p| !p.quiet) {
                    self.notice = Some(format!("Saved {}.", name(self, Some(&path))));
                }
                vec![]
            }
            TransferEvent::Failed {
                message,
                needs_login,
            } => {
                let placements = self.pending.remove(&file_id).unwrap_or_default();
                if needs_login {
                    self.login_required = true;
                }
                if placements.iter().any(|p| !p.automatic) {
                    self.notice = Some(format!(
                        "Could not download {}: {message}",
                        name(self, None)
                    ));
                }
                self.transfers.insert(file_id, Transfer::Failed(message));
                vec![]
            }
        }
    }

    /// After an assignment is read: read the details of the files it links
    /// to, and keep its images locally so they open instantly later.
    fn after_detail(&mut self, course_id: &str, assignment_id: &str) -> Vec<Effect> {
        let links = self.links_of(course_id, assignment_id);
        let dir = self.detail_dir(course_id, assignment_id);
        let mut effects = Vec::new();
        for link in links {
            let Target::File { id } = &link.target else {
                continue;
            };
            let key = ResourceKey::File(id.clone());
            self.load_snapshot(&key);
            effects.extend(self.request(key, false));
            if link.image
                && let Some(dir) = &dir
            {
                let intent = Intent {
                    dir: dir.clone(),
                    open: false,
                    automatic: true,
                    quiet: true,
                };
                let list = self.waiting.entry(id.clone()).or_default();
                if !list.iter().any(|i| i.dir == intent.dir) {
                    list.push(intent);
                }
            }
        }
        effects
    }

    /// Downloads waiting for a slot and running.
    pub fn transfer_counts(&self) -> (usize, usize) {
        let queued = self
            .transfers
            .values()
            .filter(|t| matches!(t, Transfer::Queued { .. }))
            .count();
        let running = self
            .transfers
            .values()
            .filter(|t| matches!(t, Transfer::Running { .. }))
            .count();
        (queued, running)
    }

    fn start_submission(&mut self, course: &str, assignment: &str) -> Vec<Effect> {
        let Some(resource) = self
            .details
            .get(&(course.to_string(), assignment.to_string()))
        else {
            self.notice = Some("Load the assignment before submitting.".into());
            return vec![];
        };
        if resource.origin != Origin::Network || !matches!(resource.sync, SyncState::Idle) {
            self.notice = Some("Wait for a fresh assignment sync before submitting.".into());
            return vec![];
        }
        if !resource
            .fetched_at
            .is_some_and(|at| Utc::now() - at < chrono::Duration::minutes(2))
        {
            self.notice =
                Some("Checking current submission rules. Press s again when synced.".into());
            return self
                .request(
                    ResourceKey::Detail(course.to_string(), assignment.to_string()),
                    true,
                )
                .into_iter()
                .collect();
        }
        let Some(detail) = &resource.data else {
            return vec![];
        };
        if let Err(reason) = submission::eligible(detail, Utc::now()) {
            self.notice = Some(reason);
            return vec![];
        }
        let start_directory = submission::starting_directory();
        let mut suggestions = submission::PathSuggestions::default();
        suggestions.update(&start_directory);
        self.submission = Some(SubmissionDraft {
            input: start_directory.clone(),
            start_directory,
            suggestions,
            files: Vec::new(),
            reviewing: false,
            busy: false,
        });
        vec![]
    }

    fn open_history(&mut self, course: &str, assignment: &str) -> Vec<Effect> {
        self.history_open = true;
        self.history_selected = 0;
        self.history_scroll = 0;
        self.request(
            ResourceKey::History(course.to_string(), assignment.to_string()),
            true,
        )
        .into_iter()
        .collect()
    }

    fn on_history_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = &self.view().kind
        else {
            self.history_open = false;
            return vec![];
        };
        let (course_id, assignment_id) = (course_id.clone(), assignment_id.clone());
        let count = self
            .histories
            .get(&(course_id.clone(), assignment_id.clone()))
            .and_then(|r| r.data.as_ref())
            .map_or(0, |h| h.attempts.len());
        match key.code {
            KeyCode::Esc | KeyCode::Char('h') => self.history_open = false,
            KeyCode::Left | KeyCode::BackTab => {
                self.history_selected = self.history_selected.saturating_sub(1);
                self.history_scroll = 0;
            }
            KeyCode::Right | KeyCode::Tab => {
                self.history_selected = (self.history_selected + 1).min(count.saturating_sub(1));
                self.history_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_scroll = self.history_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_scroll = self.history_scroll.saturating_add(1)
            }
            KeyCode::PageUp => {
                self.history_scroll = self.history_scroll.saturating_sub(self.page());
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.history_scroll = self.history_scroll.saturating_add(self.page())
            }
            KeyCode::Home | KeyCode::Char('g') => self.history_scroll = 0,
            KeyCode::Char('r') => {
                return self
                    .request(ResourceKey::History(course_id, assignment_id), true)
                    .into_iter()
                    .collect();
            }
            _ => {}
        }
        vec![]
    }

    fn on_submission_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let kind = self.view().kind.clone();
        let draft = self.submission.as_mut().expect("checked above");
        if draft.busy {
            return vec![];
        }
        if draft.reviewing {
            match key.code {
                KeyCode::Esc | KeyCode::Char('n') => draft.reviewing = false,
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    draft.busy = true;
                    if let ViewKind::Detail {
                        course_id,
                        assignment_id,
                        ..
                    } = &kind
                    {
                        return vec![Effect::Submit {
                            course_id: course_id.clone(),
                            assignment_id: assignment_id.clone(),
                            files: draft.files.clone(),
                        }];
                    }
                }
                _ => {}
            }
            return vec![];
        }
        match key.code {
            KeyCode::Esc => self.submission = None,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                draft.input.clear();
                draft.suggestions.update(&draft.input);
            }
            KeyCode::Backspace if key.kind != KeyEventKind::Repeat => {
                draft.input.pop();
                draft.suggestions.update(&draft.input);
            }
            KeyCode::Up | KeyCode::BackTab => draft.suggestions.move_selection(-1),
            KeyCode::Down => draft.suggestions.move_selection(1),
            KeyCode::Tab => {
                if let Some(path) = draft.suggestions.complete() {
                    draft.input = path;
                    draft.suggestions.update(&draft.input);
                }
            }
            KeyCode::Enter => {
                if draft.input.trim().is_empty() || draft.input == draft.start_directory {
                    if draft.files.is_empty() {
                        self.notice = Some("Add a file path first.".into());
                    } else {
                        draft.reviewing = true;
                    }
                } else if let ViewKind::Detail {
                    course_id,
                    assignment_id,
                    ..
                } = &kind
                {
                    if let Some(detail) = self
                        .details
                        .get(&(course_id.clone(), assignment_id.clone()))
                        .and_then(|r| r.data.as_ref())
                    {
                        match submission::choose_file(&draft.input, detail, &draft.files) {
                            Ok(file) => {
                                draft.files.push(file);
                                draft.input = draft.start_directory.clone();
                                draft.suggestions.update(&draft.input);
                            }
                            Err(reason) => self.notice = Some(reason),
                        }
                    }
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && draft.input.chars().count() < 1024 =>
            {
                draft.input.push(c);
                draft.suggestions.update(&draft.input);
            }
            _ => {}
        }
        vec![]
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![];
        }
        if self.submission.is_some() {
            return self.on_submission_key(key);
        }
        if self.history_open {
            return self.on_history_key(key);
        }
        let page = self.page() as isize;
        if self.view().filtering {
            match key.code {
                KeyCode::Esc => {
                    let v = self.view_mut();
                    v.filter.clear();
                    v.filtering = false;
                    v.selected = 0;
                }
                KeyCode::Enter => self.view_mut().filtering = false,
                KeyCode::Backspace => {
                    let v = self.view_mut();
                    v.filter.pop();
                    v.selected = 0;
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    let v = self.view_mut();
                    if v.filter.chars().count() < 64 {
                        v.filter.push(c);
                    }
                    v.selected = 0;
                }
                _ => {}
            }
            return vec![];
        }
        if let Some(effects) = self.on_view_key(key) {
            return effects;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                vec![]
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                vec![]
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                vec![]
            }
            KeyCode::PageUp => {
                self.move_selection(-page);
                vec![]
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.move_selection(page);
                vec![]
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.move_selection(isize::MIN / 2);
                vec![]
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.move_selection(isize::MAX / 2);
                vec![]
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                if !self.view().filter.is_empty() {
                    let v = self.view_mut();
                    v.filter.clear();
                    v.selected = 0;
                    return vec![];
                }
                self.back()
            }
            KeyCode::Char('/') if self.is_list() => {
                self.view_mut().filtering = true;
                vec![]
            }
            KeyCode::Char('r') => {
                let key = self.view().key();
                self.request(key, true).into_iter().collect()
            }
            KeyCode::Char('o') => self
                .browser_url()
                .map(Effect::OpenUrl)
                .into_iter()
                .collect(),
            KeyCode::Char('L') => vec![Effect::Login],
            _ => vec![],
        }
    }

    fn back(&mut self) -> Vec<Effect> {
        if self.stack.len() <= 1 {
            return vec![];
        }
        let left = self.stack.pop().expect("checked above");
        // Going up a folder moves within data already read for this visit.
        if let (ViewKind::Files { course_id: a, .. }, ViewKind::Files { course_id: b, .. }) =
            (&left.kind, &self.view().kind)
            && a == b
        {
            self.dirty = true;
            return vec![];
        }
        // Returning to a view is a new access: show what we have, sync again.
        self.enter_current(false)
    }

    /// Keys that only some views use. `None` falls through to the shared keys.
    fn on_view_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let kind = self.view().kind.clone();
        match kind {
            ViewKind::Assignments { .. } | ViewKind::Announcements { .. } => match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    Some(self.switch_tab(key.code == KeyCode::BackTab))
                }
                _ => None,
            },
            ViewKind::Files {
                course_id,
                course_name,
                folder_id,
            } => match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    Some(self.switch_tab(key.code == KeyCode::BackTab))
                }
                KeyCode::Char('d') => {
                    let rows = self.file_rows();
                    let selected = rows.get(self.view().selected).copied();
                    Some(match selected {
                        Some(FileRow::File(f)) => {
                            let id = f.id.clone();
                            let dir = self.file_dir(&course_id, &course_name, f);
                            self.place_from(&id, dir, false)
                        }
                        Some(FileRow::Folder(f)) => {
                            let id = f.id.clone();
                            self.download_folder(&course_id, &course_name, Some(&id))
                        }
                        None => vec![],
                    })
                }
                KeyCode::Char('D') => {
                    Some(self.download_folder(&course_id, &course_name, folder_id.as_deref()))
                }
                KeyCode::Char('e') => {
                    let rows = self.file_rows();
                    let saved = match rows.get(self.view().selected) {
                        Some(FileRow::File(f)) => self.saved_path(&f.id),
                        _ => None,
                    };
                    let path = saved
                        .or_else(|| self.class_dir(&course_id, &course_name, folder_id.as_deref()));
                    Some(self.reveal(path))
                }
                _ => None,
            },
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => {
                let links = self.announcement_links(&course_id, &announcement_id);
                let selected = self.view().link.filter(|&i| i < links.len());
                match key.code {
                    KeyCode::Tab | KeyCode::BackTab if !links.is_empty() => {
                        let n = links.len();
                        let next = match (selected, key.code) {
                            (None, KeyCode::BackTab) => n - 1,
                            (None, _) => 0,
                            (Some(i), KeyCode::BackTab) => (i + n - 1) % n,
                            (Some(i), _) => (i + 1) % n,
                        };
                        self.view_mut().link = Some(next);
                        Some(vec![])
                    }
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i < links.len() {
                            self.view_mut().link = Some(i);
                        }
                        Some(vec![])
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Some(match selected {
                        Some(i) => self.activate_announcement_link(
                            &course_id,
                            &announcement_id,
                            links[i].clone(),
                        ),
                        None => vec![],
                    }),
                    KeyCode::Esc if selected.is_some() => {
                        self.view_mut().link = None;
                        Some(vec![])
                    }
                    KeyCode::Char('d') => Some(match selected.map(|i| &links[i].target) {
                        Some(Target::File { id }) => {
                            let id = id.clone();
                            let dir = self.announcement_dir(&course_id, &announcement_id);
                            self.place_announcement_file(&id, dir, false)
                        }
                        Some(_) => {
                            self.notice = Some("That link is not a file.".into());
                            vec![]
                        }
                        None => {
                            self.notice = Some(
                                "Select a file with Tab first, or press D for all files.".into(),
                            );
                            vec![]
                        }
                    }),
                    KeyCode::Char('D') => {
                        let dir = self.announcement_dir(&course_id, &announcement_id);
                        let ids: Vec<String> = links
                            .iter()
                            .filter_map(|l| match &l.target {
                                Target::File { id } => Some(id.clone()),
                                _ => None,
                            })
                            .collect();
                        if ids.is_empty() {
                            self.notice = Some("This announcement has no files.".into());
                            return Some(vec![]);
                        }
                        let Some(dir) = dir else {
                            self.notice = Some("The file library is unavailable.".into());
                            return Some(vec![]);
                        };
                        let mut effects = Vec::new();
                        for id in ids {
                            effects.extend(self.place_announcement_file(
                                &id,
                                Some(dir.clone()),
                                false,
                            ));
                        }
                        self.notice = Some("Checking files before downloading…".into());
                        Some(effects)
                    }
                    KeyCode::Char('e') => {
                        let saved = match selected.map(|i| &links[i].target) {
                            Some(Target::File { id }) => self.saved_path(id),
                            _ => None,
                        };
                        let path =
                            saved.or_else(|| self.announcement_dir(&course_id, &announcement_id));
                        Some(self.reveal(path))
                    }
                    _ => None,
                }
            }
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => {
                if key.code == KeyCode::Char('h') {
                    return Some(self.open_history(&course_id, &assignment_id));
                }
                if key.code == KeyCode::Char('s') {
                    return Some(self.start_submission(&course_id, &assignment_id));
                }
                let links = self.links_of(&course_id, &assignment_id);
                let selected = self.view().link.filter(|&i| i < links.len());
                match key.code {
                    KeyCode::Tab | KeyCode::BackTab if !links.is_empty() => {
                        let n = links.len();
                        let next = match (selected, key.code) {
                            (None, KeyCode::BackTab) => n - 1,
                            (None, _) => 0,
                            (Some(i), KeyCode::BackTab) => (i + n - 1) % n,
                            (Some(i), _) => (i + 1) % n,
                        };
                        self.view_mut().link = Some(next);
                        Some(vec![])
                    }
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i < links.len() {
                            self.view_mut().link = Some(i);
                        }
                        Some(vec![])
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Some(match selected {
                        Some(i) => self.activate_link(&course_id, &assignment_id, links[i].clone()),
                        None => vec![],
                    }),
                    KeyCode::Esc if selected.is_some() => {
                        self.view_mut().link = None;
                        Some(vec![])
                    }
                    KeyCode::Char('d') => Some(match selected.map(|i| &links[i].target) {
                        Some(Target::File { id }) => {
                            let id = id.clone();
                            let dir = self.detail_dir(&course_id, &assignment_id);
                            self.place_from(&id, dir, false)
                        }
                        Some(_) => {
                            self.notice = Some("That link is not a file.".into());
                            vec![]
                        }
                        None => {
                            self.notice = Some(
                                "Select a file with Tab first, or press D for all files.".into(),
                            );
                            vec![]
                        }
                    }),
                    KeyCode::Char('D') => {
                        let dir = self.detail_dir(&course_id, &assignment_id);
                        let ids: Vec<String> = links
                            .iter()
                            .filter_map(|l| match &l.target {
                                Target::File { id } => Some(id.clone()),
                                _ => None,
                            })
                            .collect();
                        if ids.is_empty() {
                            self.notice = Some("This assignment has no files.".into());
                            return Some(vec![]);
                        }
                        let Some(dir) = dir else {
                            self.notice = Some("The file library is unavailable.".into());
                            return Some(vec![]);
                        };
                        let batch = ids.into_iter().map(|id| (id, dir.clone())).collect();
                        Some(self.download_batch(batch))
                    }
                    KeyCode::Char('e') => {
                        let saved = match selected.map(|i| &links[i].target) {
                            Some(Target::File { id }) => self.saved_path(id),
                            _ => None,
                        };
                        let path = saved.or_else(|| self.detail_dir(&course_id, &assignment_id));
                        Some(self.reveal(path))
                    }
                    _ => None,
                }
            }
            ViewKind::Courses => match key.code {
                KeyCode::Char('E') => {
                    self.editing_home = true;
                    self.view_mut().selected = 0;
                    self.view_mut().offset = 0;
                    Some(vec![])
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if self.editing_home => {
                    self.toggle_selected_course_hidden();
                    Some(vec![])
                }
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace
                    if self.editing_home =>
                {
                    self.editing_home = false;
                    self.view_mut().selected = 0;
                    self.view_mut().offset = 0;
                    self.clamp_selection();
                    Some(vec![])
                }
                KeyCode::Char('e') => {
                    let root = self.library.as_ref().map(|l| l.root().to_path_buf());
                    Some(self.reveal(root))
                }
                _ => None,
            },
        }
    }

    fn toggle_selected_course_hidden(&mut self) {
        let Some(&index) = self.visible_rows().get(self.view().selected) else {
            return;
        };
        let Some(course) = self.courses.data.as_ref().and_then(|c| c.get(index)) else {
            return;
        };
        let id = course.id.clone();
        let title = course.title().to_string();
        if self.hidden_courses.remove(&id) {
            self.notice = Some(format!("Showing {title}."));
        } else {
            self.hidden_courses.insert(id);
            self.notice = Some(format!("Hid {title}."));
        }
        if let Some(store) = &self.store {
            store.save_hidden_courses(&self.hidden_courses);
        }
        self.clamp_selection();
    }

    /// Cycle course tabs; leaving Files leaves its nested folders.
    fn switch_tab(&mut self, backwards: bool) -> Vec<Effect> {
        let (course_id, course_name, index) = match &self.view().kind {
            ViewKind::Assignments {
                course_id,
                course_name,
            } => (course_id.clone(), course_name.clone(), 0),
            ViewKind::Files {
                course_id,
                course_name,
                ..
            } => (course_id.clone(), course_name.clone(), 1),
            ViewKind::Announcements {
                course_id,
                course_name,
            } => (course_id.clone(), course_name.clone(), 2),
            _ => return vec![],
        };
        let next = match (index + if backwards { 2 } else { 1 }) % 3 {
            0 => ViewKind::Assignments {
                course_id,
                course_name,
            },
            1 => ViewKind::Files {
                course_id,
                course_name,
                folder_id: None,
            },
            _ => ViewKind::Announcements {
                course_id,
                course_name,
            },
        };
        while self.stack.len() > 1
            && matches!(
                self.stack[self.stack.len() - 2].kind,
                ViewKind::Files { .. }
            )
        {
            self.stack.pop();
        }
        *self.view_mut() = View::new(next);
        self.enter_current(false)
    }

    fn reveal(&mut self, path: Option<PathBuf>) -> Vec<Effect> {
        match path {
            Some(p) => vec![Effect::Reveal(p)],
            None => {
                self.notice = Some("The file library is unavailable.".into());
                vec![]
            }
        }
    }

    pub fn announcement_status(
        &self,
        course_id: &str,
        summary: &crate::announcements::Summary,
    ) -> &'static str {
        if self
            .announcement_reads
            .contains(&(course_id.into(), summary.id.clone()))
        {
            "Read"
        } else {
            summary.status()
        }
    }

    fn mark_current_announcement_read(&mut self) {
        let ViewKind::Announcement {
            course_id,
            announcement_id,
            ..
        } = &self.view().kind
        else {
            return;
        };
        let key = (course_id.clone(), announcement_id.clone());
        let readable = self
            .announcement_details
            .get(&key)
            .and_then(|r| r.data.as_ref())
            .is_some_and(|d| d.can_view() && d.message.is_some());
        if readable && self.announcement_reads.insert(key) {
            if let Some(store) = &self.store {
                store.save_announcement_reads(&self.announcement_reads);
            }
        }
    }

    pub fn announcement_links(&self, course_id: &str, id: &str) -> Vec<Link> {
        let Some(detail) = self
            .announcement_details
            .get(&(course_id.into(), id.into()))
            .and_then(|r| r.data.as_ref())
        else {
            return vec![];
        };
        if !detail.can_view() {
            return vec![];
        }
        let mut links = crate::html::links(detail.message.as_deref().unwrap_or(""), &self.origin);
        for f in &detail.attachments {
            if crate::model::valid_id(&f.id)
                && !links
                    .iter()
                    .any(|l| matches!(&l.target, Target::File {id} if id == &f.id))
            {
                links.push(Link {
                    text: crate::html::sanitize(f.name()),
                    url: format!("{}/courses/{course_id}/files/{}", self.origin, f.id),
                    target: Target::File { id: f.id.clone() },
                    image: false,
                });
            }
        }
        links
    }

    pub fn announcement_dir(&self, course_id: &str, id: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        Some(
            lib.root()
                .join(self.course_dir(course_id, &self.course_title(course_id)))
                .join("Announcement Files")
                .join(id),
        )
    }

    /// File metadata is lazy for announcements; validate it when a file is
    /// chosen so a saved URL or version never suppresses this fresh read.
    fn place_announcement_file(
        &mut self,
        id: &str,
        dir: Option<PathBuf>,
        open: bool,
    ) -> Vec<Effect> {
        if dir.is_none() {
            return self.place_from(id, dir, open);
        }
        let mut effects: Vec<_> = self
            .request(ResourceKey::File(id.into()), false)
            .into_iter()
            .collect();
        effects.extend(self.place_from(id, dir, open));
        effects
    }

    fn activate_announcement_link(&mut self, course_id: &str, id: &str, link: Link) -> Vec<Effect> {
        if let Target::File { id: file_id } = &link.target {
            let dir = self.announcement_dir(course_id, id);
            self.place_announcement_file(file_id, dir, true)
        } else {
            self.activate_link(course_id, id, link)
        }
    }

    fn activate_link(&mut self, course_id: &str, assignment_id: &str, link: Link) -> Vec<Effect> {
        match link.target {
            Target::File { id } => {
                let dir = self.detail_dir(course_id, assignment_id);
                self.place_from(&id, dir, true)
            }
            Target::Assignment { course_id, id } => {
                let course_name = self.course_title(&course_id);
                let title = if link.text.is_empty() {
                    "Assignment".to_string()
                } else {
                    link.text
                };
                self.stack.push(View::new(ViewKind::Detail {
                    course_id,
                    course_name,
                    assignment_id: id,
                    title,
                }));
                self.enter_current(false)
            }
            // Pages, modules, discussions, tools and outside sites open in the browser.
            Target::Canvas | Target::Web | Target::Mail => vec![Effect::OpenUrl(link.url)],
        }
    }

    fn open_selected(&mut self) -> Vec<Effect> {
        let rows = self.visible_rows();
        let Some(&index) = rows.get(self.view().selected) else {
            return vec![];
        };
        let next = match &self.view().kind {
            ViewKind::Courses => {
                let Some(course) = self.courses.data.as_ref().and_then(|l| l.get(index)) else {
                    return vec![];
                };
                ViewKind::Assignments {
                    course_id: course.id.clone(),
                    course_name: course.title().to_string(),
                }
            }
            ViewKind::Announcements {
                course_id,
                course_name,
            } => {
                let Some(a) = self
                    .announcements
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|l| l.get(index))
                else {
                    return vec![];
                };
                ViewKind::Announcement {
                    course_id: course_id.clone(),
                    course_name: course_name.clone(),
                    announcement_id: a.id.clone(),
                    title: a.title().to_string(),
                }
            }
            ViewKind::Assignments {
                course_id,
                course_name,
            } => {
                let Some(a) = self
                    .assignments
                    .get(course_id)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|l| l.get(index))
                else {
                    return vec![];
                };
                ViewKind::Detail {
                    course_id: course_id.clone(),
                    course_name: course_name.clone(),
                    assignment_id: a.id.clone(),
                    title: a.title().to_string(),
                }
            }
            ViewKind::Detail { .. } | ViewKind::Announcement { .. } => return vec![],
            ViewKind::Files {
                course_id,
                course_name,
                ..
            } => {
                let (course_id, course_name) = (course_id.clone(), course_name.clone());
                let rows = self.file_rows();
                return match rows.get(self.view().selected).copied() {
                    Some(FileRow::Folder(f)) => {
                        let id = f.id.clone();
                        self.stack.push(View::new(ViewKind::Files {
                            course_id,
                            course_name,
                            folder_id: Some(id),
                        }));
                        self.dirty = true;
                        vec![]
                    }
                    Some(FileRow::File(f)) => {
                        let id = f.id.clone();
                        let dir = self.file_dir(&course_id, &course_name, f);
                        self.place_from(&id, dir, true)
                    }
                    None => vec![],
                };
            }
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// The page `o` opens: the selected row or link, or the current screen.
    /// Pages for Canvas items are built on the Canvas origin; links from a
    /// description may lead anywhere on the web.
    pub fn browser_url(&self) -> Option<String> {
        let same_origin = |u: &str| u.starts_with(&format!("{}/", self.origin));
        match &self.view().kind {
            ViewKind::Announcements { course_id, .. } => {
                let base = format!("{}/courses/{course_id}/announcements", self.origin);
                let rows = self.visible_rows();
                let item = rows
                    .get(self.view().selected)
                    .and_then(|i| self.announcements.get(course_id)?.data.as_ref()?.get(*i));
                Some(
                    item.map(|a| {
                        format!(
                            "{}/courses/{course_id}/discussion_topics/{}",
                            self.origin, a.id
                        )
                    })
                    .unwrap_or(base),
                )
            }
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => {
                if let Some(i) = self.view().link {
                    return self
                        .announcement_links(course_id, announcement_id)
                        .get(i)
                        .map(|l| l.url.clone());
                }
                Some(format!(
                    "{}/courses/{course_id}/discussion_topics/{announcement_id}",
                    self.origin
                ))
            }

            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } if self.view().link.is_some() => {
                let links = self.links_of(course_id, assignment_id);
                let link = links.get(self.view().link?)?;
                Some(match &link.target {
                    // The file's Canvas page rather than a direct download.
                    Target::File { id } => link.url.replacen(
                        &format!("/files/{id}/download"),
                        &format!("/files/{id}"),
                        1,
                    ),
                    _ => link.url.clone(),
                })
            }
            ViewKind::Files {
                course_id,
                folder_id,
                ..
            } => {
                let base = format!("{}/courses/{}/files", self.origin, course_id);
                let rows = self.file_rows();
                let folder = match rows.get(self.view().selected) {
                    Some(FileRow::File(f)) => return Some(format!("{base}/{}", f.id)),
                    Some(FileRow::Folder(f)) => Some(*f),
                    None => folder_id
                        .as_deref()
                        .and_then(|id| self.folder(course_id, id)),
                };
                let parts: Vec<String> = folder
                    .and_then(|f| f.full_name.as_deref())
                    .map(|full| full.split('/').skip(1).map(percent_encode).collect())
                    .unwrap_or_default();
                Some(if parts.is_empty() {
                    base
                } else {
                    format!("{base}/folder/{}", parts.join("/"))
                })
            }
            ViewKind::Courses => {
                let rows = self.visible_rows();
                let index = *rows.get(self.view().selected)?;
                let course = self.courses.data.as_ref()?.get(index)?;
                Some(format!("{}/courses/{}", self.origin, course.id))
            }
            ViewKind::Assignments { course_id, .. } => {
                let rows = self.visible_rows();
                let index = *rows.get(self.view().selected)?;
                let a = self.assignments.get(course_id)?.data.as_ref()?.get(index)?;
                Some(
                    a.html_url
                        .clone()
                        .filter(|u| same_origin(u))
                        .unwrap_or_else(|| {
                            format!("{}/courses/{}/assignments/{}", self.origin, course_id, a.id)
                        }),
                )
            }
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => {
                let html = self
                    .details
                    .get(&(course_id.clone(), assignment_id.clone()))
                    .and_then(|r| r.data.as_ref())
                    .and_then(|d| d.base.html_url.clone())
                    .filter(|u| same_origin(u));
                Some(html.unwrap_or_else(|| {
                    format!(
                        "{}/courses/{}/assignments/{}",
                        self.origin, course_id, assignment_id
                    )
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn course(id: &str, name: &str) -> Course {
        Course {
            id: id.into(),
            name: Some(name.into()),
            ..Default::default()
        }
    }

    fn fetch_of(effects: &[Effect]) -> (ResourceKey, u64) {
        match effects.iter().find(|e| matches!(e, Effect::Fetch { .. })) {
            Some(Effect::Fetch { key, request }) => (key.clone(), *request),
            _ => panic!("expected a fetch, got {effects:?}"),
        }
    }

    fn deliver(app: &mut App, key: ResourceKey, request: u64, payload: Payload) {
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((payload, FetchStats::default())),
            at: Utc::now(),
        });
    }

    fn app_with_courses() -> App {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, r) = fetch_of(&app.start());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![
                course("10", "GPU Computing"),
                course("20", "Compilers"),
            ]),
        );
        app
    }

    #[test]
    fn start_fetches_courses_and_applies_result() {
        let app = app_with_courses();
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
        assert_eq!(app.courses.origin, Origin::Network);
        assert!(!app.current_syncing());
    }

    #[test]
    fn file_submission_requires_review_and_explicit_yes() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let mut detail = AssignmentDetail::default();
        detail.base.id = "20".into();
        detail.base.submission_types = vec!["online_upload".into()];
        let resource = app.details.entry(("10".into(), "20".into())).or_default();
        resource.data = Some(detail);
        resource.origin = Origin::Network;
        resource.fetched_at = Some(Utc::now());
        assert!(app.update(key(KeyCode::Char('s'))).is_empty());
        assert!(app.submission.is_some());
        let start = app.submission.as_ref().unwrap().start_directory.clone();
        assert!(start.ends_with(std::path::MAIN_SEPARATOR));
        assert_eq!(app.submission.as_ref().unwrap().input, start);
        app.update(Msg::Paste("\"C:\\work\\lab.pdf\"".into()));
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf\""
        );
        for _ in 0..12 {
            app.update(Msg::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::NONE,
                ratatui::crossterm::event::KeyEventKind::Repeat,
            )));
        }
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf\""
        );
        app.update(key(KeyCode::Backspace));
        assert_eq!(
            app.submission.as_ref().unwrap().input,
            "\"C:\\work\\lab.pdf"
        );
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.submission.as_ref().unwrap().input.is_empty());
        app.submission.as_mut().unwrap().input = start;
        app.submission.as_mut().unwrap().files.push(FileChoice {
            path: PathBuf::from("C:/work/lab.pdf"),
            name: "lab.pdf".into(),
            size: 7,
        });
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.submission.as_ref().unwrap().reviewing);
        assert!(app.update(key(KeyCode::Char('n'))).is_empty());
        assert!(!app.submission.as_ref().unwrap().reviewing);
        app.update(key(KeyCode::Enter));
        assert!(matches!(&app.update(key(KeyCode::Char('y')))[..],
            [Effect::Submit { course_id, assignment_id, files }] if course_id == "10" && assignment_id == "20" && files.len() == 1));
        assert!(app.update(key(KeyCode::Char('y'))).is_empty());
    }

    #[test]
    fn past_submissions_fetch_only_when_opened_and_refresh_each_visit() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        assert!(app.histories.is_empty());
        let (key1, request1) = fetch_of(&app.update(key(KeyCode::Char('h'))));
        assert_eq!(key1, ResourceKey::History("10".into(), "20".into()));
        assert!(app.history_open);
        let history = submission::History::parse(serde_json::json!({
            "attempt": 2, "submitted_at": "2026-10-22T12:00:00Z",
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z"}
            ]
        }))
        .unwrap();
        deliver(&mut app, key1, request1, Payload::History(history));
        assert_eq!(
            app.histories[&("10".into(), "20".into())]
                .data
                .as_ref()
                .unwrap()
                .attempts
                .len(),
            2
        );
        app.update(key(KeyCode::Right));
        assert_eq!(app.history_selected, 1);
        app.update(key(KeyCode::Esc));
        assert!(!app.history_open);
        let (key2, request2) = fetch_of(&app.update(key(KeyCode::Char('h'))));
        assert_eq!(key2, ResourceKey::History("10".into(), "20".into()));
        assert!(request2 > request1);
    }

    #[test]
    fn hidden_courses_survive_refresh_and_restart_and_can_be_restored() {
        let root =
            std::env::temp_dir().join(format!("tuicanvas-hidden-courses-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "one"));
        let mut app = App::new("https://ufl.instructure.com", Some(store.clone()));
        let (k, r) = fetch_of(&app.start());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        assert!(app.update(key(KeyCode::Char('E'))).is_empty());
        assert!(app.editing_home);
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.visible_rows(), [0, 1]);
        app.update(key(KeyCode::Esc));
        assert_eq!(app.visible_rows(), [1]);
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        assert_eq!(app.visible_rows(), [1]);
        let mut restarted = App::new("https://ufl.instructure.com", Some(store));
        let (k, r) = fetch_of(&restarted.start());
        deliver(
            &mut restarted,
            k,
            r,
            Payload::Courses(vec![course("10", "GPU"), course("20", "Compilers")]),
        );
        assert_eq!(restarted.visible_rows(), [1]);
        restarted.update(key(KeyCode::Char('E')));
        assert_eq!(restarted.visible_rows(), [0, 1]);
        restarted.update(key(KeyCode::Enter));
        assert!(restarted.hidden_courses.is_empty());
        restarted.update(key(KeyCode::Esc));
        assert_eq!(restarted.visible_rows(), [0, 1]);
        let other = App::new(
            "https://ufl.instructure.com",
            Some(Arc::new(SnapshotStore::new(&root, "other"))),
        );
        assert!(other.hidden_courses.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hiding_every_course_keeps_recovery_available() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('E')));
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Esc));
        assert!(app.visible_rows().is_empty());
        app.update(key(KeyCode::Char('E')));
        assert_eq!(app.visible_rows(), [0, 1]);
        app.update(key(KeyCode::Enter));
        assert_eq!(app.hidden_courses.len(), 1);
        app.update(key(KeyCode::Esc));
        assert_eq!(app.visible_rows(), [0]);
    }

    #[test]
    fn stale_response_is_ignored_after_explicit_refresh() {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, first) = fetch_of(&app.start());
        let (_, second) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        assert_ne!(first, second);
        deliver(
            &mut app,
            k.clone(),
            first,
            Payload::Courses(vec![course("1", "old")]),
        );
        assert!(
            app.courses.data.is_none(),
            "older request must not satisfy the newer access"
        );
        deliver(
            &mut app,
            k,
            second,
            Payload::Courses(vec![course("2", "new")]),
        );
        assert_eq!(app.courses.data.as_ref().unwrap()[0].id, "2");
    }

    #[test]
    fn failure_keeps_data_and_marks_outdated() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Network(crate::transport::NetError::Timeout)),
            at: Utc::now(),
        });
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
        assert!(matches!(app.courses.sync, SyncState::Failed(_)));
        assert!(!app.login_required);
    }

    #[test]
    fn expired_session_requests_login() {
        let mut app = App::new("https://ufl.instructure.com", None);
        let (k, r) = fetch_of(&app.start());
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Unauthenticated),
            at: Utc::now(),
        });
        assert_eq!(
            effects,
            vec![Effect::Login],
            "dead session at startup starts sign-in"
        );
        assert!(app.login_required);
        assert_eq!(app.update(key(KeyCode::Char('L'))), vec![Effect::Login]);
        let effects = app.update(Msg::LoginFinished {
            ok: true,
            message: "ok".into(),
        });
        assert_eq!(fetch_of(&effects).0, ResourceKey::Courses);
        assert!(!app.login_required);
    }

    #[test]
    fn later_expiry_waits_for_user() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Err(CanvasError::Unauthenticated),
            at: Utc::now(),
        });
        assert!(effects.is_empty());
        assert!(app.login_required);
        assert_eq!(app.courses.data.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn navigation_fetches_each_access_and_back_resyncs() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        let a = AssignmentSummary {
            id: "7".into(),
            name: Some("Lab-2".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a]));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Detail("10".into(), "7".into()));
        // Back to the list: the earlier list read completed, so a new one is required.
        let (k, _) = fetch_of(&app.update(key(KeyCode::Esc)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        assert_eq!(app.stack.len(), 2);
    }

    #[test]
    fn recent_in_flight_read_satisfies_quick_reentry() {
        let mut app = app_with_courses();
        let first = app.update(key(KeyCode::Enter));
        fetch_of(&first);
        app.update(key(KeyCode::Esc));
        let again = app.update(key(KeyCode::Enter));
        assert!(
            again.is_empty(),
            "in-flight read from <2s ago is promoted, not duplicated"
        );
    }

    #[test]
    fn filter_narrows_rows_and_selection_targets_filtered_item() {
        let mut app = app_with_courses();
        app.update(key(KeyCode::Char('/')));
        for c in "comp".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert_eq!(app.visible_rows(), vec![0, 1]);
        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.visible_rows(), vec![1]);
        app.update(key(KeyCode::Enter));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Assignments("20".into()));
    }

    #[test]
    fn browser_urls_stay_on_origin() {
        let mut app = app_with_courses();
        assert_eq!(
            app.browser_url().unwrap(),
            "https://ufl.instructure.com/courses/10"
        );
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let a = AssignmentSummary {
            id: "7".into(),
            html_url: Some("https://evil.example/x".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a]));
        assert_eq!(
            app.browser_url().unwrap(),
            "https://ufl.instructure.com/courses/10/assignments/7"
        );
    }

    #[test]
    fn snapshot_is_shown_before_sync() {
        let root = std::env::temp_dir().join(format!("tuicanvas-app-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "1"));
        store.save(
            &ResourceKey::Courses,
            &vec![course("5", "Saved")],
            Utc::now(),
        );
        let mut app = App::new("https://ufl.instructure.com", Some(store.clone()));
        let effects = app.start();
        assert_eq!(app.courses.origin, Origin::Cache);
        assert_eq!(app.courses.data.as_ref().unwrap()[0].id, "5");
        assert!(app.current_syncing());
        fetch_of(&effects);
        store.clear();
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- Files and the files & links list ----

    const ORIGIN: &str = "https://ufl.instructure.com";

    fn temp_library(tag: &str) -> (Arc<Library>, PathBuf) {
        let root = std::env::temp_dir().join(format!("tuicanvas-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (Arc::new(Library::open(&root, "ufl.instructure.com")), root)
    }

    fn canvas_file(id: &str, name: &str, folder: &str) -> CanvasFile {
        CanvasFile {
            id: id.into(),
            display_name: Some(name.into()),
            size: Some(5),
            updated_at: Some("2026-10-01T12:00:00Z".into()),
            url: Some(format!("{ORIGIN}/files/{id}/download?verifier=v")),
            folder_id: Some(folder.into()),
            ..Default::default()
        }
    }

    fn canvas_folder(id: &str, full: &str, parent: Option<&str>) -> CanvasFolder {
        CanvasFolder {
            id: id.into(),
            name: full.rsplit('/').next().map(str::to_string),
            full_name: Some(full.into()),
            parent_folder_id: parent.map(str::to_string),
            ..Default::default()
        }
    }

    fn course_files() -> CourseFilesData {
        CourseFilesData {
            folders: vec![
                canvas_folder("1", "course files", None),
                canvas_folder("2", "course files/Lectures", Some("1")),
                canvas_folder("3", "course files/Lectures/Week 1", Some("2")),
            ],
            files: Some(vec![
                canvas_file("100", "syllabus.pdf", "1"),
                canvas_file("101", "lecture1.pdf", "3"),
                canvas_file("102", "Notes.txt", "2"),
            ]),
        }
    }

    /// Courses -> GPU Computing -> Tab to Files, with the listing delivered.
    fn files_app(library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(&mut app, k, r, Payload::Assignments(vec![]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Files("10".into()));
        deliver(&mut app, k, r, Payload::Files(Box::new(course_files())));
        app
    }

    fn row_names(app: &App) -> Vec<String> {
        app.file_rows()
            .iter()
            .map(|r| r.name().to_string())
            .collect()
    }

    /// What the runtime does for a download: write the file, install it, report.
    fn finish_download(app: &mut App, effect: &Effect, body: &str) -> Vec<Effect> {
        let Effect::Download {
            file,
            target,
            automatic,
        } = effect
        else {
            panic!("expected a download, got {effect:?}")
        };
        let lib = app.library.clone().unwrap();
        let partial = lib.temp_dir().join(format!("{}.part", file.id));
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(&partial, body).unwrap();
        lib.install(&file.id, &partial, file.version(), target, *automatic)
            .unwrap();
        app.update(Msg::Transfer {
            file_id: file.id.clone(),
            event: TransferEvent::Done(target.clone()),
        })
    }

    #[test]
    fn files_tab_lists_folders_first_and_moves_between_folders_without_reading_again() {
        let mut app = files_app(None);
        assert_eq!(row_names(&app), vec!["Lectures", "syllabus.pdf"]);
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "opening a folder uses the listing already read"
        );
        assert_eq!(row_names(&app), vec!["Week 1", "Notes.txt"]);
        assert_eq!(app.folder_path(), "Lectures");
        app.update(key(KeyCode::Enter));
        assert_eq!(row_names(&app), vec!["lecture1.pdf"]);
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(row_names(&app), vec!["Lectures", "syllabus.pdf"]);
        // Leaving Files for the course list is a new visit there.
        assert_eq!(
            fetch_of(&app.update(key(KeyCode::Esc))).0,
            ResourceKey::Courses
        );
    }

    #[test]
    fn tab_switches_tabs_and_leaves_all_open_folders() {
        let mut app = files_app(None);
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.stack.len(), 4);
        let (k, _) = fetch_of(&app.update(key(KeyCode::BackTab)));
        assert_eq!(k, ResourceKey::Assignments("10".into()));
        assert_eq!(app.stack.len(), 2);
        assert!(matches!(app.view().kind, ViewKind::Assignments { .. }));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Files("10".into()));
        assert_eq!(app.stack.len(), 2);
    }

    #[test]
    fn announcements_navigate_filter_refresh_and_ignore_old_responses() {
        let mut app = files_app(None);
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        assert_eq!(k, ResourceKey::Announcements("10".into()));
        let a = crate::announcements::Summary {
            id: "8".into(),
            title: Some("Exam update".into()),
            ..Default::default()
        };
        deliver(&mut app, k.clone(), r, Payload::Announcements(vec![a]));
        assert_eq!(app.visible_rows(), [0]);
        app.update(key(KeyCode::Char('/')));
        for c in "exam".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        app.update(key(KeyCode::Enter));
        let (detail_key, request) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(
            detail_key,
            ResourceKey::Announcement("10".into(), "8".into())
        );
        let detail = crate::announcements::Detail {
            base: crate::announcements::Summary {
                id: "8".into(),
                ..Default::default()
            },
            message: Some("<a href='/courses/10/files/9/download'>Notes</a>".into()),
            attachments: vec![
                canvas_file("9", "notes.pdf", "1"),
                canvas_file("11", "extra.txt", "1"),
            ],
            ..Default::default()
        };
        deliver(
            &mut app,
            detail_key,
            request,
            Payload::Announcement(Box::new(detail)),
        );
        assert_eq!(app.announcement_links("10", "8").len(), 2);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.view().scroll, 1);
        app.update(key(KeyCode::Tab));
        assert!(app.browser_url().unwrap().ends_with("/files/9/download"));
        app.update(key(KeyCode::Esc)); // deselect link
        assert_eq!(fetch_of(&app.update(key(KeyCode::Esc))).0, k);
        let (_, newer) = fetch_of(&app.update(key(KeyCode::Char('r'))));
        deliver(&mut app, k.clone(), r, Payload::Announcements(vec![]));
        assert_eq!(app.visible_rows(), [0]);
        app.update(Msg::Fetched {
            key: k,
            request: newer,
            result: Err(CanvasError::NoAccess(403)),
            at: Utc::now(),
        });
        assert!(matches!(app.announcements["10"].sync, SyncState::Failed(_)));
        assert_eq!(app.visible_rows(), [0]);
        // Forward wraps to Assignments; backward wraps to Announcements.
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Assignments("10".into()));
        let effects = app.update(key(KeyCode::BackTab));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Announcements("10".into())
        );
    }

    #[test]
    fn local_announcement_reads_survive_refresh_restart_and_stay_account_scoped() {
        let root =
            std::env::temp_dir().join(format!("tuicanvas-local-reads-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        let mut app = app_with_courses();
        app.store = Some(store.clone());
        app.update(key(KeyCode::Enter));
        let (k, r) = fetch_of(&app.update(key(KeyCode::BackTab)));
        let summary = crate::announcements::Summary {
            id: "8".into(),
            read_state: Some("unread".into()),
            ..Default::default()
        };
        deliver(
            &mut app,
            k,
            r,
            Payload::Announcements(vec![summary.clone()]),
        );
        assert_eq!(app.announcement_status("10", &summary), "Unread");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(app.announcement_status("10", &summary), "Unread");
        let detail = crate::announcements::Detail {
            base: summary.clone(),
            message: Some("Read me".into()),
            locked_for_user: Some(true),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Announcement(Box::new(detail)));
        assert_eq!(app.announcement_status("10", &summary), "Read");
        let (k, r) = fetch_of(&app.update(key(KeyCode::Esc)));
        deliver(
            &mut app,
            k,
            r,
            Payload::Announcements(vec![summary.clone()]),
        );
        assert_eq!(app.announcement_status("10", &summary), "Read");
        assert_eq!(
            app.announcements["10"].data.as_ref().unwrap()[0]
                .read_state
                .as_deref(),
            Some("unread")
        );
        let restarted = App::new(&app.origin, Some(store));
        assert_eq!(restarted.announcement_status("10", &summary), "Read");
        assert_eq!(restarted.announcement_status("20", &summary), "Unread");
        let other = App::new(
            &app.origin,
            Some(Arc::new(SnapshotStore::new(&root, "other"))),
        );
        assert_eq!(other.announcement_status("10", &summary), "Unread");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unavailable_and_unseen_announcement_details_are_not_marked_read() {
        for (leave, restricted, fail) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut app = app_with_courses();
            app.update(key(KeyCode::Enter));
            let (k, r) = fetch_of(&app.update(key(KeyCode::BackTab)));
            let summary = crate::announcements::Summary {
                id: "8".into(),
                read_state: Some("unread".into()),
                ..Default::default()
            };
            deliver(
                &mut app,
                k,
                r,
                Payload::Announcements(vec![summary.clone()]),
            );
            let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
            if leave {
                app.update(key(KeyCode::Esc));
            }
            let detail = crate::announcements::Detail {
                base: summary.clone(),
                message: Some("Message".into()),
                lock_info: restricted.then_some(crate::announcements::LockInfo {
                    can_view: Some(false),
                }),
                ..Default::default()
            };
            let result = if fail {
                Err(CanvasError::NoAccess(403))
            } else {
                Ok((
                    Payload::Announcement(Box::new(detail)),
                    FetchStats::default(),
                ))
            };
            app.update(Msg::Fetched {
                key: k,
                request: r,
                result,
                at: Utc::now(),
            });
            assert_eq!(app.announcement_status("10", &summary), "Unread");
            if leave {
                // The completed detail is now displayed immediately on reentry.
                app.update(key(KeyCode::Enter));
                assert_eq!(app.announcement_status("10", &summary), "Read");
            }
        }
    }

    #[test]
    fn announcement_file_choices_refresh_metadata_before_reusing_local_files() {
        let (library, root) = temp_library("announcement-file");
        let mut app = app_with_courses();
        app.library = Some(library);
        let dir = app.announcement_dir("10", "8");
        let effects = app.place_announcement_file("9", dir.clone(), true);
        assert_eq!(fetch_of(&effects).0, ResourceKey::File("9".into()));
        let effects = deliver_file(&mut app, canvas_file("9", "notes.pdf", "1"));
        let download = effects
            .iter()
            .find(|e| matches!(e, Effect::Download { .. }))
            .unwrap();
        let result = finish_download(&mut app, download, "notes");
        assert!(result.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        // Even with a saved copy, check its current Canvas version on the next choice.
        let effects = app.place_announcement_file("9", dir, true);
        assert_eq!(fetch_of(&effects).0, ResourceKey::File("9".into()));
        assert!(!effects.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        let effects = deliver_file(&mut app, canvas_file("9", "notes.pdf", "1"));
        assert!(effects.iter().any(|e| matches!(e, Effect::OpenPath(_))));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Download { .. })));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn announcement_snapshots_show_while_syncing_and_closed_comments_keep_links() {
        let root =
            std::env::temp_dir().join(format!("tuicanvas-announcements-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "test"));
        let key = ResourceKey::Announcements("10".into());
        store.save(
            &key,
            &vec![crate::announcements::Summary {
                id: "8".into(),
                title: Some("Saved notice".into()),
                ..Default::default()
            }],
            Utc::now(),
        );
        let mut app = App::new("https://ufl.instructure.com", Some(store));
        app.stack.push(View::new(ViewKind::Announcements {
            course_id: "10".into(),
            course_name: "Course".into(),
        }));
        let effects = app.enter_current(false);
        assert_eq!(fetch_of(&effects).0, key);
        assert_eq!(app.announcements["10"].origin, Origin::Cache);
        assert!(app.current_syncing());
        assert_eq!(app.visible_rows(), [0]);
        let (key, request) = fetch_of(&app.update(super::tests::key(KeyCode::Enter)));
        let d = crate::announcements::Detail {
            base: crate::announcements::Summary {
                id: "8".into(),
                ..Default::default()
            },
            message: Some("<a href='https://example.org'>Schedule</a>".into()),
            locked_for_user: Some(true),
            ..Default::default()
        };
        deliver(&mut app, key, request, Payload::Announcement(Box::new(d)));
        assert_eq!(app.announcement_links("10", "8").len(), 1);
        let d = app
            .announcement_details
            .get_mut(&("10".into(), "8".into()))
            .unwrap()
            .data
            .as_mut()
            .unwrap();
        d.lock_info = Some(crate::announcements::LockInfo {
            can_view: Some(false),
        });
        assert!(app.announcement_links("10", "8").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_in_files_covers_the_whole_course() {
        let mut app = files_app(None);
        app.update(key(KeyCode::Char('/')));
        for c in "lecture".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert_eq!(row_names(&app), vec!["lecture1.pdf"]);
    }

    #[test]
    fn hidden_files_area_lists_nothing() {
        let mut app = app_with_courses();
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        deliver(&mut app, k, r, Payload::Assignments(vec![]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Tab)));
        let hidden = CourseFilesData {
            folders: course_files().folders,
            files: None,
        };
        deliver(&mut app, k, r, Payload::Files(Box::new(hidden)));
        assert!(app.file_rows().is_empty());
        assert!(app.update(key(KeyCode::Char('D'))).is_empty());
        assert!(app.notice.as_deref().unwrap_or("").contains("hidden"));
    }

    #[test]
    fn opening_a_file_downloads_it_once_then_opens_the_saved_copy() {
        let (lib, root) = temp_library("open");
        let mut app = files_app(Some(lib.clone()));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        let target = root
            .join("GPU Computing")
            .join("Class Files")
            .join("syllabus.pdf");
        assert!(
            matches!(&effects[..], [Effect::Download { target: t, automatic: false, .. }] if *t == target),
            "{effects:?}"
        );
        assert_eq!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Queued
        );
        assert_eq!(
            finish_download(&mut app, &effects[0], "hello"),
            vec![Effect::OpenPath(target.clone())]
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Saved
        );
        // Up to date: opens straight away, no second download.
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Effect::OpenPath(target)]
        );
        // A newer version on Canvas is shown as such.
        let mut newer = canvas_file("100", "syllabus.pdf", "1");
        newer.updated_at = Some("2026-10-02T12:00:00Z".into());
        assert_eq!(app.local_state(&newer), Local::Changed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn download_all_takes_subfolders_and_keeps_canvas_folders() {
        let (lib, root) = temp_library("all");
        let mut app = files_app(Some(lib));
        let effects = app.update(key(KeyCode::Char('D')));
        let mut targets: Vec<PathBuf> = effects
            .iter()
            .map(|e| match e {
                Effect::Download { target, .. } => {
                    target.strip_prefix(&root).unwrap().to_path_buf()
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        targets.sort();
        let class = PathBuf::from("GPU Computing").join("Class Files");
        assert_eq!(
            targets,
            vec![
                class.join("Lectures").join("Notes.txt"),
                class.join("Lectures").join("Week 1").join("lecture1.pdf"),
                class.join("syllabus.pdf"),
            ]
        );
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .starts_with("Downloading 3 of 3 files")
        );
        // Pressing again while they run starts nothing new.
        assert!(app.update(key(KeyCode::Char('D'))).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    fn detail_app(library: Option<Arc<Library>>, description: &str) -> App {
        let mut app = app_with_courses();
        app.library = library;
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let a = AssignmentSummary {
            id: "7".into(),
            name: Some("Lab-2".into()),
            ..Default::default()
        };
        deliver(&mut app, k, r, Payload::Assignments(vec![a.clone()]));
        let (k, r) = fetch_of(&app.update(key(KeyCode::Enter)));
        let detail = AssignmentDetail {
            base: a,
            description: Some(description.into()),
            ..Default::default()
        };
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::new(detail)), FetchStats::default())),
            at: Utc::now(),
        });
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(
                    e,
                    Effect::Fetch {
                        key: ResourceKey::File(_),
                        ..
                    }
                ))
                .count(),
            3,
            "every linked file's details are read"
        );
        app.notice = None;
        app
    }

    fn deliver_file(app: &mut App, file: CanvasFile) -> Vec<Effect> {
        let key = ResourceKey::File(file.id.clone());
        let request = match app.sync_of(&key) {
            Some(SyncState::Syncing { request, .. }) => *request,
            other => panic!("no read in flight for {key:?}: {other:?}"),
        };
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((Payload::File(Box::new(file)), FetchStats::default())),
            at: Utc::now(),
        })
    }

    const DESCRIPTION: &str = r##"<p>Read <a href="/courses/10/files/22/download?verifier=v">the handout</a>,
        see <a href="https://github.com/a/b">the repo</a> and <a href="/courses/10/assignments/8">Lab-1</a>.</p>
        <p><img src="/courses/10/files/33/preview" alt="chart"><img src="/courses/10/files/34/preview" alt="chart 2"></p>"##;

    #[test]
    fn assignment_images_download_automatically_and_names_never_collide() {
        let (lib, root) = temp_library("images");
        let mut app = detail_app(Some(lib), DESCRIPTION);
        assert_eq!(app.links_of("10", "7").len(), 5);
        // The handout is only read; images are fetched too, each under its own name.
        assert!(deliver_file(&mut app, canvas_file("22", "handout.pdf", "1")).is_empty());
        let first = deliver_file(&mut app, canvas_file("33", "image.png", "1"));
        let second = deliver_file(&mut app, canvas_file("34", "image.png", "1"));
        let dir = root
            .join("GPU Computing")
            .join("Assignment Files")
            .join("Lab-2");
        assert!(
            matches!(&first[..], [Effect::Download { target, automatic: true, .. }] if *target == dir.join("image.png")),
            "{first:?}"
        );
        assert!(
            matches!(&second[..], [Effect::Download { target, automatic: true, .. }] if *target == dir.join("image (34).png")),
            "{second:?}"
        );
        assert!(
            finish_download(&mut app, &first[0], "a").is_empty(),
            "automatic downloads open nothing"
        );
        assert!(app.notice.is_none(), "and say nothing");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn links_list_selects_and_opens_each_kind() {
        let (lib, root) = temp_library("links");
        let mut app = detail_app(Some(lib), DESCRIPTION);
        // Tab selects the first entry: the handout, whose details are still being read.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.view().link, Some(0));
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "waits for the file's details"
        );
        let effects = deliver_file(&mut app, canvas_file("22", "handout.pdf", "1"));
        let target = root
            .join("GPU Computing")
            .join("Assignment Files")
            .join("Lab-2")
            .join("handout.pdf");
        assert!(
            matches!(&effects[..], [Effect::Download { target: t, automatic: false, .. }] if *t == target),
            "{effects:?}"
        );
        assert_eq!(
            finish_download(&mut app, &effects[0], "pdf"),
            vec![Effect::OpenPath(target)]
        );
        // A web link opens in the browser; o on a file opens its Canvas page.
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/files/22?verifier=v")
        );
        app.update(key(KeyCode::Tab));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Effect::OpenUrl("https://github.com/a/b".into())]
        );
        // Another assignment opens in the reader.
        app.update(key(KeyCode::Char('3')));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Detail("10".into(), "8".into()));
        assert!(matches!(&app.view().kind, ViewKind::Detail { title, .. } if title == "Lab-1"));
        // Back again: Esc first clears the selection, then leaves.
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::Tab));
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(app.view().link, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_download_reports_and_flags_sign_in() {
        let (lib, root) = temp_library("fail");
        let mut app = files_app(Some(lib));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Char('d')));
        assert!(matches!(&effects[..], [Effect::Download { .. }]));
        app.update(Msg::Transfer {
            file_id: "100".into(),
            event: TransferEvent::Failed {
                message: "signed out or session expired".into(),
                needs_login: true,
            },
        });
        assert!(app.login_required);
        assert!(app.notice.as_deref().unwrap().contains("syllabus.pdf"));
        assert!(matches!(
            app.local_state(&canvas_file("100", "syllabus.pdf", "1")),
            Local::Failed(_)
        ));
        // Trying again starts a new download.
        assert!(matches!(
            &app.update(key(KeyCode::Char('d')))[..],
            [Effect::Download { .. }]
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn locked_files_are_not_downloaded() {
        let (lib, root) = temp_library("locked");
        let mut app = files_app(Some(lib));
        if let Some(files) = app
            .course_files
            .get_mut("10")
            .and_then(|r| r.data.as_mut())
            .and_then(|d| d.files.as_mut())
        {
            files[0].url = None;
            files[0].locked_for_user = Some(true);
        }
        app.update(key(KeyCode::Down));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.notice.as_deref().unwrap().contains("locked"));
        let _ = std::fs::remove_dir_all(root);
    }
}
