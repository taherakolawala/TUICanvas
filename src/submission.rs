//! Deliberate, one-shot assignment file submissions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::canvas::CanvasClient;
use crate::model::{AssignmentDetail, valid_id};

#[derive(Clone, Debug, PartialEq)]
pub struct FileChoice {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Attempt {
    #[serde(default)]
    pub attempt: Option<i64>,
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub submission_type: Option<String>,
    #[serde(default)]
    pub workflow_state: Option<String>,
    #[serde(default)]
    pub late: Option<bool>,
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub attachments: Vec<SubmittedFile>,
}

fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SubmittedFile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

impl SubmittedFile {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.filename.as_deref())
            .unwrap_or("file")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub attempts: Vec<Attempt>,
}

#[derive(Deserialize)]
struct HistoryResponse {
    #[serde(flatten)]
    current: Attempt,
    submission_history: Vec<Attempt>,
}

impl History {
    pub fn parse(value: Value) -> Result<Self, serde_json::Error> {
        let HistoryResponse {
            current,
            submission_history: mut attempts,
        } = serde_json::from_value(value)?;
        if current.submitted_at.is_some()
            && !attempts
                .iter()
                .any(|a| a.attempt == current.attempt && a.submitted_at == current.submitted_at)
        {
            attempts.push(current);
        }
        attempts.retain(|a| a.submitted_at.is_some());
        attempts.sort_by(|a, b| {
            b.attempt
                .cmp(&a.attempt)
                .then_with(|| b.submitted_at.cmp(&a.submitted_at))
        });
        Ok(Self { attempts })
    }
}

/// Local path matches are cached for the current directory, so typing each
/// character filters memory instead of repeatedly walking the filesystem.
#[derive(Clone, Debug, Default)]
pub struct PathSuggestions {
    directory: Option<PathBuf>,
    entries: Vec<(String, bool)>,
    pub matches: Vec<PathSuggestion>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct PathSuggestion {
    pub path: String,
    pub name: String,
    pub directory: bool,
}

pub fn starting_directory() -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut path = home.to_string_lossy().into_owned();
    if !path.ends_with(['\\', '/']) {
        path.push(std::path::MAIN_SEPARATOR);
    }
    path
}

impl PathSuggestions {
    pub fn update(&mut self, input: &str) {
        self.matches.clear();
        self.selected = 0;
        let input = input.trim_matches('"');
        if input.is_empty() {
            return;
        }
        let separator = std::path::MAIN_SEPARATOR;
        let (typed_dir, fragment) = match input.rfind(['\\', '/']) {
            Some(index) => (&input[..=index], &input[index + 1..]),
            None => ("", input),
        };
        let expanded_dir = if typed_dir == "~\\" || typed_dir == "~/" {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        } else if typed_dir.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(typed_dir)
        };
        if self.directory.as_ref() != Some(&expanded_dir) {
            self.directory = Some(expanded_dir.clone());
            self.entries = std::fs::read_dir(&expanded_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name().into_string().ok()?;
                    let directory = entry.file_type().ok()?.is_dir();
                    if directory && name.starts_with('.') {
                        return None;
                    }
                    Some((name, directory))
                })
                .collect();
            self.entries.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            });
        }
        let prefix = fragment.to_lowercase();
        let output_dir = if typed_dir == "~\\" || typed_dir == "~/" {
            format!("{}{}", expanded_dir.display(), separator)
        } else {
            typed_dir.to_string()
        };
        self.matches = self
            .entries
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
            .take(8)
            .map(|(name, directory)| PathSuggestion {
                path: format!(
                    "{output_dir}{name}{}",
                    if *directory {
                        separator.to_string()
                    } else {
                        String::new()
                    }
                ),
                name: name.clone(),
                directory: *directory,
            })
            .collect();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if !self.matches.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.matches.len() as isize) as usize;
        }
    }

    pub fn complete(&self) -> Option<String> {
        self.matches
            .get(self.selected)
            .map(|entry| entry.path.clone())
    }
}

pub fn choose_file(
    input: &str,
    assignment: &AssignmentDetail,
    existing: &[FileChoice],
) -> Result<FileChoice, String> {
    let raw = input.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("Enter a file path.".into());
    }
    let path = std::fs::canonicalize(raw).map_err(|_| "File not found.".to_string())?;
    let meta = std::fs::metadata(&path).map_err(|_| "Could not read file metadata.".to_string())?;
    if !meta.is_file() {
        return Err("Select a file, not a folder.".into());
    }
    if meta.len() == 0 {
        return Err("The file is empty.".into());
    }
    if existing.iter().any(|f| f.path == path) {
        return Err("That file is already selected.".into());
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "File name is not valid UTF-8.".to_string())?
        .to_string();
    if !assignment.allowed_extensions.is_empty() {
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !assignment
            .allowed_extensions
            .iter()
            .any(|allowed| allowed.trim_start_matches('.').eq_ignore_ascii_case(ext))
        {
            return Err(format!(
                "Canvas allows only: {}",
                assignment.allowed_extensions.join(", ")
            ));
        }
    }
    Ok(FileChoice {
        path,
        name,
        size: meta.len(),
    })
}

pub fn eligible(assignment: &AssignmentDetail, now: DateTime<Utc>) -> Result<(), String> {
    if !assignment
        .base
        .submission_types
        .iter()
        .any(|t| t == "online_upload")
    {
        return Err("This assignment does not accept file uploads.".into());
    }
    if assignment.base.locked_for_user == Some(true)
        || assignment.base.unlock_at.is_some_and(|t| t > now)
        || assignment.base.lock_at.is_some_and(|t| t <= now)
    {
        return Err("This assignment is locked for submissions.".into());
    }
    if let Some(max) = assignment.allowed_attempts
        && max >= 0
        && prior_attempts(assignment) >= max
    {
        return Err("No submission attempts remain.".into());
    }
    Ok(())
}

pub fn prior_attempts(assignment: &AssignmentDetail) -> i64 {
    assignment
        .base
        .submission
        .as_ref()
        .map(|s| {
            s.attempt
                .unwrap_or(0)
                .max(if s.submitted_at.is_some() { 1 } else { 0 })
        })
        .unwrap_or(0)
}

pub async fn submit(
    client: &CanvasClient,
    course: &str,
    assignment: &str,
    files: &[FileChoice],
) -> Result<String, String> {
    if !valid_id(course) || !valid_id(assignment) || files.is_empty() {
        return Err("Invalid submission request.".into());
    }
    let mut ids = Vec::new();
    for file in files {
        let meta = std::fs::metadata(&file.path)
            .map_err(|_| format!("{} is no longer readable.", file.name))?;
        if !meta.is_file() || meta.len() != file.size {
            return Err(format!("{} changed since review. Start again.", file.name));
        }
        let size = file.size.to_string();
        let init: Value = client
            .post_form(
                &format!(
                    "/api/v1/courses/{course}/assignments/{assignment}/submissions/self/files"
                ),
                &[("name", &file.name), ("size", &size)],
            )
            .await
            .map_err(|e| format!("Could not start upload for {}: {}", file.name, e.describe()))?;
        let url = init
            .get("upload_url")
            .and_then(Value::as_str)
            .ok_or_else(|| "Canvas did not provide an upload URL.".to_string())?;
        let params = init
            .get("upload_params")
            .and_then(Value::as_object)
            .ok_or_else(|| "Canvas did not provide upload parameters.".to_string())?;
        let uploaded = client
            .upload_signed(url, params, &file.path, file.size)
            .await
            .map_err(|e| format!("Could not upload {}: {}", file.name, e.describe()))?;
        let id = uploaded
            .get("id")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .filter(|id| valid_id(id))
            .ok_or_else(|| format!("Canvas did not confirm the upload of {}.", file.name))?;
        ids.push(id);
    }
    let endpoint = format!("/api/v1/courses/{course}/assignments/{assignment}/submissions");
    let mut fields = vec![("submission[submission_type]", "online_upload")];
    fields.extend(ids.iter().map(|id| ("submission[file_ids][]", id.as_str())));
    let response: Value = client.post_form(&endpoint, &fields).await.map_err(|e| {
        format!(
            "Submission may not have completed: {}. Refresh before trying again.",
            e.describe()
        )
    })?;
    if response
        .get("submitted_at")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(
            "Canvas did not confirm a submission time. Refresh before trying again.".into(),
        );
    }
    let check: Value = client
        .get_json(&format!("{endpoint}/self?include[]=submission_history"))
        .await
        .map(|(v, _)| v)
        .map_err(|e| {
            format!(
                "Canvas accepted the submission, but verification failed: {}. Refresh to check it.",
                e.describe()
            )
        })?;
    let recorded: HashSet<String> = check
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("id"))
        .filter_map(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .collect();
    if !ids.iter().all(|id| recorded.contains(id)) {
        return Err("Canvas accepted the submission, but the file list could not be verified. Refresh to check it.".into());
    }
    Ok(format!(
        "Submitted and verified {} file{} on Canvas.",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssignmentSummary, Submission};

    fn assignment() -> AssignmentDetail {
        AssignmentDetail {
            base: AssignmentSummary {
                submission_types: vec!["online_upload".into()],
                ..Default::default()
            },
            allowed_extensions: vec!["pdf".into()],
            allowed_attempts: Some(2),
            ..Default::default()
        }
    }

    #[test]
    fn parses_and_orders_past_attempts_without_duplicating_current() {
        let value = serde_json::json!({
            "attempt": 2,
            "submitted_at": "2026-10-22T12:00:00Z",
            "submission_type": "online_upload",
            "attachments": [{"display_name": "new.pdf", "size": 20}],
            "submission_history": [
                {"attempt": 1, "submitted_at": "2026-10-20T12:00:00Z", "submission_type": "online_text_entry", "body": "<p>First draft</p>"},
                {"attempt": 2, "submitted_at": "2026-10-22T12:00:00Z", "submission_type": "online_upload", "attachments": [{"display_name": "new.pdf", "size": 20}]}
            ]
        });
        let history = History::parse(value).unwrap();
        assert_eq!(history.attempts.len(), 2);
        assert_eq!(history.attempts[0].attempt, Some(2));
        assert_eq!(history.attempts[0].attachments[0].name(), "new.pdf");
        assert_eq!(
            history.attempts[1].body.as_deref(),
            Some("<p>First draft</p>")
        );
        let fallback = History::parse(serde_json::json!({
            "attempt": 1,
            "submitted_at": "2026-10-22T12:00:00Z",
            "attachments": [],
            "submission_history": []
        }))
        .unwrap();
        assert_eq!(fallback.attempts.len(), 1);
        assert!(History::parse(serde_json::json!({"attempt": 2})).is_err());
    }

    #[test]
    fn validates_paths_extensions_and_duplicates_before_upload() {
        let dir =
            std::env::temp_dir().join(format!("tuicanvas-submit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("my work.PDF");
        let txt = dir.join("notes.txt");
        std::fs::write(&pdf, b"example").unwrap();
        std::fs::write(&txt, b"example").unwrap();
        let a = assignment();
        let first = choose_file(&pdf.to_string_lossy(), &a, &[]).unwrap();
        assert_eq!(first.size, 7);
        assert!(
            choose_file(&pdf.to_string_lossy(), &a, &[first])
                .unwrap_err()
                .contains("already")
        );
        assert!(
            choose_file(&txt.to_string_lossy(), &a, &[])
                .unwrap_err()
                .contains("only")
        );
        assert!(
            choose_file(&dir.to_string_lossy(), &a, &[])
                .unwrap_err()
                .contains("folder")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn respects_lock_and_attempt_limit() {
        let now = Utc::now();
        let mut a = assignment();
        assert!(eligible(&a, now).is_ok());
        a.base.submission = Some(Submission {
            attempt: Some(2),
            ..Default::default()
        });
        assert!(eligible(&a, now).unwrap_err().contains("attempt"));
        a.base.submission = None;
        a.base.locked_for_user = Some(true);
        assert!(eligible(&a, now).unwrap_err().contains("locked"));
        a.base.locked_for_user = None;
        a.base.submission_types = vec!["online_text_entry".into()];
        assert!(eligible(&a, now).unwrap_err().contains("does not accept"));
    }

    #[test]
    fn submitted_work_counts_as_a_prior_attempt_even_without_attempt_field() {
        let mut a = assignment();
        a.allowed_attempts = Some(1);
        a.base.submission = Some(Submission {
            submitted_at: Some(Utc::now()),
            ..Default::default()
        });
        assert_eq!(prior_attempts(&a), 1);
        assert!(eligible(&a, Utc::now()).unwrap_err().contains("attempt"));
    }

    #[test]
    fn suggests_paths_and_completes_selected_directory_or_file() {
        let root =
            std::env::temp_dir().join(format!("tuicanvas-path-suggestions-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Course Files")).unwrap();
        std::fs::create_dir_all(root.join(".hidden-folder")).unwrap();
        std::fs::write(root.join("coursework.PDF"), b"pdf").unwrap();
        std::fs::write(root.join(".notes"), b"note").unwrap();
        std::fs::write(root.join("other.txt"), b"text").unwrap();
        let mut suggestions = PathSuggestions::default();
        suggestions.update(&format!(
            "{}{}cou",
            root.display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(suggestions.matches.len(), 2);
        assert!(suggestions.matches[0].directory);
        assert!(
            suggestions
                .complete()
                .unwrap()
                .ends_with(&format!("Course Files{}", std::path::MAIN_SEPARATOR))
        );
        suggestions.move_selection(1);
        assert!(suggestions.complete().unwrap().ends_with("coursework.PDF"));
        suggestions.update(&format!(
            "{}{}COURSEW",
            root.display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(suggestions.matches.len(), 1);
        assert!(suggestions.complete().unwrap().ends_with("coursework.PDF"));
        suggestions.update(&format!("{}{}.", root.display(), std::path::MAIN_SEPARATOR));
        assert_eq!(suggestions.matches.len(), 1);
        assert_eq!(suggestions.matches[0].name, ".notes");
        std::fs::remove_dir_all(root).unwrap();
    }
}
