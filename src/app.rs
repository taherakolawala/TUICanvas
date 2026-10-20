//! Application state and navigation. Pure logic: input and network results
//! come in as messages; requests to fetch, open, or sign in go out as effects.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cache::{ResourceKey, SnapshotStore};
use crate::canvas::{CanvasError, FetchStats};
use crate::model::{AssignmentDetail, AssignmentSummary, Course};

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
    Assignments(Vec<AssignmentSummary>),
    Detail(Box<AssignmentDetail>),
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Fetch { key: ResourceKey, request: u64 },
    OpenUrl(String),
    Login,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewKind {
    Courses,
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
}

#[derive(Clone, Debug)]
pub struct View {
    pub kind: ViewKind,
    pub selected: usize,
    pub offset: usize,
    pub filter: String,
    pub filtering: bool,
    pub scroll: usize,
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
        }
    }

    pub fn key(&self) -> ResourceKey {
        match &self.kind {
            ViewKind::Courses => ResourceKey::Courses,
            ViewKind::Assignments { course_id, .. } => ResourceKey::Assignments(course_id.clone()),
            ViewKind::Detail {
                course_id,
                assignment_id,
                ..
            } => ResourceKey::Detail(course_id.clone(), assignment_id.clone()),
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

pub struct App {
    pub origin: String,
    store: Option<Arc<SnapshotStore>>,
    pub courses: Resource<Vec<Course>>,
    pub assignments: HashMap<String, Resource<Vec<AssignmentSummary>>>,
    pub details: HashMap<(String, String), Resource<AssignmentDetail>>,
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
}

fn matches_filter(text: &str, filter: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(&filter.to_lowercase())
}

impl App {
    pub fn new(origin: &str, store: Option<Arc<SnapshotStore>>) -> Self {
        App {
            origin: origin.to_string(),
            store,
            courses: Resource::default(),
            assignments: HashMap::new(),
            details: HashMap::new(),
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
            ResourceKey::Assignments(c) => self.assignments.get(c).map(|r| &r.sync),
            ResourceKey::Detail(c, a) => self.details.get(&(c.clone(), a.clone())).map(|r| &r.sync),
        }
    }

    fn set_sync(&mut self, key: &ResourceKey, sync: SyncState) {
        match key {
            ResourceKey::Courses => self.courses.sync = sync,
            ResourceKey::Assignments(c) => {
                self.assignments.entry(c.clone()).or_default().sync = sync
            }
            ResourceKey::Detail(c, a) => {
                self.details.entry((c.clone(), a.clone())).or_default().sync = sync
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
            _ => {}
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
                self.last_stats = Some((key.label(), stats));
                match (payload, &key) {
                    (Payload::Courses(list), ResourceKey::Courses) => {
                        let r = &mut self.courses;
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
                    (Payload::Detail(detail), ResourceKey::Detail(c, a)) => {
                        let r = self.details.entry((c.clone(), a.clone())).or_default();
                        r.data = Some(*detail);
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
                vec![]
            }
            Err(error) => {
                // Keep whatever was displayed; mark it outdated instead.
                let needs_login = error.needs_login();
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
                .filter(|(_, c)| {
                    matches_filter(c.title(), &view.filter)
                        || matches_filter(c.course_code.as_deref().unwrap_or(""), &view.filter)
                })
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
            ViewKind::Detail { .. } => vec![],
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
        !matches!(self.view().kind, ViewKind::Detail { .. })
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

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![];
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
        self.stack.pop();
        // Returning to a view is a new access: show what we have, sync again.
        self.enter_current(false)
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
            ViewKind::Detail { .. } => return vec![],
        };
        self.stack.push(View::new(next));
        self.enter_current(false)
    }

    /// Only Canvas-origin pages are opened.
    pub fn browser_url(&self) -> Option<String> {
        let same_origin = |u: &str| u.starts_with(&format!("{}/", self.origin));
        match &self.view().kind {
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
}
