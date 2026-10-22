//! Account-scoped snapshots and local UI preferences. Snapshots are displayed
//! as saved copies while fresh requests run; preferences are kept separately.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKey {
    Courses,
    /// One month of the home-screen calendar (`2026-10`).
    Planner(String),
    /// The student's course colours.
    Colors,
    Assignments(String),
    Detail(String, String),
    /// Loaded on demand; kept only in memory because attempts may contain work.
    History(String, String),
    Announcements(String),
    Announcement(String, String),
    /// A course's Files area (folder tree and file list).
    Files(String),
    /// One Canvas file's metadata.
    File(String),
}

impl ResourceKey {
    fn file_name(&self) -> Option<String> {
        let ok = |id: &str| crate::model::valid_id(id);
        let name = match self {
            ResourceKey::Courses => "courses".to_string(),
            ResourceKey::Planner(m) if crate::calendar::month_of(m).is_some() && m.len() == 7 => {
                format!("planner-{m}")
            }
            ResourceKey::Colors => "colors".to_string(),
            ResourceKey::Announcements(c) if ok(c) => {
                format!("announcements-{}", c.replace('~', "_"))
            }
            ResourceKey::Announcement(c, a) if ok(c) && ok(a) => format!(
                "announcement-{}-{}",
                c.replace('~', "_"),
                a.replace('~', "_")
            ),
            ResourceKey::Assignments(c) if ok(c) => format!("assignments-{}", c.replace('~', "_")),
            ResourceKey::Detail(c, a) if ok(c) && ok(a) => {
                format!("assignment-{}-{}", c.replace('~', "_"), a.replace('~', "_"))
            }
            ResourceKey::History(..) => return None,
            ResourceKey::Files(c) if ok(c) => format!("files-{}", c.replace('~', "_")),
            ResourceKey::File(f) if ok(f) => format!("file-{}", f.replace('~', "_")),
            _ => return None,
        };
        Some(name + ".json")
    }

    pub fn label(&self) -> &'static str {
        match self {
            ResourceKey::Courses => "courses",
            ResourceKey::Planner(_) => "calendar",
            ResourceKey::Colors => "colors",
            ResourceKey::Announcements(_) => "announcements",
            ResourceKey::Announcement(..) => "announcement",
            ResourceKey::Assignments(_) => "assignments",
            ResourceKey::Detail(..) => "assignment",
            ResourceKey::History(..) => "submission history",
            ResourceKey::Files(_) => "files",
            ResourceKey::File(_) => "file",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u32,
    account: String,
    fetched_at: DateTime<Utc>,
    data: T,
}

const VERSION: u32 = 1;

pub struct SnapshotStore {
    dir: PathBuf,
    account: String,
}

impl SnapshotStore {
    /// `root` is origin-scoped; each account gets its own directory.
    pub fn new(root: &Path, account: &str) -> Self {
        let safe: String = account
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '~')
            .collect();
        SnapshotStore {
            dir: root.join(format!("user-{safe}")),
            account: account.to_string(),
        }
    }

    pub fn load<T: DeserializeOwned>(&self, key: &ResourceKey) -> Option<(T, DateTime<Utc>)> {
        self.load_named(&key.file_name()?)
    }

    fn load_named<T: DeserializeOwned>(&self, name: &str) -> Option<(T, DateTime<Utc>)> {
        let bytes = std::fs::read(self.dir.join(name)).ok()?;
        let env: Envelope<T> = serde_json::from_slice(&bytes).ok()?;
        (env.version == VERSION && env.account == self.account)
            .then_some((env.data, env.fetched_at))
    }

    pub fn save<T: Serialize>(&self, key: &ResourceKey, data: &T, fetched_at: DateTime<Utc>) {
        let Some(name) = key.file_name() else { return };
        self.save_named(&name, data, fetched_at);
    }

    fn save_named<T: Serialize>(&self, name: &str, data: &T, fetched_at: DateTime<Utc>) {
        let env = Envelope {
            version: VERSION,
            account: self.account.clone(),
            fetched_at,
            data,
        };
        let Ok(bytes) = serde_json::to_vec(&env) else {
            return;
        };
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Local reading history is separate from refreshed Canvas snapshots.
    pub fn announcement_reads(&self) -> std::collections::BTreeSet<(String, String)> {
        self.load_named("local-announcement-reads.json")
            .map(|(reads, _)| reads)
            .unwrap_or_default()
    }

    pub fn save_announcement_reads(&self, reads: &std::collections::BTreeSet<(String, String)>) {
        self.save_named("local-announcement-reads.json", reads, Utc::now());
    }

    /// Local course visibility does not change Canvas enrollment or its course snapshot.
    pub fn hidden_courses(&self) -> std::collections::BTreeSet<String> {
        self.load_named("local-hidden-courses.json")
            .map(|(ids, _)| ids)
            .unwrap_or_default()
    }

    pub fn save_hidden_courses(&self, ids: &std::collections::BTreeSet<String>) {
        self.save_named("local-hidden-courses.json", ids, Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tuicanvas-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn snapshots_are_account_scoped() {
        let root = temp_root("scope");
        let a = SnapshotStore::new(&root, "100");
        let b = SnapshotStore::new(&root, "200");
        let when = Utc::now();
        a.save(&ResourceKey::Courses, &vec!["x".to_string()], when);
        let (data, at): (Vec<String>, _) = a.load(&ResourceKey::Courses).unwrap();
        assert_eq!(data, vec!["x"]);
        assert_eq!(at, when);
        assert!(b.load::<Vec<String>>(&ResourceKey::Courses).is_none());
        a.clear();
        assert!(a.load::<Vec<String>>(&ResourceKey::Courses).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unsafe_keys() {
        assert!(
            ResourceKey::Assignments("../x".into())
                .file_name()
                .is_none()
        );
        assert_eq!(
            ResourceKey::Detail("1~2".into(), "3".into())
                .file_name()
                .unwrap(),
            "assignment-1_2-3.json"
        );
    }
}
