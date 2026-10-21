//! The local file library: `TUICanvas\<course>\Class Files\...` and
//! `TUICanvas\<course>\Assignment Files\<assignment>\...`.
//!
//! Each Canvas file is stored once. A file referenced from more than one place
//! appears in each through an NTFS hard link (same data, several names); if
//! linking fails, a second copy is made and both are tracked. A small index
//! records each file's Canvas version, local paths and last use, so the reader
//! can mark files that changed on Canvas, resync them in place (which updates
//! every linked name), and remove auto-downloaded images after 30 days unused.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Auto-downloaded images are removed after this long without use.
pub const IMAGE_RETENTION_DAYS: i64 = 30;
const MAX_NAME: usize = 120;

/// What Canvas says about a file, enough to place it and detect changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileVersion {
    /// Canvas `updated_at` (or `modified_at`) as sent.
    pub updated_at: Option<String>,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub version: FileVersion,
    pub paths: Vec<PathBuf>,
    pub last_used: DateTime<Utc>,
    /// Downloaded automatically (assignment images); removed after the retention period.
    pub automatic: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    files: HashMap<String, Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalState {
    NotDownloaded,
    Current,
    /// Canvas has a newer version than the local copy.
    Changed,
}

pub struct Library {
    root: PathBuf,
    index_path: PathBuf,
    index: Mutex<Index>,
}

/// A name that is valid on Windows (and elsewhere), keeping the extension.
pub fn safe_name(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        cleaned = "_".into();
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        cleaned.insert(0, '_');
    }
    if cleaned.chars().count() > MAX_NAME {
        let (base, ext) = match cleaned.rfind('.') {
            Some(i) if cleaned.len() - i <= 10 => {
                (cleaned[..i].to_string(), cleaned[i..].to_string())
            }
            _ => (cleaned.clone(), String::new()),
        };
        let keep = MAX_NAME.saturating_sub(ext.chars().count());
        cleaned = base
            .chars()
            .take(keep)
            .collect::<String>()
            .trim_end()
            .to_string()
            + &ext;
    }
    cleaned
}

/// Files that run code when opened. The reader shows these in their folder
/// instead of opening them.
pub fn is_program(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "exe"
            | "com"
            | "scr"
            | "pif"
            | "bat"
            | "cmd"
            | "ps1"
            | "psm1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "hta"
            | "msi"
            | "msp"
            | "msix"
            | "appx"
            | "appinstaller"
            | "lnk"
            | "url"
            | "cpl"
            | "reg"
            | "jar"
            | "dll"
            | "sys"
            | "inf"
            | "scf"
            | "application"
            | "gadget"
            | "settingcontent-ms"
            | "library-ms"
            | "search-ms"
            | "xll"
            | "iqy"
            | "slk"
            | "chm"
            | "msc"
    )
}

/// `name (id).ext`, used when two different files want the same name.
pub fn disambiguate(name: &str, id: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({id}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({id})"),
    }
}

/// Folder name for a course: its name, prefixed with the course code unless the name already has it.
pub fn course_folder(code: Option<&str>, name: &str) -> String {
    match code.filter(|c| !c.is_empty() && !name.contains(*c)) {
        Some(code) => safe_name(&format!("{code} {name}")),
        None => safe_name(name),
    }
}

/// Path components under `Class Files` for a Canvas folder `full_name`
/// such as `course files/Lectures/Week 1` (the root folder itself is dropped).
pub fn class_folder_parts(full_name: &str) -> Vec<String> {
    full_name
        .split('/')
        .skip(1)
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .map(safe_name)
        .collect()
}

impl Library {
    /// `root` is the TUICanvas folder; the index is per Canvas host.
    pub fn open(root: &Path, host: &str) -> Self {
        let index_path = root
            .join(".tuicanvas")
            .join(format!("index-{}.json", safe_name(host)));
        let index = std::fs::read(&index_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
            .filter(|i| i.version == 1)
            .unwrap_or(Index {
                version: 1,
                files: HashMap::new(),
            });
        Library {
            root: root.to_path_buf(),
            index_path,
            index: Mutex::new(index),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn class_files_dir(&self, course_folder: &str) -> PathBuf {
        self.root.join(course_folder).join("Class Files")
    }

    pub fn assignment_dir(&self, course_folder: &str, assignment_title: &str) -> PathBuf {
        self.root
            .join(course_folder)
            .join("Assignment Files")
            .join(safe_name(assignment_title))
    }

    /// Where partial downloads are written: on the same drive, so finishing is a rename.
    pub fn temp_dir(&self) -> PathBuf {
        self.root.join(".tuicanvas").join("partial")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, index: &Index) -> std::io::Result<()> {
        if let Some(dir) = self.index_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.index_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(index).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.index_path)
    }

    pub fn entry(&self, file_id: &str) -> Option<Entry> {
        let mut index = self.lock();
        let entry = index.files.get_mut(file_id)?;
        entry.paths.retain(|p| p.is_file());
        if entry.paths.is_empty() {
            index.files.remove(file_id);
            return None;
        }
        Some(entry.clone())
    }

    pub fn state(&self, file_id: &str, canvas: &FileVersion) -> LocalState {
        match self.entry(file_id) {
            None => LocalState::NotDownloaded,
            Some(e) if canvas.updated_at.is_some() && e.version.updated_at != canvas.updated_at => {
                LocalState::Changed
            }
            Some(e) if canvas.size.is_some() && e.version.size != canvas.size => {
                LocalState::Changed
            }
            Some(_) => LocalState::Current,
        }
    }

    /// Like `state`, from the index alone (no disk access), for drawing.
    pub fn known_state(&self, file_id: &str, canvas: &FileVersion) -> LocalState {
        let index = self.lock();
        match index.files.get(file_id) {
            None => LocalState::NotDownloaded,
            Some(e) if canvas.updated_at.is_some() && e.version.updated_at != canvas.updated_at => {
                LocalState::Changed
            }
            Some(e) if canvas.size.is_some() && e.version.size != canvas.size => {
                LocalState::Changed
            }
            Some(_) => LocalState::Current,
        }
    }

    /// The file the index places at `path`, if any.
    pub fn owner_of(&self, path: &Path) -> Option<String> {
        let index = self.lock();
        index
            .files
            .iter()
            .find(|(_, e)| e.paths.iter().any(|p| p == path))
            .map(|(id, _)| id.clone())
    }

    /// The user asked for this file: never remove it automatically.
    pub fn keep(&self, file_id: &str) {
        let mut index = self.lock();
        if let Some(e) = index.files.get_mut(file_id)
            && e.automatic
        {
            e.automatic = false;
            let _ = self.save(&index);
        }
    }

    /// Make `target` show an already-downloaded file: a hard link to an existing
    /// copy, or a plain copy if linking is impossible. Returns false if the file
    /// is not downloaded yet.
    pub fn reference(&self, file_id: &str, target: &Path) -> std::io::Result<bool> {
        let Some(entry) = self.entry(file_id) else {
            return Ok(false);
        };
        if entry.paths.iter().any(|p| p == target) {
            self.touch(file_id);
            return Ok(true);
        }
        let source = &entry.paths[0];
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if target.exists() {
            std::fs::remove_file(target)?;
        }
        if std::fs::hard_link(source, target).is_err() {
            std::fs::copy(source, target)?;
        }
        let mut index = self.lock();
        if let Some(e) = index.files.get_mut(file_id) {
            e.paths.push(target.to_path_buf());
            e.last_used = Utc::now();
        }
        self.save(&index)?;
        Ok(true)
    }

    /// Put a completed download in place. A new file is moved to `target`. A
    /// resync of a file that is already present is written into the existing
    /// copies in place, which keeps hard links intact.
    pub fn install(
        &self,
        file_id: &str,
        downloaded: &Path,
        version: FileVersion,
        target: &Path,
        automatic: bool,
    ) -> std::io::Result<()> {
        let existing = self.entry(file_id);
        let mut paths = Vec::new();
        match &existing {
            Some(entry) => {
                for path in &entry.paths {
                    overwrite_in_place(downloaded, path)?;
                }
                paths.extend(entry.paths.iter().cloned());
                std::fs::remove_file(downloaded)?;
                if !paths.iter().any(|p| p == target) {
                    // Show it at the requested place too.
                    if let Some(dir) = target.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    if target.exists() {
                        std::fs::remove_file(target)?;
                    }
                    if std::fs::hard_link(&paths[0], target).is_err() {
                        std::fs::copy(&paths[0], target)?;
                    }
                    paths.push(target.to_path_buf());
                }
            }
            None => {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                if target.exists() {
                    std::fs::remove_file(target)?;
                }
                if std::fs::rename(downloaded, target).is_err() {
                    std::fs::copy(downloaded, target)?;
                    std::fs::remove_file(downloaded)?;
                }
                paths.push(target.to_path_buf());
            }
        }
        let mut index = self.lock();
        // A file the user asked for is never auto-removed, even if it was first fetched automatically.
        let automatic = automatic && existing.as_ref().is_none_or(|e| e.automatic);
        index.files.insert(
            file_id.to_string(),
            Entry {
                version,
                paths,
                last_used: Utc::now(),
                automatic,
            },
        );
        self.save(&index)
    }

    /// Record a use, for the image retention rule.
    pub fn touch(&self, file_id: &str) {
        let mut index = self.lock();
        if let Some(e) = index.files.get_mut(file_id) {
            e.last_used = Utc::now();
            let _ = self.save(&index);
        }
    }

    /// Remove automatically downloaded files unused for the retention period.
    /// Returns how many files were removed.
    pub fn evict_unused(&self, now: DateTime<Utc>) -> usize {
        let cutoff = now - Duration::days(IMAGE_RETENTION_DAYS);
        let mut index = self.lock();
        let expired: Vec<String> = index
            .files
            .iter()
            .filter(|(_, e)| e.automatic && e.last_used < cutoff)
            .map(|(k, _)| k.clone())
            .collect();
        for key in &expired {
            if let Some(entry) = index.files.remove(key) {
                for path in entry.paths {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        if !expired.is_empty() {
            let _ = self.save(&index);
        }
        expired.len()
    }

    #[cfg(test)]
    fn set_last_used(&self, file_id: &str, at: DateTime<Utc>) {
        if let Some(e) = self.lock().files.get_mut(file_id) {
            e.last_used = at;
        }
    }
}

/// Replace a file's contents without replacing the file itself, so every hard link sees the new data.
fn overwrite_in_place(source: &Path, target: &Path) -> std::io::Result<()> {
    let mut from = std::fs::File::open(source)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(target)?;
    std::io::copy(&mut from, &mut to)?;
    to.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tuicanvas-lib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn version(at: &str, size: u64) -> FileVersion {
        FileVersion {
            updated_at: Some(at.into()),
            size: Some(size),
        }
    }

    #[test]
    fn names_are_safe_on_windows() {
        assert_eq!(
            safe_name("Lab 2: Kernels/Part A?.pdf"),
            "Lab 2_ Kernels_Part A_.pdf"
        );
        assert_eq!(safe_name("  notes.  "), "notes");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("com1"), "_com1");
        assert_eq!(safe_name("COMPUTER.txt"), "COMPUTER.txt");
        assert_eq!(safe_name(""), "_");
        let long = format!("{}.pdf", "x".repeat(300));
        let cut = safe_name(&long);
        assert!(cut.ends_with(".pdf") && cut.chars().count() <= MAX_NAME);
        assert_eq!(disambiguate("slides.pdf", "42"), "slides (42).pdf");
        assert_eq!(disambiguate("README", "7"), "README (7)");
    }

    #[test]
    fn course_and_folder_paths() {
        assert_eq!(
            course_folder(Some("EEL6763"), "GPU Computing"),
            "EEL6763 GPU Computing"
        );
        assert_eq!(
            course_folder(Some("EEL6763"), "EEL6763 - GPU Computing"),
            "EEL6763 - GPU Computing"
        );
        assert_eq!(course_folder(None, "Compilers"), "Compilers");
        assert_eq!(
            class_folder_parts("course files/Lectures/Week 1"),
            ["Lectures", "Week 1"]
        );
        assert_eq!(class_folder_parts("course files"), Vec::<String>::new());
        assert_eq!(class_folder_parts("course files/../x"), ["x"]);
    }

    #[test]
    fn one_copy_shared_by_two_places_and_resync_updates_both() {
        let root = temp_root("link");
        let lib = Library::open(&root, "ufl.instructure.com");
        let class_path = lib
            .class_files_dir("GPU")
            .join("Lectures")
            .join("slides.pdf");
        let assignment_path = lib
            .assignment_dir("GPU", "Lab-2 Assignment")
            .join("slides.pdf");

        let tmp = lib.temp_dir().join("1.part");
        write(&tmp, "v1");
        lib.install("1", &tmp, version("t1", 2), &class_path, false)
            .unwrap();
        assert!(!tmp.exists());
        assert_eq!(lib.state("1", &version("t1", 2)), LocalState::Current);
        assert!(lib.reference("1", &assignment_path).unwrap());
        assert_eq!(std::fs::read_to_string(&assignment_path).unwrap(), "v1");

        // Canvas changed the file; resync writes in place.
        assert_eq!(lib.state("1", &version("t2", 4)), LocalState::Changed);
        write(&tmp, "v2!!");
        lib.install("1", &tmp, version("t2", 4), &class_path, false)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&class_path).unwrap(), "v2!!");
        assert_eq!(std::fs::read_to_string(&assignment_path).unwrap(), "v2!!");
        assert_eq!(lib.state("1", &version("t2", 4)), LocalState::Current);

        // Deleting one name leaves the other, and the index forgets the missing path.
        std::fs::remove_file(&class_path).unwrap();
        assert_eq!(lib.entry("1").unwrap().paths, vec![assignment_path.clone()]);
        // The index survives a reopen.
        let reopened = Library::open(&root, "ufl.instructure.com");
        assert_eq!(reopened.state("1", &version("t2", 4)), LocalState::Current);
        assert!(
            !lib.reference("missing", &assignment_path.with_file_name("x"))
                .unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unused_automatic_images_are_removed_after_30_days() {
        let root = temp_root("evict");
        let lib = Library::open(&root, "ufl.instructure.com");
        let image = lib.assignment_dir("GPU", "Lab-2").join("diagram.png");
        let kept = lib.assignment_dir("GPU", "Lab-2").join("chosen.png");
        let tmp = lib.temp_dir().join("x.part");
        write(&tmp, "img");
        lib.install("10", &tmp, version("t", 3), &image, true)
            .unwrap();
        write(&tmp, "img");
        lib.install("11", &tmp, version("t", 3), &kept, false)
            .unwrap();
        let now = Utc::now();
        lib.set_last_used("10", now - Duration::days(31));
        lib.set_last_used("11", now - Duration::days(400));
        assert_eq!(lib.evict_unused(now), 1);
        assert!(!image.exists());
        assert!(kept.exists(), "files the user chose are never auto-removed");
        // Re-fetching automatically an image the user had chosen keeps it chosen.
        write(&tmp, "img");
        lib.install("11", &tmp, version("t", 3), &kept, true)
            .unwrap();
        lib.set_last_used("11", now - Duration::days(400));
        assert_eq!(lib.evict_unused(now), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
