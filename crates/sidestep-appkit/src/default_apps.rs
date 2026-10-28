//! Which application opens which type, as the freedesktop.org MIME
//! applications specification has it (what `xdg-mime query default` reads
//! and `xdg-mime default` writes): `mimeapps.list` files name the defaults,
//! desktop-specific ones (`gnome-mimeapps.list`) before the rest, in the
//! configuration directories and then the data directories'
//! `applications`; `mimeinfo.cache` lists the applications whose desktop
//! entries declare a type. Applications are desktop entries, named by their
//! desktop IDs (`org.gnome.TextEditor.desktop`). And how an application
//! starts: its desktop entry's `Exec` line.

use std::path::{Path, PathBuf};

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn env_dirs(name: &str, default: &str) -> Vec<PathBuf> {
    let value = std::env::var_os(name).filter(|v| !v.is_empty()).unwrap_or_else(|| default.into());
    std::env::split_paths(&value).filter(|p| p.is_absolute()).collect()
}

fn config_home() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".config")))
}

/// The data directories, the user's first.
fn data_dirs() -> Vec<PathBuf> {
    let home = env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local/share")));
    home.into_iter().chain(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share")).collect()
}

/// The directories holding desktop entries, the user's first.
pub(crate) fn applications_dirs() -> Vec<PathBuf> {
    data_dirs().into_iter().map(|d| d.join("applications")).collect()
}

/// The PNG of the icon desktop entry `id` names: its `Icon` path, or the
/// largest of the usual sizes in the hicolor theme, or in `pixmaps`.
pub(crate) fn icon_file(id: &str) -> Option<PathBuf> {
    let entry = applications_dirs().into_iter().map(|d| d.join(id)).find(|p| p.is_file())?;
    let icon = values(&std::fs::read_to_string(entry).ok()?, "Desktop Entry", "Icon").into_iter().next()?;
    if Path::new(&icon).is_absolute() {
        return Path::new(&icon).is_file().then(|| PathBuf::from(&icon));
    }
    let sizes = [512, 256, 192, 128, 96, 64, 48];
    let dirs = data_dirs();
    let icon = icon.as_str();
    let themed =
        sizes.iter().flat_map(|s| dirs.iter().map(move |d| d.join(format!("icons/hicolor/{s}x{s}/apps/{icon}.png"))));
    let pixmaps = dirs.iter().map(|d| d.join(format!("pixmaps/{icon}.png")));
    themed.chain(pixmaps).find(|p| p.is_file())
}

/// The `mimeapps.list` files, in order of precedence.
fn list_files() -> Vec<PathBuf> {
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_lowercase)
        .collect();
    let dirs = config_home().into_iter().chain(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg")).chain(applications_dirs());
    dirs.flat_map(|d| {
        let named: Vec<PathBuf> = desktops.iter().map(|desktop| d.join(format!("{desktop}-mimeapps.list"))).collect();
        named.into_iter().chain([d.join("mimeapps.list")])
    })
    .collect()
}

/// The `;`-separated values of `key` in `[section]` of a desktop-entry-style
/// file's text.
fn values(text: &str, section: &str, key: &str) -> Vec<String> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == section;
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return v.split(';').map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned).collect();
        }
    }
    Vec::new()
}

/// The desktop IDs that open `mime`: the defaults first, then the
/// applications that declare it.
pub(crate) fn handlers(mime: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
    let defaults = list_files().into_iter().flat_map(|f| values(&read(&f), "Default Applications", mime));
    let declared =
        applications_dirs().into_iter().flat_map(|d| values(&read(&d.join("mimeinfo.cache")), "MIME Cache", mime));
    for id in defaults.chain(declared) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Make desktop ID `id` the default for `mime` in the user's
/// `mimeapps.list`, keeping the rest of the file as it was.
pub(crate) fn set_default(mime: &str, id: &str) -> std::io::Result<()> {
    let dir = config_home().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    let path = dir.join("mimeapps.list");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::create_dir_all(&dir)?;
    let temporary = dir.join(format!(".mimeapps.list.{}", std::process::id()));
    std::fs::write(&temporary, with_default(&text, mime, id))?;
    std::fs::rename(&temporary, &path)
}

/// `text` with `mime=id;` in `[Default Applications]`, replacing the line
/// there for `mime` or adding one (and the section, if missing).
fn with_default(text: &str, mime: &str, id: &str) -> String {
    let entry = format!("{mime}={id};");
    let mut out = Vec::new();
    let (mut inside, mut done) = (false, false);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if inside && !done {
                out.push(entry.clone());
                done = true;
            }
            inside = trimmed == "[Default Applications]";
        } else if inside && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == mime) {
            if !done {
                out.push(entry.clone());
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        if !inside {
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            out.push("[Default Applications]".to_owned());
        }
        out.push(entry);
    }
    out.join("\n") + "\n"
}

/// The command a desktop entry starts with no documents: its `Exec` line's
/// arguments, unquoted, without field codes (`%f`, `%U`, …).
pub(crate) fn exec_command(entry: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(entry).ok()?;
    let mut inside = false;
    let exec = text.lines().map(str::trim).find_map(|line| {
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            None
        } else if inside {
            line.strip_prefix("Exec=")
        } else {
            None
        }
    })?;
    let command: Vec<String> =
        split_exec(exec).into_iter().filter(|a| !is_field_code(a)).map(|a| a.replace("%%", "%")).collect();
    (!command.is_empty()).then_some(command)
}

fn is_field_code(arg: &str) -> bool {
    arg.len() == 2 && arg.starts_with('%') && "fFuUdDnNickvm".contains(&arg[1..])
}

/// An `Exec` value's arguments: separated by spaces, double quotes around
/// one argument, and a backslash escaping `"`, `` ` ``, `$` or `\` inside them.
fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' if !quoted => {
                if started || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
                started = false;
            }
            c => current.push(c),
        }
    }
    if started || !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_the_named_section() {
        let text = "[Added Associations]\ntext/markdown=b.desktop;\n\n[Default Applications]\ntext/plain=c.desktop\ntext/markdown = a.desktop;b.desktop;\n";
        assert_eq!(values(text, "Default Applications", "text/markdown"), ["a.desktop", "b.desktop"]);
        assert_eq!(values(text, "Default Applications", "image/png"), Vec::<String>::new());
    }

    #[test]
    fn setting_a_default_keeps_the_rest() {
        let text =
            "[Default Applications]\ntext/plain=c.desktop;\ntext/markdown=old.desktop;\n[Added Associations]\nx=y;\n";
        assert_eq!(
            with_default(text, "text/markdown", "new.desktop"),
            "[Default Applications]\ntext/plain=c.desktop;\ntext/markdown=new.desktop;\n[Added Associations]\nx=y;\n"
        );
        assert_eq!(
            with_default("[Added Associations]\nx=y;\n", "text/markdown", "new.desktop"),
            "[Added Associations]\nx=y;\n\n[Default Applications]\ntext/markdown=new.desktop;\n"
        );
        assert_eq!(with_default("", "a/b", "c.desktop"), "[Default Applications]\na/b=c.desktop;\n");
        assert_eq!(
            with_default("[Default Applications]\nx/y=z.desktop;\n[Other]\n", "a/b", "c.desktop"),
            "[Default Applications]\nx/y=z.desktop;\na/b=c.desktop;\n[Other]\n"
        );
    }

    #[test]
    fn exec_lines_lose_their_field_codes() {
        assert_eq!(
            split_exec(r#"env A=1 "/opt/My App/bin/app" --flag %U"#),
            ["env", "A=1", "/opt/My App/bin/app", "--flag", "%U"]
        );
        assert_eq!(split_exec(r#"sh -c "echo \"hi\" \$HOME""#), ["sh", "-c", r#"echo "hi" $HOME"#]);
        assert!(is_field_code("%F") && !is_field_code("%%") && !is_field_code("--x"));
    }
}
