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
use crate::keys::{self, Act, Scope};
use crate::library::{self, Library, LocalState};
use crate::model::{AssignmentDetail, AssignmentSummary, CanvasFile, CanvasFolder, Course};
use crate::modules::ItemKind;
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
    Planner(Vec<crate::calendar::Item>),
    Colors(HashMap<String, String>),
    Tabs(Vec<crate::modules::Tab>),
    Modules(Vec<crate::modules::Module>),
    WikiPage(Box<crate::modules::WikiPage>),
}

/// One row of the Modules screen: a module's heading or one of its items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModuleRow<'a> {
    Module(&'a crate::modules::Module),
    Item(&'a crate::modules::Module, &'a crate::modules::Item),
}

impl ModuleRow<'_> {
    /// Headings (modules and text headers) are not selectable.
    pub fn selectable(&self) -> bool {
        matches!(self, ModuleRow::Item(_, i) if i.kind() != crate::modules::ItemKind::Heading)
    }
}

/// A course section the reader can show, when Canvas shows its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CourseTab {
    /// The course's Canvas home, when it is a page.
    Home,
    Assignments,
    Announcements,
    Files,
    Modules,
}

impl CourseTab {
    pub fn label(self) -> &'static str {
        match self {
            CourseTab::Home => "Home",
            CourseTab::Assignments => "Assignments",
            CourseTab::Announcements => "Announcements",
            CourseTab::Files => "Files",
            CourseTab::Modules => "Modules",
        }
    }

    fn of_canvas(id: &str) -> Option<CourseTab> {
        match id {
            "assignments" => Some(CourseTab::Assignments),
            "announcements" => Some(CourseTab::Announcements),
            "files" => Some(CourseTab::Files),
            "modules" => Some(CourseTab::Modules),
            _ => None,
        }
    }

    pub fn view(self, course_id: String, course_name: String) -> ViewKind {
        match self {
            CourseTab::Home => ViewKind::WikiPage {
                course_id,
                course_name,
                page_url: crate::modules::FRONT_PAGE.to_string(),
                title: "Home".into(),
            },
            CourseTab::Assignments => ViewKind::Assignments {
                course_id,
                course_name,
            },
            CourseTab::Announcements => ViewKind::Announcements {
                course_id,
                course_name,
            },
            CourseTab::Files => ViewKind::Files {
                course_id,
                course_name,
                folder_id: None,
            },
            CourseTab::Modules => ViewKind::Modules {
                course_id,
                course_name,
            },
        }
    }
}

/// A module file waiting for its details before it can be placed.
#[derive(Clone, Debug, PartialEq)]
struct ModuleFile {
    course_id: String,
    module: String,
    open: bool,
    quiet: bool,
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

/// What has focus on an assignment or announcement page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Text,
    Links,
    Submissions,
    /// The month grid on the course list (a day is selected).
    Calendar,
    /// The selected day's entries, listed under the grid.
    Day,
}

/// The actions menu (`Space`).
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub selected: usize,
    /// The search text, while searching.
    pub search: Option<String>,
}

/// A screen showing one Canvas item with text, files and links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Assignment(String, String),
    Announcement(String, String),
    /// A course wiki page: course id and the page's URL name.
    Wiki(String, String),
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
    Modules {
        course_id: String,
        course_name: String,
    },
    WikiPage {
        course_id: String,
        course_name: String,
        page_url: String,
        title: String,
    },
    /// A course where Canvas shows none of the sections the reader has.
    CourseHome {
        course_id: String,
        course_name: String,
    },
}

impl ViewKind {
    pub fn page(&self) -> Option<Page> {
        match self {
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => Some(Page::Assignment(course_id.clone(), assignment_id.clone())),
            ViewKind::Announcement {
                course_id,
                announcement_id,
                ..
            } => Some(Page::Announcement(
                course_id.clone(),
                announcement_id.clone(),
            )),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => Some(Page::Wiki(course_id.clone(), page_url.clone())),
            _ => None,
        }
    }

    /// The course section this view is, with the course.
    pub fn tab(&self) -> Option<(CourseTab, &str, &str)> {
        match self {
            ViewKind::Assignments {
                course_id,
                course_name,
            } => Some((CourseTab::Assignments, course_id, course_name)),
            ViewKind::Announcements {
                course_id,
                course_name,
            } => Some((CourseTab::Announcements, course_id, course_name)),
            ViewKind::Files {
                course_id,
                course_name,
                ..
            } => Some((CourseTab::Files, course_id, course_name)),
            ViewKind::Modules {
                course_id,
                course_name,
            } => Some((CourseTab::Modules, course_id, course_name)),
            ViewKind::WikiPage {
                course_id,
                course_name,
                page_url,
                ..
            } if page_url == crate::modules::FRONT_PAGE => {
                Some((CourseTab::Home, course_id, course_name))
            }
            _ => None,
        }
    }

    /// The course of a course section or the empty course screen.
    fn course(&self) -> Option<(&str, &str)> {
        match self {
            ViewKind::CourseHome {
                course_id,
                course_name,
            } => Some((course_id, course_name)),
            _ => self.tab().map(|(_, c, n)| (c, n)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub kind: ViewKind,
    pub selected: usize,
    pub offset: usize,
    pub filter: String,
    pub filtering: bool,
    pub scroll: usize,
    /// Focused section on a page.
    pub focus: Section,
    /// Selected entry in a page's files & links.
    pub link: usize,
    /// Selected attempt in an assignment's submissions (0 = latest).
    pub attempt: usize,
    /// Bring the focused section into view on the next draw.
    pub reveal: bool,
}

impl View {
    pub fn new(kind: ViewKind) -> Self {
        View {
            kind,
            selected: 0,
            offset: 0,
            filter: String::new(),
            filtering: false,
            scroll: 0,
            focus: Section::Text,
            link: 0,
            attempt: 0,
            reveal: false,
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
            ViewKind::Modules { course_id, .. } => ResourceKey::Modules(course_id.clone()),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => ResourceKey::WikiPage(course_id.clone(), page_url.clone()),
            ViewKind::CourseHome { course_id, .. } => ResourceKey::Tabs(course_id.clone()),
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
    /// The home-screen calendar, one read per month shown (`2026-10`).
    pub planner: HashMap<String, Resource<Vec<crate::calendar::Item>>>,
    /// Course colours chosen in Canvas, by course id.
    pub colors: Resource<HashMap<String, String>>,
    /// The day selected on the calendar; its month is the one shown.
    pub calendar_day: chrono::NaiveDate,
    /// The selected entry of that day.
    pub day_item: usize,
    pub announcements: HashMap<String, Resource<Vec<crate::announcements::Summary>>>,
    pub announcement_details: HashMap<(String, String), Resource<crate::announcements::Detail>>,
    pub announcement_render: Option<DetailRender>,
    announcement_reads: std::collections::BTreeSet<(String, String)>,
    pub hidden_courses: std::collections::BTreeSet<String>,
    /// Hidden courses are listed (marked) so they can be restored.
    pub show_hidden: bool,
    pub assignments: HashMap<String, Resource<Vec<AssignmentSummary>>>,
    pub details: HashMap<(String, String), Resource<AssignmentDetail>>,
    pub histories: HashMap<(String, String), Resource<submission::History>>,
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
    pub menu: Option<MenuState>,
    /// The help screen, with its scroll position.
    pub help: Option<usize>,
    /// Course tabs Canvas shows; read once per run (and on `r`).
    pub tabs: HashMap<String, Resource<Vec<crate::modules::Tab>>>,
    tabs_read: std::collections::HashSet<String>,
    pub modules: HashMap<String, Resource<Vec<crate::modules::Module>>>,
    pub wiki_pages: HashMap<(String, String), Resource<crate::modules::WikiPage>>,
    pub wiki_render: Option<DetailRender>,
    /// Module files waiting for their details (and their course's folders).
    module_waiting: HashMap<String, ModuleFile>,
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
            planner: HashMap::new(),
            colors: Resource::default(),
            calendar_day: chrono::Local::now().date_naive(),
            day_item: 0,
            announcements: HashMap::new(),
            announcement_details: HashMap::new(),
            announcement_render: None,
            announcement_reads,
            hidden_courses,
            show_hidden: false,
            assignments: HashMap::new(),
            details: HashMap::new(),
            histories: HashMap::new(),
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
            menu: None,
            help: None,
            tabs: HashMap::new(),
            tabs_read: std::collections::HashSet::new(),
            modules: HashMap::new(),
            wiki_pages: HashMap::new(),
            wiki_render: None,
            module_waiting: HashMap::new(),
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
            ResourceKey::Planner(m) => self.planner.get(m).map(|r| &r.sync),
            ResourceKey::Colors => Some(&self.colors.sync),
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
            ResourceKey::Tabs(c) => self.tabs.get(c).map(|r| &r.sync),
            ResourceKey::Modules(c) => self.modules.get(c).map(|r| &r.sync),
            ResourceKey::WikiPage(c, u) => self
                .wiki_pages
                .get(&(c.clone(), u.clone()))
                .map(|r| &r.sync),
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
            ResourceKey::Planner(m) => self.planner.entry(m.clone()).or_default().sync = sync,
            ResourceKey::Colors => self.colors.sync = sync,
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
            ResourceKey::Tabs(c) => self.tabs.entry(c.clone()).or_default().sync = sync,
            ResourceKey::Modules(c) => self.modules.entry(c.clone()).or_default().sync = sync,
            ResourceKey::WikiPage(c, u) => {
                self.wiki_pages
                    .entry((c.clone(), u.clone()))
                    .or_default()
                    .sync = sync
            }
        }
    }

    pub fn current_syncing(&self) -> bool {
        matches!(
            self.sync_of(&self.view().key()),
            Some(SyncState::Syncing { .. })
        )
    }

    /// Show a saved copy (if any and nothing newer is loaded), then request fresh data.
    fn enter_current(&mut self, explicit: bool) -> Vec<Effect> {
        let key = self.view().key();
        self.load_snapshot(&key);
        self.clamp_selection();
        self.mark_current_announcement_read();
        self.dirty = true;
        // Course tabs are read once per run, not on every visit.
        let mut effects: Vec<Effect> = match key {
            ResourceKey::Tabs(_) => vec![],
            key => self.request(key, explicit).into_iter().collect(),
        };
        if let Some((course_id, _)) = self.view().kind.course() {
            let course_id = course_id.to_string();
            effects.extend(self.read_tabs(&course_id, explicit));
        }
        // The calendar is part of the home screen: read it with the course list.
        if self.view().kind == ViewKind::Courses {
            effects.extend(self.read_calendar(explicit));
        }
        // Past submissions are part of the assignment page: read them with it
        // when the copy on hand shows something was submitted.
        if let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = self.view().kind.clone()
            && self.was_submitted(&course_id, &assignment_id)
        {
            effects.extend(self.request(ResourceKey::History(course_id, assignment_id), explicit));
        }
        effects
    }

    /// Whether Canvas shows any submission for this assignment.
    fn was_submitted(&self, course_id: &str, assignment_id: &str) -> bool {
        self.details
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|d| d.base.submission.as_ref())
            .is_some_and(|s| s.submitted_at.is_some() || s.attempt.unwrap_or(0) > 0)
    }

    pub fn attempt_count(&self, course_id: &str, assignment_id: &str) -> usize {
        self.histories
            .get(&(course_id.to_string(), assignment_id.to_string()))
            .and_then(|r| r.data.as_ref())
            .map_or(0, |h| h.attempts.len())
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
            ResourceKey::Planner(m) => {
                let res = self.planner.entry(m.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Colors if self.colors.data.is_none() => {
                if let Some((data, at)) = store.load(key) {
                    self.colors.data = Some(data);
                    self.colors.fetched_at = Some(at);
                    self.colors.origin = Origin::Cache;
                    self.colors.version += 1;
                }
            }
            ResourceKey::Colors => {}
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
            ResourceKey::Tabs(c) => {
                let res = self.tabs.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::Modules(c) => {
                let res = self.modules.entry(c.clone()).or_default();
                if res.data.is_none()
                    && let Some((data, at)) = store.load(key)
                {
                    res.data = Some(data);
                    res.fetched_at = Some(at);
                    res.origin = Origin::Cache;
                    res.version += 1;
                }
            }
            ResourceKey::WikiPage(c, u) => {
                let res = self.wiki_pages.entry((c.clone(), u.clone())).or_default();
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
                if key == self.view().key() {
                    self.last_stats = Some((key.label(), stats));
                }
                let mut effects = Vec::new();
                match (payload, &key) {
                    (Payload::Planner(items), ResourceKey::Planner(m)) => {
                        let r = self.planner.entry(m.clone()).or_default();
                        r.data = Some(items);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let n = self.day_items(self.calendar_day).len();
                        self.day_item = self.day_item.min(n.saturating_sub(1));
                    }
                    (Payload::Colors(colors), ResourceKey::Colors) => {
                        let r = &mut self.colors;
                        r.data = Some(colors);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
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
                        if self.was_submitted(c, a) {
                            effects.extend(
                                self.request(ResourceKey::History(c.clone(), a.clone()), false),
                            );
                        }
                    }
                    (Payload::History(history), ResourceKey::History(c, a)) => {
                        let r = self.histories.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(history);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        let count = r.data.as_ref().map_or(0, |h| h.attempts.len());
                        for view in &mut self.stack {
                            if view.kind.page() == Some(Page::Assignment(c.clone(), a.clone())) {
                                view.attempt = view.attempt.min(count.saturating_sub(1));
                            }
                        }
                    }
                    (Payload::Files(data), ResourceKey::Files(c)) => {
                        let r = self.course_files.entry(c.clone()).or_default();
                        r.data = Some(*data);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.place_module_files());
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
                        effects.extend(self.place_module_files());
                    }
                    (Payload::Tabs(list), ResourceKey::Tabs(c)) => {
                        let r = self.tabs.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                        effects.extend(self.follow_tabs());
                    }
                    (Payload::Modules(list), ResourceKey::Modules(c)) => {
                        let r = self.modules.entry(c.clone()).or_default();
                        r.data = Some(list);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
                    }
                    (Payload::WikiPage(page), ResourceKey::WikiPage(c, u)) => {
                        let r = self.wiki_pages.entry((c.clone(), u.clone())).or_default();
                        r.data = Some(*page);
                        r.origin = Origin::Network;
                        r.fetched_at = Some(at);
                        r.sync = SyncState::Idle;
                        r.version += 1;
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
                if let ResourceKey::File(f) = &key
                    && let Some(m) = self.module_waiting.remove(f)
                    && !m.quiet
                {
                    self.notice = Some(format!(
                        "Could not read that file from Canvas ({}). Press o to open it in the browser.",
                        error.describe()
                    ));
                }
                self.set_sync(&key, SyncState::Failed(error));
                // Without the course's folders a module file goes to Module Files.
                let mut effects = if matches!(key, ResourceKey::Files(_)) {
                    self.place_module_files()
                } else {
                    vec![]
                };
                if needs_login {
                    self.login_required = true;
                    // A session found dead at startup goes straight to sign-in;
                    // later expiry waits for the user to press L.
                    if first_result {
                        effects.push(Effect::Login);
                    }
                }
                effects
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
                .filter(|(_, c)| self.show_hidden || !self.hidden_courses.contains(&c.id))
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
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::CourseHome { .. } => vec![],
            ViewKind::Files { .. } => (0..self.file_rows().len()).collect(),
            ViewKind::Modules { course_id, .. } => self.visible_modules(course_id, &view.filter),
        }
    }

    /// Whether each visible row can be selected (module headings cannot).
    fn row_selectable(&self) -> Vec<bool> {
        match &self.view().kind {
            ViewKind::Modules { course_id, .. } => {
                let rows = self.module_rows(course_id);
                self.visible_rows()
                    .iter()
                    .map(|&i| rows.get(i).is_some_and(|r| r.selectable()))
                    .collect()
            }
            _ => vec![true; self.visible_rows().len()],
        }
    }

    /// The nearest selectable row from `at`, looking in `step`'s direction first.
    fn nearest_selectable(selectable: &[bool], at: usize, step: isize) -> usize {
        let len = selectable.len() as isize;
        let first = if step < 0 { -1 } else { 1 };
        for dir in [first, -first] {
            let mut i = at as isize;
            while (0..len).contains(&i) {
                if selectable[i as usize] {
                    return i as usize;
                }
                i += dir;
            }
        }
        at
    }

    fn clamp_selection(&mut self) {
        let selectable = self.row_selectable();
        let len = selectable.len();
        let view = self.view_mut();
        if view.selected >= len {
            view.selected = len.saturating_sub(1);
        }
        if len > 0 && !selectable[view.selected] {
            view.selected = Self::nearest_selectable(&selectable, view.selected, 1);
        }
    }

    /// Every module heading and item of a course, in order.
    pub fn module_rows(&self, course_id: &str) -> Vec<ModuleRow<'_>> {
        let mut rows = Vec::new();
        for m in self
            .modules
            .get(course_id)
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
        {
            rows.push(ModuleRow::Module(m));
            rows.extend(m.items().iter().map(|i| ModuleRow::Item(m, i)));
        }
        rows
    }

    /// Module rows matching a search: matching items under their module's
    /// heading, or a whole module whose name matches.
    fn visible_modules(&self, course_id: &str, filter: &str) -> Vec<usize> {
        let modules = self
            .modules
            .get(course_id)
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[]);
        let mut out = Vec::new();
        let mut at = 0;
        for m in modules {
            let head = at;
            let whole = matches_filter(m.name(), filter);
            let hits: Vec<usize> = m
                .items()
                .iter()
                .enumerate()
                .filter(|(_, i)| whole || matches_filter(i.title(), filter))
                .map(|(k, _)| head + 1 + k)
                .collect();
            if whole || !hits.is_empty() {
                out.push(head);
                out.extend(hits);
            }
            at += 1 + m.items().len();
        }
        out
    }

    /// The selected row of the Modules screen.
    pub fn selected_module_row(&self) -> Option<ModuleRow<'_>> {
        let ViewKind::Modules { course_id, .. } = &self.view().kind else {
            return None;
        };
        let index = *self.visible_rows().get(self.view().selected)?;
        self.module_rows(course_id).get(index).copied()
    }

    /// The course sections Canvas shows, in its order. The course home comes
    /// first when it is modules or assignments, since Canvas shows those on
    /// Home even with their own tab hidden. Before the tabs are known, all.
    pub fn course_tabs(&self, course_id: &str) -> Vec<CourseTab> {
        // Canvas's Home shows the course's default view: a page, modules or
        // assignments (a syllabus or activity feed home is not shown here).
        let home = match self
            .courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|c| c.id == course_id)
            .and_then(|c| c.default_view.as_deref())
        {
            Some("wiki") => Some(CourseTab::Home),
            Some("modules") => Some(CourseTab::Modules),
            Some("assignments") => Some(CourseTab::Assignments),
            _ => None,
        };
        let mut out: Vec<CourseTab> = home.into_iter().collect();
        let Some(list) = self.tabs.get(course_id).and_then(|r| r.data.as_ref()) else {
            for tab in [
                CourseTab::Assignments,
                CourseTab::Files,
                CourseTab::Announcements,
                CourseTab::Modules,
            ] {
                if !out.contains(&tab) {
                    out.push(tab);
                }
            }
            return out;
        };
        if !list.iter().any(|t| t.id == "home") {
            out.clear();
        }
        for tab in list.iter().filter_map(|t| CourseTab::of_canvas(&t.id)) {
            if !out.contains(&tab) {
                out.push(tab);
            }
        }
        out
    }

    /// Where a course opens: its first section, or the empty course screen.
    fn course_view(&self, course_id: &str, course_name: &str) -> ViewKind {
        match self.course_tabs(course_id).first() {
            Some(tab) => tab.view(course_id.to_string(), course_name.to_string()),
            None => ViewKind::CourseHome {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
            },
        }
    }

    /// Read a course's tabs: from the saved copy now, from Canvas once per run.
    fn read_tabs(&mut self, course_id: &str, explicit: bool) -> Vec<Effect> {
        let key = ResourceKey::Tabs(course_id.to_string());
        self.load_snapshot(&key);
        if !self.tabs_read.insert(course_id.to_string()) && !explicit {
            return vec![];
        }
        self.request(key, explicit).into_iter().collect()
    }

    /// After the tabs change: leave a section the instructor hid.
    fn follow_tabs(&mut self) -> Vec<Effect> {
        let kind = self.view().kind.clone();
        let Some((course_id, course_name)) = kind.course() else {
            return vec![];
        };
        let tabs = self.course_tabs(course_id);
        let shown = match kind.tab() {
            Some((tab, ..)) => tabs.contains(&tab),
            None => tabs.is_empty(),
        };
        if shown {
            return vec![];
        }
        let next = self.course_view(course_id, course_name);
        self.replace_tab(next)
    }

    /// Show another section of the same course in place of this one.
    fn replace_tab(&mut self, next: ViewKind) -> Vec<Effect> {
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

    fn page(&self) -> usize {
        usize::from(self.size.1.saturating_sub(4)).max(1)
    }

    fn is_list(&self) -> bool {
        !matches!(
            self.view().kind,
            ViewKind::Detail { .. } | ViewKind::Announcement { .. } | ViewKind::WikiPage { .. }
        )
    }

    fn move_selection(&mut self, delta: isize) {
        if self.is_list() {
            let selectable = self.row_selectable();
            let len = selectable.len();
            let view = self.view_mut();
            if len == 0 {
                view.selected = 0;
                return;
            }
            let target = (view.selected as isize + delta).clamp(0, len as isize - 1) as usize;
            view.selected = Self::nearest_selectable(&selectable, target, delta);
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
                }
                | ViewKind::Modules {
                    course_id: c,
                    course_name,
                }
                | ViewKind::WikiPage {
                    course_id: c,
                    course_name,
                    ..
                }
                | ViewKind::Announcements {
                    course_id: c,
                    course_name,
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

    // ---- Keyboard ----
    //
    // Keys are looked up in `keys::BINDINGS` for the scopes active now, and the
    // action they name is carried out here. Nothing below matches on raw keys
    // except text typed into a text box.

    /// The key scopes active now.
    pub fn scopes(&self) -> Vec<Scope> {
        if self.help.is_some() {
            return vec![Scope::Help];
        }
        if let Some(menu) = &self.menu {
            return vec![if menu.search.is_some() {
                Scope::MenuSearch
            } else {
                Scope::Menu
            }];
        }
        self.screen_scopes()
    }

    /// Scopes of the screen under any open menu or help.
    pub fn screen_scopes(&self) -> Vec<Scope> {
        if let Some(draft) = &self.submission {
            return if draft.busy {
                vec![]
            } else if draft.reviewing {
                vec![Scope::Review]
            } else {
                vec![Scope::PathInput]
            };
        }
        let view = self.view();
        if view.filtering {
            return vec![Scope::Filter];
        }
        let mut scopes = vec![Scope::Screen];
        if self.login_required {
            scopes.push(Scope::SignedOut);
        }
        match &view.kind {
            ViewKind::Courses => match self.focus() {
                Section::Calendar => scopes.extend([Scope::Courses, Scope::Calendar]),
                Section::Day => scopes.extend([Scope::Courses, Scope::Day]),
                _ => scopes.extend([Scope::List, Scope::Courses]),
            },
            ViewKind::Assignments { .. } | ViewKind::Announcements { .. } => {
                scopes.extend([Scope::List, Scope::CourseTabs])
            }
            // Modules share the Files keys: d, D and e work on module files.
            ViewKind::Files { .. } | ViewKind::Modules { .. } => {
                scopes.extend([Scope::List, Scope::CourseTabs, Scope::Files])
            }
            ViewKind::CourseHome { .. } => {}
            ViewKind::Detail { .. } | ViewKind::Announcement { .. } | ViewKind::WikiPage { .. } => {
                scopes.push(Scope::Page);
                // On a course's Home, Tab moves between the course's tabs.
                scopes.push(if view.kind.tab().is_some() {
                    Scope::CourseTabs
                } else {
                    Scope::Sections
                });
                if matches!(view.kind, ViewKind::Detail { .. }) {
                    scopes.push(Scope::Assignment);
                }
                if !self.current_links().is_empty() {
                    scopes.push(Scope::HasLinks);
                }
                match self.focus() {
                    Section::Links => scopes.push(Scope::Links),
                    Section::Submissions => scopes.push(Scope::Submissions),
                    Section::Text | Section::Calendar | Section::Day => {}
                }
            }
        }
        scopes
    }

    /// Sections the current page has, in order: text, files & links, submissions.
    pub fn sections(&self) -> Vec<Section> {
        if self.view().kind == ViewKind::Courses {
            return vec![Section::Text, Section::Calendar];
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        let mut out = vec![Section::Text];
        if !self.page_links(&page).is_empty() {
            out.push(Section::Links);
        }
        if let Page::Assignment(c, a) = &page
            && self.attempt_count(c, a) > 0
        {
            out.push(Section::Submissions);
        }
        out
    }

    /// The focused section, falling back to the text if it went away.
    pub fn focus(&self) -> Section {
        let focus = self.view().focus;
        if focus == Section::Day {
            return if self.day_items(self.calendar_day).is_empty() {
                Section::Calendar
            } else {
                Section::Day
            };
        }
        if focus == Section::Text || self.sections().contains(&focus) {
            focus
        } else {
            Section::Text
        }
    }

    /// Files and links of the current page.
    pub fn current_links(&self) -> Vec<Link> {
        self.view()
            .kind
            .page()
            .map(|p| self.page_links(&p))
            .unwrap_or_default()
    }

    pub fn page_links(&self, page: &Page) -> Vec<Link> {
        match page {
            Page::Assignment(c, a) => self.links_of(c, a),
            Page::Announcement(c, a) => self.announcement_links(c, a),
            Page::Wiki(c, u) => self.wiki_links(c, u),
        }
    }

    fn page_dir(&self, page: &Page) -> Option<PathBuf> {
        match page {
            Page::Assignment(c, a) => self.detail_dir(c, a),
            Page::Announcement(c, a) => self.announcement_dir(c, a),
            Page::Wiki(c, u) => self.wiki_dir(c, u),
        }
    }

    /// The selected entry of the files & links section, while it has focus.
    pub fn selected_link(&self) -> Option<Link> {
        if self.focus() != Section::Links {
            return None;
        }
        self.current_links().get(self.view().link).cloned()
    }

    /// Whether an action does something here (for the hint line and the menu).
    pub fn available(&self, act: Act) -> bool {
        if self.help.is_some()
            || self.menu.is_some()
            || self.submission.is_some()
            || self.view().filtering
        {
            return true;
        }
        self.screen_available(act)
    }

    fn screen_available(&self, act: Act) -> bool {
        let page = self.view().kind.page();
        match act {
            Act::Open => match (page, self.focus()) {
                (Some(_), focus) => focus == Section::Links,
                (None, Section::Calendar | Section::Day) => {
                    !self.day_items(self.calendar_day).is_empty()
                }
                (None, _) => !self.visible_rows().is_empty(),
            },
            Act::Download => match self.view().kind {
                ViewKind::Files { .. } => !self.file_rows().is_empty(),
                ViewKind::Modules { .. } => matches!(
                    self.selected_module_row(),
                    Some(ModuleRow::Item(_, i)) if i.kind() == ItemKind::File
                ),
                _ => matches!(
                    self.selected_link().map(|l| l.target),
                    Some(Target::File { .. })
                ),
            },
            Act::DownloadAll => match &self.view().kind {
                ViewKind::Files { .. } => !self.file_rows().is_empty(),
                ViewKind::Modules { course_id, .. } => self
                    .module_rows(course_id)
                    .iter()
                    .any(|r| matches!(r, ModuleRow::Item(_, i) if i.kind() == ItemKind::File)),
                _ => self
                    .current_links()
                    .iter()
                    .any(|l| matches!(l.target, Target::File { .. })),
            },
            Act::ShowFolder | Act::OpenLibrary => self.library.is_some(),
            Act::Submit => match &self.view().kind {
                ViewKind::Detail {
                    course_id,
                    assignment_id,
                    ..
                } => self
                    .details
                    .get(&(course_id.clone(), assignment_id.clone()))
                    .and_then(|r| r.data.as_ref())
                    .is_some_and(|d| d.base.submission_types.iter().any(|t| t == "online_upload")),
                _ => false,
            },
            Act::NextSection | Act::PrevSection => match self.view().kind.tab() {
                Some((_, course_id, _)) => self.course_tabs(course_id).len() > 1,
                None => self.sections().len() > 1,
            },
            Act::HideCourse => !self.visible_rows().is_empty(),
            Act::Search => self.is_list(),
            _ => true,
        }
    }

    /// The actions menu's entries for the screen under it, filtered by its search.
    pub fn menu_entries(&self) -> Vec<&'static keys::Binding> {
        let scopes = self.screen_scopes();
        let search = self
            .menu
            .as_ref()
            .and_then(|m| m.search.as_deref())
            .unwrap_or("")
            .to_lowercase();
        keys::menu(&scopes, &|a| self.screen_available(a))
            .into_iter()
            .filter(|b| search.is_empty() || self.title_of(b).to_lowercase().contains(&search))
            .collect()
    }

    /// An action's name in the menu and help, adjusted to what is selected.
    pub fn title_of(&self, binding: &keys::Binding) -> String {
        match binding.act {
            Act::HideCourse => {
                let hidden = self
                    .visible_rows()
                    .get(self.view().selected)
                    .and_then(|&i| self.courses.data.as_ref()?.get(i))
                    .is_some_and(|c| self.hidden_courses.contains(&c.id));
                if hidden {
                    "Restore this course"
                } else {
                    "Hide this course"
                }
                .into()
            }
            Act::ShowHidden if self.show_hidden => "Stop showing hidden courses".into(),
            Act::ShowHidden => "Show hidden courses".into(),
            _ => binding.title.into(),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![];
        }
        let scopes = self.scopes();
        if let Some(binding) = keys::lookup(keys::key_of(&key), &scopes) {
            return self.perform(binding.act, &key);
        }
        if keys::typing(&scopes) {
            self.type_text(&key);
            return vec![];
        }
        // Letters in the actions menu run the entry they label.
        if scopes.contains(&Scope::Menu)
            && let KeyCode::Char(c) = key.code
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && let Some(entry) = self.menu_entries().into_iter().find(|e| e.menu == Some(c))
        {
            self.menu = None;
            return self.perform(entry.act, &key);
        }
        vec![]
    }

    /// Text typed into the search box, the menu's search, or a file path.
    fn type_text(&mut self, key: &KeyEvent) {
        let typed = match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(c)
            }
            _ => None,
        };
        let erase = key.code == KeyCode::Backspace;
        if let Some(menu) = &mut self.menu {
            let search = menu.search.get_or_insert_with(String::new);
            if erase {
                search.pop();
            } else if let Some(c) = typed
                && search.chars().count() < 40
            {
                search.push(c);
            }
            menu.selected = 0;
            return;
        }
        if let Some(draft) = &mut self.submission {
            // A held Backspace would wipe the path; one character per press.
            if erase && key.kind != KeyEventKind::Repeat {
                draft.input.pop();
            } else if let Some(c) = typed
                && draft.input.chars().count() < 1024
            {
                draft.input.push(c);
            } else {
                return;
            }
            draft.suggestions.update(&draft.input);
            return;
        }
        let view = self.view_mut();
        if erase {
            view.filter.pop();
        } else if let Some(c) = typed
            && view.filter.chars().count() < 64
        {
            view.filter.push(c);
        } else {
            return;
        }
        view.selected = 0;
        self.clamp_selection();
    }

    /// Carry out an action.
    fn perform(&mut self, act: Act, key: &KeyEvent) -> Vec<Effect> {
        if self.help.is_some() {
            return self.help_act(act);
        }
        if self.menu.is_some() {
            return self.menu_act(act);
        }
        if self.submission.is_some() {
            return self.submission_act(act);
        }
        if self.view().filtering {
            return self.filter_act(act);
        }
        let page = self.view().kind.page();
        match act {
            Act::Quit => {
                self.quit = true;
                vec![]
            }
            Act::Up
            | Act::Down
            | Act::Left
            | Act::Right
            | Act::PageUp
            | Act::PageDown
            | Act::Top
            | Act::Bottom => self.navigate(act),
            Act::Open => match &page {
                Some(page) => self.open_link(page),
                None => self.open_selected(),
            },
            Act::Back => self.go_back(),
            Act::NextSection | Act::PrevSection => {
                let forward = act == Act::NextSection;
                if self.view().kind.tab().is_some() {
                    self.switch_tab(!forward)
                } else {
                    self.cycle_section(forward);
                    vec![]
                }
            }
            Act::Search => {
                if self.is_list() {
                    self.view_mut().filtering = true;
                }
                vec![]
            }
            Act::Refresh => self.refresh(),
            Act::Browser => self
                .browser_url()
                .map(Effect::OpenUrl)
                .into_iter()
                .collect(),
            Act::Menu => {
                self.menu = Some(MenuState::default());
                vec![]
            }
            Act::Help => {
                self.help = Some(0);
                vec![]
            }
            Act::Download => self.download_selected(),
            Act::DownloadAll => self.download_everything(),
            Act::ShowFolder => self.show_folder(),
            Act::Submit => match self.view().kind.clone() {
                ViewKind::Detail {
                    course_id,
                    assignment_id,
                    ..
                } => self.start_submission(&course_id, &assignment_id),
                _ => vec![],
            },
            Act::JumpToLink => {
                if let KeyCode::Char(c) = key.code
                    && let Some(n) = c.to_digit(10)
                {
                    self.jump_to_link(n as usize);
                }
                vec![]
            }
            Act::HideCourse => {
                self.toggle_selected_course_hidden();
                vec![]
            }
            Act::ShowHidden => {
                self.show_hidden = !self.show_hidden;
                self.notice = Some(if self.show_hidden {
                    "Showing hidden courses. Select one, then Space h to restore it.".into()
                } else {
                    "Hidden courses are hidden again.".into()
                });
                self.clamp_selection();
                vec![]
            }
            Act::OpenLibrary => {
                let root = self.library.as_ref().map(|l| l.root().to_path_buf());
                self.reveal(root)
            }
            Act::SignIn => vec![Effect::Login],
            Act::InputDone
            | Act::InputCancel
            | Act::InputComplete
            | Act::InputClear
            | Act::Confirm => vec![],
        }
    }

    fn help_act(&mut self, act: Act) -> Vec<Effect> {
        let page = self.page();
        let Some(scroll) = self.help.as_mut() else {
            return vec![];
        };
        match act {
            Act::Up => *scroll = scroll.saturating_sub(1),
            Act::Down => *scroll += 1,
            Act::PageUp => *scroll = scroll.saturating_sub(page),
            Act::PageDown => *scroll += page,
            Act::Top => *scroll = 0,
            Act::Bottom => *scroll = usize::MAX / 2,
            Act::Back | Act::Help => self.help = None,
            _ => {}
        }
        vec![]
    }

    fn menu_act(&mut self, act: Act) -> Vec<Effect> {
        let count = self.menu_entries().len();
        let Some(menu) = self.menu.as_mut() else {
            return vec![];
        };
        match act {
            Act::Up => menu.selected = menu.selected.saturating_sub(1),
            Act::Down => menu.selected = (menu.selected + 1).min(count.saturating_sub(1)),
            Act::Search => menu.search = Some(String::new()),
            Act::InputCancel => {
                menu.search = None;
                menu.selected = 0;
            }
            Act::Back | Act::Menu => self.menu = None,
            Act::Help => {
                self.menu = None;
                self.help = Some(0);
            }
            Act::Open => {
                let selected = menu.selected;
                let chosen = self.menu_entries().get(selected).map(|e| e.act);
                self.menu = None;
                if let Some(act) = chosen {
                    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                    return self.perform(act, &enter);
                }
            }
            _ => {}
        }
        vec![]
    }

    fn filter_act(&mut self, act: Act) -> Vec<Effect> {
        match act {
            Act::InputDone => self.view_mut().filtering = false,
            Act::InputCancel => {
                let v = self.view_mut();
                v.filter.clear();
                v.filtering = false;
                v.selected = 0;
                self.clamp_selection();
            }
            Act::Up => self.move_selection(-1),
            Act::Down => self.move_selection(1),
            _ => {}
        }
        vec![]
    }

    fn submission_act(&mut self, act: Act) -> Vec<Effect> {
        let kind = self.view().kind.clone();
        let draft = self.submission.as_mut().expect("checked by the caller");
        if draft.busy {
            return vec![];
        }
        if draft.reviewing {
            match act {
                Act::InputCancel => draft.reviewing = false,
                Act::Confirm => {
                    if let ViewKind::Detail {
                        course_id,
                        assignment_id,
                        ..
                    } = &kind
                    {
                        draft.busy = true;
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
        match act {
            Act::InputCancel => self.submission = None,
            Act::InputClear => {
                draft.input.clear();
                draft.suggestions.update(&draft.input);
            }
            Act::Up => draft.suggestions.move_selection(-1),
            Act::Down => draft.suggestions.move_selection(1),
            Act::InputComplete => {
                if let Some(path) = draft.suggestions.complete() {
                    draft.input = path;
                    draft.suggestions.update(&draft.input);
                }
            }
            Act::InputDone => {
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
                    && let Some(detail) = self
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
            _ => {}
        }
        vec![]
    }

    /// Moving: rows in a list, lines of a page's text, or the entries of its
    /// focused section.
    fn navigate(&mut self, act: Act) -> Vec<Effect> {
        if self.view().kind == ViewKind::Courses && self.focus() == Section::Calendar {
            return self.move_day(act);
        }
        if matches!(act, Act::Left | Act::Right) {
            return vec![];
        }
        let page = self.page() as isize;
        let delta = match act {
            Act::Up => -1,
            Act::Down => 1,
            Act::PageUp => -page,
            Act::PageDown => page,
            Act::Top => isize::MIN / 2,
            _ => isize::MAX / 2,
        };
        if self.focus() == Section::Day {
            let n = self.day_items(self.calendar_day).len();
            self.day_item =
                (self.day_item as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
            return vec![];
        }
        if self.is_list() {
            self.move_selection(delta);
            return vec![];
        }
        let count = match (self.focus(), self.view().kind.page()) {
            (Section::Links, _) => self.current_links().len(),
            (Section::Submissions, Some(Page::Assignment(c, a))) => self.attempt_count(&c, &a),
            _ => 0,
        };
        let focus = self.focus();
        let view = self.view_mut();
        let step =
            |at: usize| (at as isize + delta).clamp(0, count.saturating_sub(1) as isize) as usize;
        match focus {
            Section::Text => view.scroll = (view.scroll as isize + delta).max(0) as usize,
            Section::Links => view.link = step(view.link),
            Section::Submissions => view.attempt = step(view.attempt),
            Section::Calendar | Section::Day => {}
        }
        vec![]
    }

    /// Arrows on the month grid: a day left/right, a week up/down, a month
    /// with PgUp/PgDn, today with Home. A new month is read when shown.
    fn move_day(&mut self, act: Act) -> Vec<Effect> {
        use crate::calendar::shift_month;
        let day = self.calendar_day;
        let today = chrono::Local::now().date_naive();
        let next = match act {
            Act::Left => day - chrono::Duration::days(1),
            Act::Right => day + chrono::Duration::days(1),
            Act::Up => day - chrono::Duration::days(7),
            Act::Down => day + chrono::Duration::days(7),
            Act::PageUp => shift_month(day, -1),
            Act::PageDown => shift_month(day, 1),
            Act::Top => today,
            _ => day,
        };
        let new_month = crate::calendar::month_key(next) != crate::calendar::month_key(day);
        self.calendar_day = next;
        self.day_item = 0;
        if new_month {
            self.read_calendar(false)
        } else {
            vec![]
        }
    }

    /// Read the month shown (and the course colours) for the home screen.
    fn read_calendar(&mut self, explicit: bool) -> Vec<Effect> {
        let month = ResourceKey::Planner(crate::calendar::month_key(self.calendar_day));
        self.load_snapshot(&month);
        self.load_snapshot(&ResourceKey::Colors);
        let mut effects: Vec<Effect> = self.request(month, explicit).into_iter().collect();
        effects.extend(self.request(ResourceKey::Colors, explicit));
        effects
    }

    /// Next or previous section of a page; the text is always first.
    fn cycle_section(&mut self, forward: bool) {
        let sections = self.sections();
        if sections.len() < 2 {
            return;
        }
        let at = sections
            .iter()
            .position(|s| *s == self.focus())
            .unwrap_or(0);
        let n = sections.len();
        let next = sections[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }];
        let links = self.current_links().len();
        let view = self.view_mut();
        view.focus = next;
        view.reveal = next != Section::Text;
        if view.link >= links {
            view.link = 0;
        }
    }

    fn jump_to_link(&mut self, n: usize) {
        if n >= 1 && n <= self.current_links().len() {
            let view = self.view_mut();
            view.focus = Section::Links;
            view.link = n - 1;
            view.reveal = true;
        }
    }

    /// `Esc`: clear a search, leave a section, stop showing hidden courses, or go back.
    fn go_back(&mut self) -> Vec<Effect> {
        if !self.view().filter.is_empty() {
            let v = self.view_mut();
            v.filter.clear();
            v.selected = 0;
            self.clamp_selection();
            return vec![];
        }
        if self.view().kind == ViewKind::Courses && self.focus() == Section::Day {
            self.view_mut().focus = Section::Calendar;
            return vec![];
        }
        if (self.view().kind.page().is_some() || self.view().kind == ViewKind::Courses)
            && self.view().focus != Section::Text
        {
            self.view_mut().focus = Section::Text;
            return vec![];
        }
        if self.view().kind == ViewKind::Courses && self.show_hidden {
            self.show_hidden = false;
            self.clamp_selection();
            return vec![];
        }
        self.back()
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

    /// Short label for a course: its code, else its name.
    pub fn course_code(&self, course_id: &str) -> String {
        self.courses
            .data
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|c| c.id == course_id)
            .map(|c| {
                c.course_code
                    .clone()
                    .unwrap_or_else(|| c.title().to_string())
            })
            .unwrap_or_default()
    }

    /// The month shown on the calendar.
    pub fn calendar_month(&self) -> Option<&Resource<Vec<crate::calendar::Item>>> {
        self.planner
            .get(&crate::calendar::month_key(self.calendar_day))
    }

    /// One day's entries, in time order.
    pub fn day_items(&self, day: chrono::NaiveDate) -> Vec<crate::calendar::Item> {
        self.calendar_items()
            .into_iter()
            .filter(|i| i.day() == Some(day))
            .collect()
    }

    /// The shown month's entries, in date order, without hidden courses.
    pub fn calendar_items(&self) -> Vec<crate::calendar::Item> {
        self.calendar_month()
            .and_then(|r| r.data.as_deref())
            .unwrap_or(&[])
            .iter()
            .filter(|i| i.listed())
            .filter(|i| {
                self.show_hidden
                    || i.course_id
                        .as_ref()
                        .is_none_or(|c| !self.hidden_courses.contains(c))
            })
            .cloned()
            .collect()
    }

    /// The Canvas page of a calendar entry, kept on the Canvas origin.
    fn calendar_url(&self, item: &crate::calendar::Item) -> String {
        match item.url.as_deref() {
            Some(path) if path.starts_with('/') && !path.starts_with("//") => {
                format!("{}{path}", self.origin)
            }
            Some(url) if url.starts_with(&format!("{}/", self.origin)) => url.to_string(),
            _ => match &item.course_id {
                Some(c) => format!("{}/courses/{c}", self.origin),
                None => format!("{}/calendar", self.origin),
            },
        }
    }

    /// `Enter` on the calendar: on a day with several entries, choose one;
    /// assignments open in the reader, anything else in the browser.
    fn open_calendar_item(&mut self) -> Vec<Effect> {
        let items = self.day_items(self.calendar_day);
        let item = match (self.focus(), items.len()) {
            (_, 0) => return vec![],
            (Section::Calendar, n) if n > 1 => {
                self.view_mut().focus = Section::Day;
                self.day_item = 0;
                return vec![];
            }
            (Section::Day, _) => items[self.day_item.min(items.len() - 1)].clone(),
            _ => items[0].clone(),
        };
        if item.kind == crate::calendar::Kind::Assignment
            && let Some(course_id) = item.course_id.clone()
        {
            let course_name = self.course_title(&course_id);
            self.stack.push(View::new(ViewKind::Detail {
                course_id,
                course_name,
                assignment_id: item.id.clone(),
                title: item.title.clone(),
            }));
            return self.enter_current(false);
        }
        vec![Effect::OpenUrl(self.calendar_url(&item))]
    }

    /// `r`: read this screen again now (and an assignment's past submissions,
    /// or a course's tabs).
    fn refresh(&mut self) -> Vec<Effect> {
        let key = self.view().key();
        let mut effects: Vec<Effect> = match key {
            ResourceKey::Tabs(_) => vec![],
            key => self.request(key, true).into_iter().collect(),
        };
        if let Some((course_id, _)) = self.view().kind.course() {
            let course_id = course_id.to_string();
            effects.extend(self.read_tabs(&course_id, true));
        }
        if self.view().kind == ViewKind::Courses {
            effects.extend(self.read_calendar(true));
        }
        if let ViewKind::Detail {
            course_id,
            assignment_id,
            ..
        } = self.view().kind.clone()
            && (self.was_submitted(&course_id, &assignment_id)
                || self
                    .histories
                    .contains_key(&(course_id.clone(), assignment_id.clone())))
        {
            effects.extend(self.request(ResourceKey::History(course_id, assignment_id), true));
        }
        effects
    }

    /// `Enter` on a page: open the selected file or link.
    fn open_link(&mut self, page: &Page) -> Vec<Effect> {
        let Some(link) = self.selected_link() else {
            return vec![];
        };
        match page {
            Page::Assignment(c, a) => self.activate_link(c, a, link),
            Page::Announcement(c, a) => self.activate_announcement_link(c, a, link),
            Page::Wiki(c, u) => match &link.target {
                Target::File { id } => {
                    let (id, dir) = (id.clone(), self.wiki_dir(c, u));
                    self.place_announcement_file(&id, dir, true)
                }
                _ => self.activate_link(c, "", link),
            },
        }
    }

    /// `d`: download the selected file (or, in Files, folder).
    fn download_selected(&mut self) -> Vec<Effect> {
        if let ViewKind::Files {
            course_id,
            course_name,
            ..
        } = self.view().kind.clone()
        {
            let rows = self.file_rows();
            return match rows.get(self.view().selected).copied() {
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
            };
        }
        if let ViewKind::Modules { course_id, .. } = self.view().kind.clone() {
            return match self.selected_module_row() {
                Some(ModuleRow::Item(m, i)) if i.kind() == ItemKind::File => {
                    match i.content().map(str::to_string) {
                        Some(id) => {
                            let module = m.name().to_string();
                            self.place_module_file(&course_id, &module, &id, false, false)
                        }
                        None => vec![],
                    }
                }
                _ => {
                    self.notice = Some("That item is not a file.".into());
                    vec![]
                }
            };
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        match self.selected_link().map(|l| l.target) {
            Some(Target::File { id }) => {
                let dir = self.page_dir(&page);
                match page {
                    Page::Assignment(..) => self.place_from(&id, dir, false),
                    Page::Announcement(..) | Page::Wiki(..) => {
                        self.place_announcement_file(&id, dir, false)
                    }
                }
            }
            Some(_) => {
                self.notice = Some("That link is not a file.".into());
                vec![]
            }
            None => {
                self.notice = Some("Choose a file first: Tab to the files & links.".into());
                vec![]
            }
        }
    }

    /// `D`: download every file in this folder, or every file of this page.
    fn download_everything(&mut self) -> Vec<Effect> {
        if let ViewKind::Files {
            course_id,
            course_name,
            folder_id,
        } = self.view().kind.clone()
        {
            return self.download_folder(&course_id, &course_name, folder_id.as_deref());
        }
        if let ViewKind::Modules { course_id, .. } = self.view().kind.clone() {
            let files: Vec<(String, String)> = self
                .visible_rows()
                .iter()
                .filter_map(|&i| match self.module_rows(&course_id).get(i).copied() {
                    Some(ModuleRow::Item(m, item)) if item.kind() == ItemKind::File => {
                        Some((m.name().to_string(), item.content()?.to_string()))
                    }
                    _ => None,
                })
                .collect();
            if files.is_empty() {
                self.notice = Some("There are no files here.".into());
                return vec![];
            }
            let count = files.len();
            let mut effects = Vec::new();
            for (module, id) in files {
                effects.extend(self.place_module_file(&course_id, &module, &id, false, true));
            }
            self.notice = Some(format!("Checking {count} module files before downloading…"));
            return effects;
        }
        let Some(page) = self.view().kind.page() else {
            return vec![];
        };
        let ids: Vec<String> = self
            .page_links(&page)
            .iter()
            .filter_map(|l| match &l.target {
                Target::File { id } => Some(id.clone()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            self.notice = Some("There are no files here.".into());
            return vec![];
        }
        let Some(dir) = self.page_dir(&page) else {
            self.notice = Some("The file library is unavailable.".into());
            return vec![];
        };
        match page {
            Page::Assignment(..) => {
                let batch = ids.into_iter().map(|id| (id, dir.clone())).collect();
                self.download_batch(batch)
            }
            Page::Announcement(..) | Page::Wiki(..) => {
                let mut effects = Vec::new();
                for id in ids {
                    effects.extend(self.place_announcement_file(&id, Some(dir.clone()), false));
                }
                self.notice = Some("Checking files before downloading…".into());
                effects
            }
        }
    }

    /// `e`: show the selected file, or this place's folder, in File Explorer.
    fn show_folder(&mut self) -> Vec<Effect> {
        let path = match self.view().kind.clone() {
            ViewKind::Courses => {
                let course = self
                    .visible_rows()
                    .get(self.view().selected)
                    .and_then(|&i| self.courses.data.as_ref()?.get(i))
                    .map(|c| (c.id.clone(), c.title().to_string()));
                match (course, self.library.as_ref()) {
                    (Some((id, title)), Some(lib)) => {
                        Some(lib.root().join(self.course_dir(&id, &title)))
                    }
                    (None, Some(lib)) => Some(lib.root().to_path_buf()),
                    _ => None,
                }
            }
            ViewKind::Files {
                course_id,
                course_name,
                folder_id,
            } => {
                let rows = self.file_rows();
                let saved = match rows.get(self.view().selected) {
                    Some(FileRow::File(f)) => self.saved_path(&f.id),
                    _ => None,
                };
                saved.or_else(|| self.class_dir(&course_id, &course_name, folder_id.as_deref()))
            }
            ViewKind::Modules {
                course_id,
                course_name,
            } => {
                let saved = match self.selected_module_row() {
                    Some(ModuleRow::Item(_, i)) if i.kind() == ItemKind::File => {
                        i.content().and_then(|id| self.saved_path(id))
                    }
                    _ => None,
                };
                saved.or_else(|| {
                    let lib = self.library.as_ref()?;
                    Some(lib.root().join(self.course_dir(&course_id, &course_name)))
                })
            }
            kind => {
                let saved = match self.selected_link().map(|l| l.target) {
                    Some(Target::File { id }) => self.saved_path(&id),
                    _ => None,
                };
                saved.or_else(|| kind.page().and_then(|p| self.page_dir(&p)))
            }
        };
        self.reveal(path)
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

    /// Cycle the course sections Canvas shows; leaving Files leaves its
    /// nested folders.
    fn switch_tab(&mut self, backwards: bool) -> Vec<Effect> {
        let Some((tab, course_id, course_name)) = self
            .view()
            .kind
            .tab()
            .map(|(t, c, n)| (t, c.to_string(), n.to_string()))
        else {
            return vec![];
        };
        let tabs = self.course_tabs(&course_id);
        let Some(&first) = tabs.first() else {
            return vec![];
        };
        let next = match tabs.iter().position(|t| *t == tab) {
            Some(i) => tabs[(i + if backwards { tabs.len() - 1 } else { 1 }) % tabs.len()],
            None => first,
        };
        if next == tab {
            return vec![];
        }
        self.replace_tab(next.view(course_id, course_name))
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

    /// Files and links in a wiki page.
    pub fn wiki_links(&self, course_id: &str, url: &str) -> Vec<Link> {
        self.wiki_pages
            .get(&(course_id.to_string(), url.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|p| p.body.as_deref())
            .map(|html| crate::html::links(html, &self.origin))
            .unwrap_or_default()
    }

    /// Local folder for a wiki page's files.
    pub fn wiki_dir(&self, course_id: &str, url: &str) -> Option<PathBuf> {
        let lib = self.library.as_ref()?;
        let title = self
            .wiki_pages
            .get(&(course_id.to_string(), url.to_string()))
            .and_then(|r| r.data.as_ref())
            .and_then(|p| p.title.clone())
            .unwrap_or_else(|| url.to_string());
        Some(
            lib.root()
                .join(self.course_dir(course_id, &self.course_title(course_id)))
                .join("Page Files")
                .join(library::safe_name(&title)),
        )
    }

    /// A module item's Canvas page, kept on the Canvas origin. Opening an
    /// item through its module page lets Canvas record a "view" requirement.
    fn module_item_url(&self, course_id: &str, item: &crate::modules::Item) -> String {
        item.html_url
            .clone()
            .filter(|u| u.starts_with(&format!("{}/", self.origin)))
            .unwrap_or_else(|| {
                if crate::model::valid_id(&item.id) {
                    format!(
                        "{}/courses/{course_id}/modules/items/{}",
                        self.origin, item.id
                    )
                } else {
                    format!("{}/courses/{course_id}/modules", self.origin)
                }
            })
    }

    /// `Enter` on a module item: assignments and pages open here, files go
    /// to the library, everything else (quizzes, discussions, links, tools)
    /// opens in the browser.
    fn open_module_item(
        &mut self,
        course_id: &str,
        course_name: &str,
        module: &str,
        item: crate::modules::Item,
    ) -> Vec<Effect> {
        let title = item.title().to_string();
        let next = match (item.kind(), item.content(), item.page()) {
            (ItemKind::Heading, ..) => return vec![],
            (ItemKind::Assignment, Some(id), _) => ViewKind::Detail {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
                assignment_id: id.to_string(),
                title,
            },
            (ItemKind::Page, _, Some(url)) => ViewKind::WikiPage {
                course_id: course_id.to_string(),
                course_name: course_name.to_string(),
                page_url: url.to_string(),
                title,
            },
            (ItemKind::File, Some(id), _) => {
                let id = id.to_string();
                return self.place_module_file(course_id, module, &id, true, false);
            }
            _ => return vec![Effect::OpenUrl(self.module_item_url(course_id, &item))],
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// Put a module file in the library. Module files are course files, so
    /// they go in Class Files under their Canvas folder; when Files is hidden
    /// the folder cannot be read, so they go in `Module Files/<module>`.
    /// The file's details are always read fresh first.
    fn place_module_file(
        &mut self,
        course_id: &str,
        module: &str,
        file_id: &str,
        open: bool,
        quiet: bool,
    ) -> Vec<Effect> {
        if self.library.is_none() {
            if !quiet {
                self.notice = Some("The file library is unavailable.".into());
            }
            return vec![];
        }
        self.module_waiting.insert(
            file_id.to_string(),
            ModuleFile {
                course_id: course_id.to_string(),
                module: module.to_string(),
                open,
                quiet,
            },
        );
        let mut effects: Vec<Effect> = self
            .request(ResourceKey::File(file_id.to_string()), false)
            .into_iter()
            .collect();
        if self.course_tabs(course_id).contains(&CourseTab::Files) {
            let key = ResourceKey::Files(course_id.to_string());
            self.load_snapshot(&key);
            let known = self
                .course_files
                .get(course_id)
                .is_some_and(|r| r.data.is_some() || matches!(r.sync, SyncState::Syncing { .. }));
            if !known {
                effects.extend(self.request(key, false));
            }
        }
        effects.extend(self.place_module_files());
        effects
    }

    /// Place the module files whose details (and course folders) are in.
    fn place_module_files(&mut self) -> Vec<Effect> {
        let busy = |app: &App, key: ResourceKey| {
            matches!(app.sync_of(&key), Some(SyncState::Syncing { .. }))
        };
        let ready: Vec<String> = self
            .module_waiting
            .iter()
            .filter(|(id, m)| {
                !busy(self, ResourceKey::File((*id).clone()))
                    && !busy(self, ResourceKey::Files(m.course_id.clone()))
                    && self.file_meta(id).is_some()
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut effects = Vec::new();
        for id in ready {
            let Some(m) = self.module_waiting.remove(&id) else {
                continue;
            };
            let Some(dir) = self.module_file_dir(&m.course_id, &m.module, &id) else {
                continue;
            };
            effects.extend(self.place_file(
                &id,
                Intent {
                    dir,
                    open: m.open,
                    automatic: false,
                    quiet: m.quiet,
                },
            ));
        }
        effects
    }

    pub fn module_file_dir(&self, course_id: &str, module: &str, file_id: &str) -> Option<PathBuf> {
        let name = self.course_title(course_id);
        let folder = self.file_meta(file_id).and_then(|f| f.folder_id.clone());
        if let Some(id) = folder.as_deref()
            && self
                .folder(course_id, id)
                .is_some_and(|f| f.full_name.is_some())
        {
            return self.class_dir(course_id, &name, Some(id));
        }
        let lib = self.library.as_ref()?;
        Some(
            lib.root()
                .join(self.course_dir(course_id, &name))
                .join("Module Files")
                .join(library::safe_name(module)),
        )
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
            Target::Page { course_id, url } => {
                let course_name = self.course_title(&course_id);
                let title = if link.text.is_empty() {
                    url.clone()
                } else {
                    link.text
                };
                self.stack.push(View::new(ViewKind::WikiPage {
                    course_id,
                    course_name,
                    page_url: url,
                    title,
                }));
                self.enter_current(false)
            }
            // Modules, discussions, tools and outside sites open in the browser.
            Target::Canvas | Target::Web | Target::Mail => vec![Effect::OpenUrl(link.url)],
        }
    }

    fn open_selected(&mut self) -> Vec<Effect> {
        if matches!(self.focus(), Section::Calendar | Section::Day) {
            return self.open_calendar_item();
        }
        let rows = self.visible_rows();
        let Some(&index) = rows.get(self.view().selected) else {
            return vec![];
        };
        let next = match &self.view().kind {
            ViewKind::Courses => {
                let Some(course) = self.courses.data.as_ref().and_then(|l| l.get(index)) else {
                    return vec![];
                };
                let (id, title) = (course.id.clone(), course.title().to_string());
                // Open where the saved tabs say, before Canvas is asked again.
                self.load_snapshot(&ResourceKey::Tabs(id.clone()));
                self.course_view(&id, &title)
            }
            ViewKind::Modules {
                course_id,
                course_name,
            } => {
                let (course_id, course_name) = (course_id.clone(), course_name.clone());
                let Some(ModuleRow::Item(m, item)) =
                    self.module_rows(&course_id).get(index).copied()
                else {
                    return vec![];
                };
                let (module, item) = (m.name().to_string(), item.clone());
                return self.open_module_item(&course_id, &course_name, &module, item);
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
            ViewKind::Detail { .. }
            | ViewKind::Announcement { .. }
            | ViewKind::WikiPage { .. }
            | ViewKind::CourseHome { .. } => return vec![],
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
            ViewKind::Modules { course_id, .. } => Some(match self.selected_module_row() {
                Some(ModuleRow::Item(_, item)) => self.module_item_url(course_id, item),
                _ => format!("{}/courses/{course_id}/modules", self.origin),
            }),
            ViewKind::WikiPage {
                course_id,
                page_url,
                ..
            } => {
                if let Some(link) = self.selected_link() {
                    return Some(link.url);
                }
                let html = self
                    .wiki_pages
                    .get(&(course_id.clone(), page_url.clone()))
                    .and_then(|r| r.data.as_ref())
                    .and_then(|p| p.html_url.clone())
                    .filter(|u| same_origin(u));
                Some(html.unwrap_or_else(|| {
                    if page_url == crate::modules::FRONT_PAGE {
                        format!("{}/courses/{course_id}", self.origin)
                    } else {
                        format!("{}/courses/{course_id}/pages/{page_url}", self.origin)
                    }
                }))
            }
            ViewKind::CourseHome { course_id, .. } => {
                Some(format!("{}/courses/{course_id}", self.origin))
            }
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
                if let Some(link) = self.selected_link() {
                    return Some(link.url);
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
            } if self.selected_link().is_some() => {
                let _ = (course_id, assignment_id);
                let link = self.selected_link()?;
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
            ViewKind::Courses if matches!(self.focus(), Section::Calendar | Section::Day) => {
                let items = self.day_items(self.calendar_day);
                let index = if self.focus() == Section::Day {
                    self.day_item
                } else {
                    0
                };
                Some(match items.get(index) {
                    Some(item) => self.calendar_url(item),
                    None => format!("{}/calendar", self.origin),
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
    fn past_submissions_load_with_the_page_and_are_a_section_of_it() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let (k, r) = fetch_of(&app.enter_current(false));
        let mut detail = AssignmentDetail::default();
        detail.base.id = "20".into();
        detail.base.submission = Some(crate::model::Submission {
            submitted_at: Some(Utc::now()),
            attempt: Some(2),
            ..Default::default()
        });
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::new(detail)), FetchStats::default())),
            at: Utc::now(),
        });
        // Something was submitted, so the attempts are read with the page.
        let (key1, request1) = fetch_of(&effects);
        assert_eq!(key1, ResourceKey::History("10".into(), "20".into()));
        assert_eq!(app.sections(), vec![Section::Text], "no attempts known yet");
        let history = submission::History::parse(serde_json::json!({
            "attempt": 2, "submitted_at": "2026-10-22T12:00:00Z",
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z"}
            ]
        }))
        .unwrap();
        deliver(&mut app, key1, request1, Payload::History(history));
        assert_eq!(app.attempt_count("10", "20"), 2);
        assert_eq!(app.sections(), vec![Section::Text, Section::Submissions]);
        // Tab moves to the attempts; arrows choose one; Esc returns to the text, then leaves.
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.focus(), Section::Submissions);
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().attempt, 1);
        app.update(key(KeyCode::Down));
        assert_eq!(app.view().attempt, 1, "stays on the oldest");
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(app.focus(), Section::Text);
        assert_eq!(app.stack.len(), 2);
        // `h` is "back" here too, as everywhere else.
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.stack.len(), 1);
        // A new visit reads the attempts again, right away this time.
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let effects = app.enter_current(false);
        let history_reads: Vec<u64> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch {
                    key: ResourceKey::History(..),
                    request,
                } => Some(*request),
                _ => None,
            })
            .collect();
        assert!(
            matches!(history_reads[..], [r] if r > request1),
            "{effects:?}"
        );
    }

    #[test]
    fn unsubmitted_assignments_do_not_read_past_submissions() {
        let mut app = App::new("https://ufl.instructure.com", None);
        app.stack.push(View::new(ViewKind::Detail {
            course_id: "10".into(),
            course_name: "GPU".into(),
            assignment_id: "20".into(),
            title: "Lab".into(),
        }));
        let (k, r) = fetch_of(&app.enter_current(false));
        let effects = app.update(Msg::Fetched {
            key: k,
            request: r,
            result: Ok((Payload::Detail(Box::default()), FetchStats::default())),
            at: Utc::now(),
        });
        assert!(effects.is_empty(), "{effects:?}");
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.focus(), Section::Text, "nothing to move to");
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
        // Space, then h: hide the selected course.
        assert!(app.update(key(KeyCode::Char(' '))).is_empty());
        assert!(app.menu.is_some());
        assert!(app.update(key(KeyCode::Char('h'))).is_empty());
        assert!(app.menu.is_none());
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
        // Space, then H: show hidden courses; Space, then h on one restores it.
        restarted.update(key(KeyCode::Char(' ')));
        restarted.update(key(KeyCode::Char('H')));
        assert_eq!(restarted.visible_rows(), [0, 1]);
        restarted.update(key(KeyCode::Char(' ')));
        restarted.update(key(KeyCode::Char('h')));
        assert!(restarted.hidden_courses.is_empty());
        restarted.update(key(KeyCode::Esc));
        assert!(!restarted.show_hidden);
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
        for _ in 0..2 {
            app.update(key(KeyCode::Char(' ')));
            app.update(key(KeyCode::Char('h')));
        }
        assert!(app.visible_rows().is_empty());
        // The menu still offers a way back.
        app.update(key(KeyCode::Char(' ')));
        assert!(app.menu_entries().iter().any(|e| e.act == Act::ShowHidden));
        app.update(key(KeyCode::Char('H')));
        assert_eq!(app.visible_rows(), [0, 1]);
        app.update(key(KeyCode::Char(' ')));
        app.update(key(KeyCode::Char('h')));
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
        // Forward goes on to Modules; backward returns to Announcements.
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Modules("10".into()));
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
        app.update(key(KeyCode::BackTab)); // Modules, last before Canvas's tabs are known
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
            app.update(key(KeyCode::BackTab)); // Modules, last before Canvas's tabs are known
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
        // Tab moves to the files & links; the handout's details are still being read.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus(), Section::Links);
        assert_eq!(app.view().link, 0);
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
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Effect::OpenUrl("https://github.com/a/b".into())]
        );
        // Another assignment opens in the reader.
        app.update(key(KeyCode::Char('3')));
        let (k, _) = fetch_of(&app.update(key(KeyCode::Enter)));
        assert_eq!(k, ResourceKey::Detail("10".into(), "8".into()));
        assert!(matches!(&app.view().kind, ViewKind::Detail { title, .. } if title == "Lab-1"));
        // Back again: the list keeps its place; Esc returns to the text, then leaves.
        app.update(key(KeyCode::Esc));
        assert_eq!(app.focus(), Section::Links);
        assert_eq!(app.view().link, 2);
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert_eq!(app.focus(), Section::Text);
        assert_eq!(app.stack.len(), 3);
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

    // ---- Course tabs and modules ----

    fn tab(id: &str) -> crate::modules::Tab {
        crate::modules::Tab {
            id: id.into(),
            ..Default::default()
        }
    }

    fn fetches(effects: &[Effect]) -> Vec<ResourceKey> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fetch { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// Deliver the read in flight for `key` and return what the app does next.
    fn answer(app: &mut App, key: ResourceKey, payload: Payload) -> Vec<Effect> {
        let request = match app.sync_of(&key) {
            Some(SyncState::Syncing { request, .. }) => *request,
            other => panic!("no read in flight for {key:?}: {other:?}"),
        };
        app.update(Msg::Fetched {
            key,
            request,
            result: Ok((payload, FetchStats::default())),
            at: Utc::now(),
        })
    }

    fn tabs_key() -> ResourceKey {
        ResourceKey::Tabs("10".into())
    }

    #[test]
    fn course_tabs_follow_canvas_and_are_read_once_per_run() {
        let mut app = app_with_courses();
        let effects = app.update(key(KeyCode::Enter));
        // Before Canvas's tabs are known the course opens on Assignments.
        assert!(matches!(app.view().kind, ViewKind::Assignments { .. }));
        assert!(fetches(&effects).contains(&tabs_key()));
        let effects = answer(
            &mut app,
            tabs_key(),
            Payload::Tabs(vec![tab("home"), tab("announcements"), tab("modules")]),
        );
        // Assignments is hidden on Canvas: the first section Canvas shows instead.
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        assert_eq!(fetches(&effects), [ResourceKey::Announcements("10".into())]);
        assert_eq!(
            app.course_tabs("10"),
            [CourseTab::Announcements, CourseTab::Modules]
        );
        // Tab cycles only what Canvas shows.
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(fetch_of(&effects).0, ResourceKey::Modules("10".into()));
        app.update(key(KeyCode::Tab));
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        // Coming back uses the tabs already read this run.
        app.update(key(KeyCode::Esc));
        let effects = app.update(key(KeyCode::Enter));
        assert!(matches!(app.view().kind, ViewKind::Announcements { .. }));
        assert!(!fetches(&effects).contains(&tabs_key()));
        // `r` reads them again.
        assert!(fetches(&app.update(key(KeyCode::Char('r')))).contains(&tabs_key()));
    }

    #[test]
    fn saved_tabs_are_used_at_once_and_read_again_once_per_run() {
        let root = std::env::temp_dir().join(format!("tuicanvas-tabs-{}", std::process::id()));
        let store = Arc::new(SnapshotStore::new(&root, "reader"));
        store.save(&tabs_key(), &vec![tab("files")], Utc::now());
        let mut app = app_with_courses();
        app.store = Some(store);
        let effects = app.update(key(KeyCode::Enter));
        assert!(matches!(app.view().kind, ViewKind::Files { .. }));
        assert_eq!(
            fetches(&effects),
            [ResourceKey::Files("10".into()), tabs_key()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_modules_home_counts_as_shown_and_no_sections_leaves_the_course_page() {
        let mut app = App::new(ORIGIN, None);
        let (k, r) = fetch_of(&app.start());
        let mut home = course("10", "GPU Computing");
        home.default_view = Some("modules".into());
        deliver(
            &mut app,
            k,
            r,
            Payload::Courses(vec![home, course("20", "Compilers")]),
        );
        app.update(key(KeyCode::Enter));
        answer(&mut app, tabs_key(), Payload::Tabs(vec![tab("home")]));
        assert!(matches!(app.view().kind, ViewKind::Modules { .. }));
        assert!(!app.available(Act::NextSection));
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));
        let other = ResourceKey::Tabs("20".into());
        answer(
            &mut app,
            other.clone(),
            Payload::Tabs(vec![tab("home"), tab("grades")]),
        );
        assert!(matches!(app.view().kind, ViewKind::CourseHome { .. }));
        assert_eq!(app.browser_url().unwrap(), format!("{ORIGIN}/courses/20"));
        // A refresh that finds a section shown again opens it.
        assert_eq!(
            fetches(&app.update(key(KeyCode::Char('r')))),
            [other.clone()]
        );
        answer(&mut app, other, Payload::Tabs(vec![tab("files")]));
        assert!(matches!(app.view().kind, ViewKind::Files { .. }));
    }

    const MODULES: &str = r#"[{"id":1,"name":"Week 1","state":"started","items":[
        {"id":11,"type":"SubHeader","title":"Read first"},
        {"id":12,"type":"Page","title":"Intro","page_url":"intro","completion_requirement":{"type":"must_view","completed":true}},
        {"id":13,"type":"Assignment","title":"Lab 1","content_id":55,"indent":1},
        {"id":14,"type":"File","title":"lecture1.pdf","content_id":101},
        {"id":15,"type":"Quiz","title":"Quiz 1","content_id":4,"html_url":"https://ufl.instructure.com/courses/10/modules/items/15"},
        {"id":16,"type":"ExternalUrl","title":"Reading","html_url":"https://example.com/x"}]},
        {"id":2,"name":"Week 2","state":"locked","items":[]}]"#;

    /// GPU Computing with Canvas showing `tabs`, on its Modules screen.
    fn modules_app(tabs: &[&str], library: Option<Arc<Library>>) -> App {
        let mut app = app_with_courses();
        app.library = library;
        app.update(key(KeyCode::Enter));
        let list = tabs.iter().map(|t| tab(t)).collect();
        answer(&mut app, tabs_key(), Payload::Tabs(list));
        while !matches!(app.view().kind, ViewKind::Modules { .. }) {
            app.update(key(KeyCode::Tab));
        }
        let modules = serde_json::from_str(MODULES).unwrap();
        answer(
            &mut app,
            ResourceKey::Modules("10".into()),
            Payload::Modules(modules),
        );
        app
    }

    fn download_target(effects: &[Effect]) -> PathBuf {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Download { target, .. } => Some(target.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a download, got {effects:?}"))
    }

    #[test]
    fn modules_list_skips_headings_and_opens_each_kind() {
        let (library, root) = temp_library("modules");
        let mut app = modules_app(&["assignments", "modules"], Some(library));
        // Rows: Week 1, Read first, Intro, Lab 1, lecture1.pdf, Quiz 1, Reading, Week 2.
        assert_eq!(app.view().selected, 2);
        app.update(key(KeyCode::Up));
        assert_eq!(app.view().selected, 2);
        app.update(key(KeyCode::End));
        assert_eq!(app.view().selected, 6);
        app.update(key(KeyCode::Home));
        assert_eq!(app.view().selected, 2);

        // Assignments open in the reader.
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Detail("10".into(), "55".into())
        );
        app.update(key(KeyCode::Esc));

        // Files is hidden, so a module file goes to Module Files/<module>.
        app.update(key(KeyCode::Down));
        assert!(app.available(Act::Download));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(fetches(&effects), [ResourceKey::File("101".into())]);
        let effects = deliver_file(&mut app, canvas_file("101", "lecture1.pdf", "3"));
        assert!(
            download_target(&effects).ends_with(
                Path::new("GPU Computing")
                    .join("Module Files")
                    .join("Week 1")
                    .join("lecture1.pdf")
            )
        );

        // Quizzes open on Canvas; outside links go through their module item.
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Effect::OpenUrl(format!(
                "{ORIGIN}/courses/10/modules/items/15"
            ))]
        );
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.browser_url().unwrap(),
            format!("{ORIGIN}/courses/10/modules/items/16")
        );

        // Pages are read here, with their files and links.
        app.update(key(KeyCode::Home));
        let effects = app.update(key(KeyCode::Enter));
        let page = ResourceKey::WikiPage("10".into(), "intro".into());
        assert_eq!(fetch_of(&effects).0, page);
        answer(
            &mut app,
            page,
            Payload::WikiPage(Box::new(crate::modules::WikiPage {
                title: Some("Intro".into()),
                body: Some(
                    r#"<p>See <a href="/courses/10/files/22">notes</a> and <a href="/courses/10/pages/week-2">next</a>.</p>"#.into(),
                ),
                ..Default::default()
            })),
        );
        let links = app.current_links();
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[1].target,
            Target::Page {
                course_id: "10".into(),
                url: "week-2".into()
            }
        );
        assert!(
            app.wiki_dir("10", "intro")
                .unwrap()
                .ends_with(Path::new("GPU Computing").join("Page Files").join("Intro"))
        );
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Down));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::WikiPage("10".into(), "week-2".into())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn module_files_mirror_their_canvas_folder_when_files_is_shown() {
        let (library, root) = temp_library("module-folders");
        let mut app = modules_app(&["modules", "files"], Some(library));
        for _ in 0..2 {
            app.update(key(KeyCode::Down));
        }
        let effects = app.update(key(KeyCode::Char('d')));
        assert_eq!(
            fetches(&effects),
            [
                ResourceKey::File("101".into()),
                ResourceKey::Files("10".into())
            ]
        );
        // The file's details alone are not enough while its folders are being read.
        let effects = deliver_file(&mut app, canvas_file("101", "lecture1.pdf", "3"));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Download { .. })));
        let effects = answer(
            &mut app,
            ResourceKey::Files("10".into()),
            Payload::Files(Box::new(course_files())),
        );
        assert!(
            download_target(&effects).ends_with(
                Path::new("Class Files")
                    .join("Lectures")
                    .join("Week 1")
                    .join("lecture1.pdf")
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn module_search_keeps_headings_of_matching_items() {
        let mut app = modules_app(&["modules"], None);
        app.update(key(KeyCode::Char('/')));
        for c in "quiz".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        assert_eq!(app.visible_rows(), [0, 5]);
        assert_eq!(app.view().selected, 1);
        assert!(matches!(
            app.selected_module_row(),
            Some(ModuleRow::Item(_, i)) if i.title() == "Quiz 1"
        ));
    }

    #[test]
    fn a_page_home_is_the_home_tab_where_tab_changes_tabs_and_digits_reach_links() {
        let mut app = App::new(ORIGIN, None);
        let (k, r) = fetch_of(&app.start());
        let mut wiki = course("10", "GPU Computing");
        wiki.default_view = Some("wiki".into());
        deliver(&mut app, k, r, Payload::Courses(vec![wiki]));
        let effects = app.update(key(KeyCode::Enter));
        let home = ResourceKey::WikiPage("10".into(), crate::modules::FRONT_PAGE.into());
        assert_eq!(fetches(&effects), [home.clone(), tabs_key()]);
        answer(
            &mut app,
            tabs_key(),
            Payload::Tabs(vec![tab("home"), tab("announcements")]),
        );
        assert_eq!(
            app.course_tabs("10"),
            [CourseTab::Home, CourseTab::Announcements]
        );
        answer(
            &mut app,
            home.clone(),
            Payload::WikiPage(Box::new(crate::modules::WikiPage {
                title: Some("Welcome".into()),
                body: Some(r#"<a href="/courses/10/pages/week-1">Week 1</a>"#.into()),
                ..Default::default()
            })),
        );
        let scopes = app.scopes();
        assert!(scopes.contains(&Scope::CourseTabs) && !scopes.contains(&Scope::Sections));
        assert_eq!(app.browser_url().unwrap(), format!("{ORIGIN}/courses/10"));
        // Digits reach the links; Enter opens a linked page in the reader.
        app.update(key(KeyCode::Char('1')));
        let effects = app.update(key(KeyCode::Enter));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::WikiPage("10".into(), "week-1".into())
        );
        // On that page Tab moves between sections again.
        assert!(app.scopes().contains(&Scope::Sections));
        app.update(key(KeyCode::Esc));
        assert!(matches!(app.view().kind.tab(), Some((CourseTab::Home, ..))));
        app.update(key(KeyCode::Esc)); // leave the links section
        let effects = app.update(key(KeyCode::Tab));
        assert_eq!(
            fetch_of(&effects).0,
            ResourceKey::Announcements("10".into())
        );
        app.update(key(KeyCode::Tab));
        assert!(matches!(app.view().kind.tab(), Some((CourseTab::Home, ..))));
    }
}
