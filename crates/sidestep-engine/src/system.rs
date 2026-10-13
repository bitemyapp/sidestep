//! Where the engine finds what it uses from the system at run time:
//! fontconfig's library, X11's compose tables and XKB's keymap data.
//!
//! A distribution laid out as most are has them where everyone looks
//! (`libfontconfig.so.1` on the library path, `/usr/share/X11/locale`,
//! `/usr/share/X11/xkb`). NixOS (and Guix) keep each package under a
//! prefix of its own in the store, so a program built there with Cargo
//! finds none of them by name. Each is looked for in turn:
//!
//! 1. where the environment says (`LD_LIBRARY_PATH`, `XLOCALEDIR`,
//!    `XKB_CONFIG_ROOT`), as everywhere;
//! 2. where the build said: `SIDESTEP_FONTCONFIG` (the library file),
//!    `SIDESTEP_XLOCALEDIR` and `SIDESTEP_XKB_CONFIG_ROOT`, read when the
//!    engine is compiled, are what a Nix package or shell sets, as nixpkgs
//!    builds libxkbcommon with its store paths (and a Nix build then keeps
//!    them in the program's closure);
//! 3. the usual places;
//! 4. the data directories (`$XDG_DATA_DIRS`, which on NixOS lists the
//!    system's and the user's profiles); for fontconfig, the `lib`
//!    directory beside each, and the library the system's own `fc-match`
//!    runs with (its ELF run path, which names the store path on NixOS,
//!    where the font configuration installs it).
//!
//! What none of them finds is done without, as before: fonts from the
//! usual font directories ([`fonts`](crate::text::fonts)), no compose
//! sequences.

use std::path::{Path, PathBuf};
use std::sync::Once;

use kbvm::xkb::Context;

const FONTCONFIG: &str = "libfontconfig.so.1";

/// Make sure `libfontconfig.so.1` loads by that name, as fontique opens
/// it: if the dynamic linker doesn't find it, load it from where it can be
/// found. The dynamic linker answers a `dlopen` of a name with a library
/// already loaded under it.
pub fn load_fontconfig() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if open(Path::new(FONTCONFIG)) {
            return;
        }
        for path in fontconfig_candidates() {
            if open(&path) {
                return;
            }
        }
    });
}

/// Load the library at `path` (by name if it has no slash) for the rest of
/// the program's life, as fontique keeps it. A library that won't load
/// (another word size, a newer C library than ours) is passed over.
fn open(path: &Path) -> bool {
    use libloading::os::unix::{Library, RTLD_NOW};
    // SAFETY: fontconfig's initializers set up only its own state.
    match unsafe { Library::open(Some(path), RTLD_NOW) } {
        Ok(library) => {
            let _ = library.into_raw();
            true
        }
        Err(_) => false,
    }
}

fn fontconfig_candidates() -> Vec<PathBuf> {
    let built = option_env!("SIDESTEP_FONTCONFIG").map(PathBuf::from);
    let beside_data = data_dirs()
        .into_iter()
        .filter(|d| d.ends_with("share"))
        .filter_map(|d| Some(d.parent()?.join("lib").join(FONTCONFIG)));
    let tools = ["fc-match", "fc-list"].into_iter().filter_map(on_path);
    let from_tools = tools.flat_map(|tool| run_path(&tool)).map(|d| d.join(FONTCONFIG));
    let mut candidates: Vec<PathBuf> = Vec::new();
    for path in built.into_iter().chain(beside_data).chain(from_tools) {
        if path.is_file() && !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates
}

/// The data directories, `$XDG_DATA_DIRS`, or their default.
pub fn data_dirs() -> Vec<PathBuf> {
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::env::split_paths(&dirs).filter(|d| d.is_absolute()).collect()
}

/// An XKB context that finds the compose tables and keymap data where
/// this module says.
pub fn xkb_context() -> Context {
    let mut builder = Context::builder();
    builder.environment_accessor(|name| {
        std::env::var(name).ok().or_else(|| match name {
            "XLOCALEDIR" => x11_data(option_env!("SIDESTEP_XLOCALEDIR"), "locale", &[]),
            "XKB_CONFIG_ROOT" => x11_data(option_env!("SIDESTEP_XKB_CONFIG_ROOT"), "xkb", &["/etc/X11/xkb"]),
            _ => None,
        })
    });
    builder.build()
}

/// Where XKB's keymap data is, if anywhere: what keymaps named by layout
/// (rather than sent by the compositor) are built from.
pub fn xkb_config_root() -> Option<PathBuf> {
    std::env::var_os("XKB_CONFIG_ROOT")
        .map(PathBuf::from)
        .or_else(|| x11_data(option_env!("SIDESTEP_XKB_CONFIG_ROOT"), "xkb", &["/etc/X11/xkb"]).map(PathBuf::from))
}

/// The first directory of X11's data named `name` (`locale`, `xkb`): the
/// one the build named, the usual one, one of `also`, or one in the data
/// directories.
fn x11_data(built: Option<&str>, name: &str, also: &[&str]) -> Option<String> {
    let usual = Path::new("/usr/share/X11").join(name);
    let data = data_dirs().into_iter().map(|d| d.join("X11").join(name));
    built
        .map(PathBuf::from)
        .into_iter()
        .chain([usual])
        .chain(also.iter().map(PathBuf::from))
        .chain(data)
        .find(|d| d.is_dir())
        .and_then(|d| d.into_os_string().into_string().ok())
}

/// `name`'s file in the first directory of `$PATH` that has it, with
/// symbolic links resolved (for its `$ORIGIN`).
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|f| f.is_file())?.canonicalize().ok()
}

/// The directories an ELF executable names for its libraries
/// (`DT_RUNPATH`, else `DT_RPATH`), `$ORIGIN` replaced by its own
/// directory. None for anything else.
fn run_path(exe: &Path) -> Vec<PathBuf> {
    let Some(dirs) = std::fs::read(exe).ok().and_then(|file| elf_run_path(&file)) else {
        return Vec::new();
    };
    let origin = exe.parent().and_then(Path::to_str).unwrap_or_default();
    dirs.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d.replace("${ORIGIN}", origin).replace("$ORIGIN", origin)))
        .collect()
}

/// The run path in an ELF file's dynamic section, for 64-bit files of
/// this machine's byte order (which are what it runs).
fn elf_run_path(file: &[u8]) -> Option<String> {
    const PT_LOAD: u32 = 1;
    const PT_DYNAMIC: u32 = 2;
    const DT_NULL: u64 = 0;
    const DT_STRTAB: u64 = 5;
    const DT_RPATH: u64 = 15;
    const DT_RUNPATH: u64 = 29;
    let byte_order = if cfg!(target_endian = "little") { 1 } else { 2 };
    if file.get(..4)? != b"\x7fELF" || *file.get(4)? != 2 || *file.get(5)? != byte_order {
        return None;
    }
    let bytes = |at: u64, n: usize| file.get(usize::try_from(at).ok()?..)?.get(..n);
    let u16_at = |at| Some(u16::from_ne_bytes(bytes(at, 2)?.try_into().ok()?));
    let u32_at = |at| Some(u32::from_ne_bytes(bytes(at, 4)?.try_into().ok()?));
    let u64_at = |at| Some(u64::from_ne_bytes(bytes(at, 8)?.try_into().ok()?));
    // The program headers: file-backed segments (to find the string
    // table's place in the file) and the dynamic section.
    let (phoff, phentsize, phnum) = (u64_at(0x20)?, u64::from(u16_at(0x36)?), u64::from(u16_at(0x38)?));
    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..phnum {
        // A header's start is read before the fields after it, so a header
        // past the file's end ends the search rather than overflowing.
        let ph = phoff.checked_add(i * phentsize)?;
        let kind = u32_at(ph)?;
        let (offset, vaddr, size) = (u64_at(ph + 8)?, u64_at(ph + 16)?, u64_at(ph + 32)?);
        match kind {
            PT_LOAD => loads.push((vaddr, offset, size)),
            PT_DYNAMIC => dynamic = Some((offset, size)),
            _ => {}
        }
    }
    let (start, size) = dynamic?;
    let (mut strtab, mut runpath, mut rpath) = (None, None, None);
    for entry in (start..start.checked_add(size)?).step_by(16) {
        match (u64_at(entry)?, u64_at(entry + 8)?) {
            (DT_NULL, _) => break,
            (DT_STRTAB, value) => strtab = Some(value),
            (DT_RUNPATH, value) => runpath = Some(value),
            (DT_RPATH, value) => rpath = Some(value),
            _ => {}
        }
    }
    let strtab = strtab?;
    let &(vaddr, offset, _) = loads.iter().find(|&&(v, _, s)| v <= strtab && strtab - v < s)?;
    let at = (strtab - vaddr).checked_add(offset)?.checked_add(runpath.or(rpath)?)?;
    let tail = file.get(usize::try_from(at).ok()?..)?;
    let end = tail.iter().position(|&b| b == 0)?;
    String::from_utf8(tail[..end].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_executables_run_path() {
        let exe = std::env::current_exe().unwrap();
        let file = std::fs::read(&exe).unwrap();
        assert_eq!(elf_run_path(&file[..64]), None, "a truncated file has none");
        assert_eq!(elf_run_path(b"#!/bin/sh\n"), None);
        // This test's own: none where the C library is where everyone
        // looks; on NixOS, the C library's directory in the store.
        if let Some(dirs) = elf_run_path(&file) {
            assert!(dirs.split(':').all(|d| d.starts_with('/') || d.starts_with("$ORIGIN")), "{dirs}");
        }
    }

    #[test]
    fn origin_is_the_executables_directory() {
        let dir = std::env::temp_dir().join(format!("sidestep-runpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tool");
        std::fs::write(&exe, elf_with_run_path(b"$ORIGIN/../lib:/opt/x/lib")).unwrap();
        let dirs = run_path(&exe);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(dirs, [dir.join("../lib"), PathBuf::from("/opt/x/lib")]);
    }

    /// The smallest ELF file with a run path: a header, a loaded segment
    /// and a dynamic section whose string table holds `path`.
    fn elf_with_run_path(path: &[u8]) -> Vec<u8> {
        const DYNAMIC: u64 = 0x100;
        const STRINGS: u64 = 0x200;
        const BASE: u64 = 0x40_0000;
        let mut file = vec![0u8; 0x300];
        let mut put = |at: u64, value: &[u8]| file[at as usize..at as usize + value.len()].copy_from_slice(value);
        put(0, b"\x7fELF");
        put(4, &[2, if cfg!(target_endian = "little") { 1 } else { 2 }, 1]);
        put(0x20, &0x40u64.to_ne_bytes());
        put(0x36, &56u16.to_ne_bytes());
        put(0x38, &2u16.to_ne_bytes());
        // PT_LOAD, the whole file at BASE.
        put(0x40, &1u32.to_ne_bytes());
        put(0x48, &0u64.to_ne_bytes());
        put(0x50, &BASE.to_ne_bytes());
        put(0x60, &0x300u64.to_ne_bytes());
        // PT_DYNAMIC.
        put(0x78, &2u32.to_ne_bytes());
        put(0x80, &DYNAMIC.to_ne_bytes());
        put(0x88, &(BASE + DYNAMIC).to_ne_bytes());
        put(0x98, &48u64.to_ne_bytes());
        // DT_STRTAB, DT_RUNPATH (at 1: string tables start with a NUL),
        // DT_NULL.
        put(DYNAMIC, &5u64.to_ne_bytes());
        put(DYNAMIC + 8, &(BASE + STRINGS).to_ne_bytes());
        put(DYNAMIC + 16, &29u64.to_ne_bytes());
        put(DYNAMIC + 24, &1u64.to_ne_bytes());
        put(STRINGS + 1, path);
        file
    }
}
