//! Locations and the configured Canvas origin.

use std::path::{Path, PathBuf};

pub const DEFAULT_ORIGIN: &str = "https://ufl.instructure.com";

#[derive(Clone, Debug)]
pub struct Config {
    /// `https://host` with no trailing slash.
    pub origin: String,
    pub host: String,
    /// Per-user application data: protected session and content snapshots.
    pub data_dir: PathBuf,
    /// Project checkout containing the login helper and private benchmark folder.
    pub project_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let origin =
            std::env::var("TUICANVAS_ORIGIN").unwrap_or_else(|_| DEFAULT_ORIGIN.to_string());
        let host = parse_origin(&origin)?;
        Ok(Config {
            origin: format!("https://{host}"),
            host,
            data_dir: data_dir()?,
            project_dir: project_dir(),
        })
    }

    /// Protected session file, scoped to the configured Canvas host.
    pub fn session_path(&self) -> PathBuf {
        let name = if self.host == "ufl.instructure.com" {
            "ufl.session.dpapi".to_string()
        } else {
            format!("{}.session.dpapi", self.host)
        };
        self.data_dir.join(name)
    }

    pub fn cache_root(&self) -> PathBuf {
        self.data_dir.join("cache").join("v1").join(&self.host)
    }

    /// The user's file library: `TUICANVAS_LIBRARY`, else `<home>\TUICanvas`.
    pub fn library_root(&self) -> PathBuf {
        if let Ok(dir) = std::env::var("TUICANVAS_LIBRARY") {
            return PathBuf::from(dir);
        }
        let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("TUICanvas")
    }

    pub fn private_reports(&self) -> PathBuf {
        self.project_dir.join("benchmarks").join("private")
    }

    pub fn login_helper(&self) -> Option<(PathBuf, PathBuf)> {
        let script = self.project_dir.join("tools").join("terminal_login.py");
        let python = if cfg!(windows) {
            self.project_dir
                .join(".venv")
                .join("Scripts")
                .join("python.exe")
        } else {
            self.project_dir.join(".venv").join("bin").join("python")
        };
        (script.is_file() && python.is_file()).then_some((python, self.project_dir.clone()))
    }
}

/// Accept only an https origin with a plain host name.
pub fn parse_origin(origin: &str) -> Result<String, String> {
    let rest = origin
        .strip_prefix("https://")
        .ok_or_else(|| "Canvas origin must start with https://".to_string())?;
    let host = rest.trim_end_matches('/');
    let valid = !host.is_empty()
        && host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if valid {
        Ok(host.to_ascii_lowercase())
    } else {
        Err("Canvas origin must be https://<host> with no path or port".to_string())
    }
}

fn data_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("TUICANVAS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if cfg!(windows) {
        let base =
            std::env::var("LOCALAPPDATA").map_err(|_| "LOCALAPPDATA is unavailable".to_string())?;
        return Ok(PathBuf::from(base).join("TUICanvas"));
    }
    if let Ok(base) = std::env::var("XDG_DATA_HOME") {
        return Ok(PathBuf::from(base).join("tuicanvas"));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is unavailable".to_string())?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("tuicanvas"))
}

fn project_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TUICANVAS_HOME") {
        return PathBuf::from(dir);
    }
    let marker = |dir: &Path| dir.join("tools").join("terminal_login.py").is_file();
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            if marker(dir) {
                return dir.to_path_buf();
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for dir in cwd.ancestors() {
        if marker(dir) {
            return dir.to_path_buf();
        }
    }
    cwd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_validation() {
        assert_eq!(
            parse_origin("https://ufl.instructure.com/").unwrap(),
            "ufl.instructure.com"
        );
        assert!(parse_origin("http://ufl.instructure.com").is_err());
        assert!(parse_origin("https://ufl.instructure.com/api").is_err());
        assert!(parse_origin("https://ufl.instructure.com:8443").is_err());
        assert!(parse_origin("https://user@host.com").is_err());
    }
}
