//! Where things go on Linux: the XDG base directories and user
//! directories, standing in for macOS's search-path directories.
//!
//! | Search-path directory | User domain | Local and system domains |
//! |---|---|---|
//! | Application Support, Library | `$XDG_DATA_HOME` (`~/.local/share`) | `$XDG_DATA_DIRS` (`/usr/local/share`, `/usr/share`) |
//! | Caches | `$XDG_CACHE_HOME` (`~/.cache`) | `/var/cache` |
//! | Autosaved Information | `$XDG_DATA_HOME/Autosave Information` | |
//! | Trash | `$XDG_DATA_HOME/Trash` | |
//! | Documents, Desktop, Downloads, Music, Movies, Pictures, Public | `user-dirs.dirs` (`~/Documents`, `~/Videos` for movies, ...) | |
//! | Applications | `~/Applications` | |
//! | Users | | `/home` |
//!
//! Environment variables win over `$XDG_CONFIG_HOME/user-dirs.dirs`, which
//! is read once, on first use; relative values are ignored, as the XDG
//! specification says. Other directories have no Linux counterpart and
//! give no paths.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `NSSearchPathDirectory` values.
pub(crate) mod dir {
    pub(crate) const APPLICATIONS: usize = 1;
    pub(crate) const LIBRARY: usize = 5;
    pub(crate) const USERS: usize = 7;
    pub(crate) const DOCUMENTS: usize = 9;
    pub(crate) const AUTOSAVED_INFORMATION: usize = 11;
    pub(crate) const DESKTOP: usize = 12;
    pub(crate) const CACHES: usize = 13;
    pub(crate) const APPLICATION_SUPPORT: usize = 14;
    pub(crate) const DOWNLOADS: usize = 15;
    pub(crate) const MOVIES: usize = 17;
    pub(crate) const MUSIC: usize = 18;
    pub(crate) const PICTURES: usize = 19;
    pub(crate) const SHARED_PUBLIC: usize = 21;
    pub(crate) const ITEM_REPLACEMENT: usize = 99;
    pub(crate) const ALL_APPLICATIONS: usize = 100;
    pub(crate) const ALL_LIBRARIES: usize = 101;
    pub(crate) const TRASH: usize = 102;
}

/// `NSSearchPathDomainMask` bits.
pub(crate) mod domain {
    pub(crate) const USER: usize = 1;
    pub(crate) const LOCAL: usize = 2;
    pub(crate) const NETWORK: usize = 4;
    pub(crate) const SYSTEM: usize = 8;
}

/// What the resolver reads: environment variables, the home directory and
/// `user-dirs.dirs`. Tests pass their own.
pub(crate) struct Env<'a> {
    pub(crate) var: &'a dyn Fn(&str) -> Option<String>,
    pub(crate) home: PathBuf,
    pub(crate) user_dirs: &'a dyn Fn(&Path) -> Vec<(String, PathBuf)>,
}

impl Env<'_> {
    /// An absolute path from a variable, or the default under home.
    fn base(&self, name: &str, default: &str) -> PathBuf {
        (self.var)(name).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| self.home.join(default))
    }

    pub(crate) fn data_home(&self) -> PathBuf {
        self.base("XDG_DATA_HOME", ".local/share")
    }

    pub(crate) fn config_home(&self) -> PathBuf {
        self.base("XDG_CONFIG_HOME", ".config")
    }

    pub(crate) fn cache_home(&self) -> PathBuf {
        self.base("XDG_CACHE_HOME", ".cache")
    }

    pub(crate) fn data_dirs(&self) -> Vec<PathBuf> {
        let dirs = (self.var)("XDG_DATA_DIRS").filter(|d| !d.is_empty());
        let dirs = dirs.as_deref().unwrap_or("/usr/local/share/:/usr/share/");
        dirs.split(':').map(PathBuf::from).filter(|p| p.is_absolute()).collect()
    }

    /// A user directory such as `XDG_DOCUMENTS_DIR`: the variable, then
    /// `user-dirs.dirs`, then the default under home.
    pub(crate) fn user_dir(&self, key: &str, default: &str) -> PathBuf {
        if let Some(path) = (self.var)(key).map(PathBuf::from).filter(|p| p.is_absolute()) {
            return path;
        }
        let file = self.config_home().join("user-dirs.dirs");
        (self.user_dirs)(&file)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p)
            .unwrap_or_else(|| self.home.join(default))
    }

    /// The paths for a search-path directory in the domains of a mask, user
    /// domain first.
    pub(crate) fn search_path(&self, directory: usize, mask: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let domains = [domain::USER, domain::LOCAL, domain::NETWORK, domain::SYSTEM];
        for domain in domains.into_iter().filter(|d| mask & d != 0) {
            match directory {
                dir::ALL_APPLICATIONS => out.extend(self.in_domain(dir::APPLICATIONS, domain)),
                dir::ALL_LIBRARIES => out.extend(self.in_domain(dir::LIBRARY, domain)),
                directory => out.extend(self.in_domain(directory, domain)),
            }
        }
        out
    }

    fn in_domain(&self, directory: usize, domain: usize) -> Vec<PathBuf> {
        let data_dirs = || {
            let dirs = self.data_dirs();
            match domain {
                domain::LOCAL => dirs.into_iter().take(1).collect(),
                domain::SYSTEM => dirs.into_iter().skip(1).collect(),
                _ => Vec::new(),
            }
        };
        match (directory, domain) {
            (dir::APPLICATION_SUPPORT | dir::LIBRARY, domain::USER) => vec![self.data_home()],
            (dir::APPLICATION_SUPPORT | dir::LIBRARY, _) => data_dirs(),
            (dir::CACHES, domain::USER) => vec![self.cache_home()],
            (dir::CACHES, domain::LOCAL) => vec![PathBuf::from("/var/cache")],
            (dir::AUTOSAVED_INFORMATION, domain::USER) => vec![self.data_home().join("Autosave Information")],
            (dir::TRASH, domain::USER) => vec![self.data_home().join("Trash")],
            (dir::DOCUMENTS, domain::USER) => vec![self.user_dir("XDG_DOCUMENTS_DIR", "Documents")],
            (dir::DESKTOP, domain::USER) => vec![self.user_dir("XDG_DESKTOP_DIR", "Desktop")],
            (dir::DOWNLOADS, domain::USER) => vec![self.user_dir("XDG_DOWNLOAD_DIR", "Downloads")],
            (dir::MUSIC, domain::USER) => vec![self.user_dir("XDG_MUSIC_DIR", "Music")],
            (dir::MOVIES, domain::USER) => vec![self.user_dir("XDG_VIDEOS_DIR", "Videos")],
            (dir::PICTURES, domain::USER) => vec![self.user_dir("XDG_PICTURES_DIR", "Pictures")],
            (dir::SHARED_PUBLIC, domain::USER) => vec![self.user_dir("XDG_PUBLICSHARE_DIR", "Public")],
            (dir::APPLICATIONS, domain::USER) => vec![self.home.join("Applications")],
            (dir::USERS, domain::LOCAL) => vec![PathBuf::from("/home")],
            _ => Vec::new(),
        }
    }
}

/// Parse `user-dirs.dirs`: `XDG_NAME_DIR="$HOME/Name"` or an absolute
/// path, one per line.
pub(crate) fn parse_user_dirs(text: &str, home: &Path) -> Vec<(String, PathBuf)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            let path = if let Some(rest) = value.strip_prefix("$HOME") {
                let rest = rest.trim_start_matches('/');
                if rest.is_empty() { home.to_path_buf() } else { home.join(rest) }
            } else if value.starts_with('/') {
                PathBuf::from(value)
            } else {
                return None;
            };
            Some((key.trim().to_string(), path))
        })
        .collect()
}

/// The current user's home: `$HOME` when it is absolute, else the
/// password database's entry, else "/".
pub(crate) fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return home;
    }
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    passwd_by_uid(uid).map(|p| p.home).unwrap_or_else(|| PathBuf::from("/"))
}

/// A user's password-database entry, the parts Foundation reports.
pub(crate) struct Passwd {
    pub(crate) name: String,
    pub(crate) full_name: String,
    pub(crate) home: PathBuf,
}

fn passwd_from(entry: &libc::passwd) -> Passwd {
    let text = |p: *const libc::c_char| {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: the password database's strings are NUL-terminated.
            unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    // The GECOS field's first comma-separated part is the full name.
    let gecos = text(entry.pw_gecos);
    Passwd {
        name: text(entry.pw_name),
        full_name: gecos.split(',').next().unwrap_or("").to_string(),
        home: PathBuf::from(text(entry.pw_dir)),
    }
}

/// Look a user up by id.
pub(crate) fn passwd_by_uid(uid: libc::uid_t) -> Option<Passwd> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: zeroed is a valid passwd; getpwuid_r fills it and points its
    // strings into `buffer`.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: as above.
    let status = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    (status == 0 && !result.is_null()).then(|| passwd_from(&entry))
}

/// Look a user up by name.
pub(crate) fn passwd_by_name(name: &str) -> Option<Passwd> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: as in passwd_by_uid.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: as above; `name` is NUL-terminated.
    let status = unsafe { libc::getpwnam_r(name.as_ptr(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    (status == 0 && !result.is_null()).then(|| passwd_from(&entry))
}

/// The current user's entry, looked up once.
pub(crate) fn current_user() -> Option<&'static Passwd> {
    static USER: OnceLock<Option<Passwd>> = OnceLock::new();
    // SAFETY: getuid can't fail.
    USER.get_or_init(|| passwd_by_uid(unsafe { libc::getuid() })).as_ref()
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub(crate) fn config_home() -> PathBuf {
    let var = |name: &str| std::env::var(name).ok();
    let no_user_dirs = |_: &Path| Vec::new();
    Env { var: &var, home: home(), user_dirs: &no_user_dirs }.config_home()
}

/// The temporary directory: `$TMPDIR` when absolute, else `/tmp`.
pub(crate) fn temporary() -> PathBuf {
    std::env::var_os("TMPDIR").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Search paths for this process's environment. `user-dirs.dirs` is read
/// once.
pub(crate) fn search_path(directory: usize, mask: usize) -> Vec<PathBuf> {
    static USER_DIRS: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();
    let home = home();
    let var = |name: &str| std::env::var(name).ok();
    let user_dirs = |file: &Path| {
        USER_DIRS
            .get_or_init(|| std::fs::read_to_string(file).map(|t| parse_user_dirs(&t, &home)).unwrap_or_default())
            .clone()
    };
    Env { var: &var, home: home.clone(), user_dirs: &user_dirs }.search_path(directory, mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    fn user_dirs(text: &str) -> impl Fn(&Path) -> Vec<(String, PathBuf)> + '_ {
        move |file: &Path| {
            assert!(file.ends_with("user-dirs.dirs"));
            parse_user_dirs(text, Path::new("/home/u"))
        }
    }

    #[test]
    fn defaults() {
        let (var, dirs) = (vars(&[]), user_dirs(""));
        let e = Env { var: &var, home: PathBuf::from("/home/u"), user_dirs: &dirs };
        let p = |d, m| e.search_path(d, m);
        assert_eq!(p(dir::APPLICATION_SUPPORT, domain::USER), [PathBuf::from("/home/u/.local/share")]);
        assert_eq!(p(dir::CACHES, domain::USER), [PathBuf::from("/home/u/.cache")]);
        assert_eq!(p(dir::DOCUMENTS, domain::USER), [PathBuf::from("/home/u/Documents")]);
        assert_eq!(p(dir::MOVIES, domain::USER), [PathBuf::from("/home/u/Videos")]);
        assert_eq!(p(dir::TRASH, domain::USER), [PathBuf::from("/home/u/.local/share/Trash")]);
        assert_eq!(
            p(dir::APPLICATION_SUPPORT, 0xffff),
            [PathBuf::from("/home/u/.local/share"), PathBuf::from("/usr/local/share/"), PathBuf::from("/usr/share/")]
        );
        assert_eq!(p(dir::USERS, domain::USER), Vec::<PathBuf>::new());
        assert_eq!(p(dir::USERS, 0xffff), [PathBuf::from("/home")]);
        assert_eq!(p(dir::ITEM_REPLACEMENT, 0xffff), Vec::<PathBuf>::new());
        assert_eq!(p(dir::ALL_LIBRARIES, domain::USER), [PathBuf::from("/home/u/.local/share")]);
    }

    #[test]
    fn variables_and_user_dirs() {
        let variables = [
            ("XDG_DATA_HOME", "/data"),
            ("XDG_CACHE_HOME", "relative/ignored"),
            ("XDG_DATA_DIRS", "/a:/b:/c"),
            ("XDG_MUSIC_DIR", "/music"),
        ];
        let dirs = "# comment\nXDG_DOCUMENTS_DIR=\"$HOME/Docs\"\nXDG_DESKTOP_DIR=\"$HOME/\"\nXDG_DOWNLOAD_DIR=\"/dl\"\nXDG_PICTURES_DIR=\"rel\"\nXDG_MUSIC_DIR=\"$HOME/NotThis\"\n";
        let (var, dirs) = (vars(&variables), user_dirs(dirs));
        let e = Env { var: &var, home: PathBuf::from("/home/u"), user_dirs: &dirs };
        let p = |d, m| e.search_path(d, m);
        assert_eq!(p(dir::APPLICATION_SUPPORT, domain::USER), [PathBuf::from("/data")]);
        assert_eq!(p(dir::CACHES, domain::USER), [PathBuf::from("/home/u/.cache")]);
        assert_eq!(
            p(dir::APPLICATION_SUPPORT, domain::LOCAL | domain::SYSTEM),
            [PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")]
        );
        assert_eq!(p(dir::DOCUMENTS, domain::USER), [PathBuf::from("/home/u/Docs")]);
        assert_eq!(p(dir::DESKTOP, domain::USER), [PathBuf::from("/home/u")]);
        assert_eq!(p(dir::DOWNLOADS, domain::USER), [PathBuf::from("/dl")]);
        assert_eq!(p(dir::PICTURES, domain::USER), [PathBuf::from("/home/u/Pictures")], "relative entries are ignored");
        assert_eq!(p(dir::MUSIC, domain::USER), [PathBuf::from("/music")], "variables win");
    }
}
