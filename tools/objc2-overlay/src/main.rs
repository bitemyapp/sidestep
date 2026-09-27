//! objc2-overlay: objc2's published crates with the fixes Sidestep needs.
//!
//! Downloads the exact crates Sidestep's Cargo.lock uses, checks them
//! against the pinned checksums, applies the fork's hand-written changes
//! (`patches/`) and, by rule, what regenerating with the fork's generator
//! and configs changes (`generated.rs`, `manifest.rs`), and writes the
//! result for `[patch.crates-io]`. See README.md.

mod generated;
mod manifest;
mod patch;
mod platform;
mod returns;
mod sha256;

use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const TOOL: &str = concat!("objc2-overlay ", env!("CARGO_PKG_VERSION"));

/// The tool's own sources: any change to a rule changes every stamp.
const SOURCES: &[&str] = &[
    include_str!("main.rs"),
    include_str!("generated.rs"),
    include_str!("manifest.rs"),
    include_str!("patch.rs"),
    include_str!("platform.rs"),
    include_str!("returns.rs"),
    include_str!("sha256.rs"),
];

struct Patch {
    name: &'static str,
    text: &'static str,
}

macro_rules! patch {
    ($krate:literal, $name:literal) => {
        Patch { name: $name, text: include_str!(concat!("../patches/", $krate, "/", $name)) }
    };
}

/// What the generator's rules change in a crate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Generated {
    /// Not a framework crate (objc2 itself).
    No,
    Yes,
}

struct Pinned {
    name: &'static str,
    version: &'static str,
    /// From Sidestep's Cargo.lock (the crates.io index checksum).
    sha256: &'static str,
    patches: &'static [Patch],
    generated: Generated,
    /// Exactly what the rules must change, so any drift stops the tool.
    expect: Expect,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Expect {
    gates_removed: usize,
    gates_added: usize,
    gates_changed: usize,
    darwin_items: usize,
    bridging_apple_only: usize,
    items_removed: usize,
    dependencies_moved: usize,
    returns_not_retained: usize,
    returns_nullable: usize,
}

const EXPECT_NONE: Expect = Expect {
    gates_removed: 0,
    gates_added: 0,
    gates_changed: 0,
    darwin_items: 0,
    bridging_apple_only: 0,
    items_removed: 0,
    dependencies_moved: 0,
    returns_not_retained: 0,
    returns_nullable: 0,
};

/// Every crate the fork changes, at the versions Sidestep's Cargo.lock
/// uses. Fork commits (github.com/bitemyapp/objc2, branches `sidestep` and
/// `sidestep-main`, see README.md): 1 `Retained::retain_autoreleased`
/// (#862), 2 anonymous structs and signed fields in message verification,
/// 3 GNUStep in CoreGraphics, QuartzCore, CoreText and ImageIO (rules), 4
/// `NSStringEncoding` on GNUStep, 5 NSURL path helpers on GNUStep, 6
/// `NSTextAlignment` on GNUStep, 7 `NSCreate*PboardType` returns not
/// retained (rule), 8 `CGColorSpaceCopyBaseColorSpace` nullable (rule), 9
/// the `AutoreleaseSafe` negative impls hidden from stable (`sidestep`
/// only).
const CRATES: &[Pinned] = &[
    Pinned {
        name: "objc2",
        version: "0.6.4",
        sha256: "3a12a8ed07aefc768292f076dc3ac8c48f3781c8f2d5851dd3d98950e8c5a89f",
        patches: &[
            patch!("objc2", "1-retain-autoreleased.patch"),
            patch!("objc2", "2-verify-anonymous-structs.patch"),
            patch!("objc2", "4-nsstringencoding.patch"),
            patch!("objc2", "9-negative-impls.patch"),
        ],
        generated: Generated::No,
        expect: EXPECT_NONE,
    },
    Pinned {
        name: "objc2-core-foundation",
        version: "0.3.2",
        sha256: "2a180dd8642fa45cdb7dd721cd4c11b1cadd4929ce112ebd8b9f5803cc79d536",
        patches: &[],
        generated: Generated::Yes,
        expect: Expect { gates_added: 15, darwin_items: 15, ..EXPECT_NONE },
    },
    Pinned {
        name: "objc2-foundation",
        version: "0.3.2",
        sha256: "e3e0adef53c21f888deb4fa59fc59f7eb17404926ee8a6f59f5df0fd7f9f3272",
        patches: &[
            patch!("objc2-foundation", "2-swift-string-test.patch"),
            patch!("objc2-foundation", "4-nsstringencoding.patch"),
            patch!("objc2-foundation", "5-nsurl-path-helpers.patch"),
        ],
        generated: Generated::Yes,
        expect: Expect { items_removed: 12, ..EXPECT_NONE },
    },
    Pinned {
        name: "objc2-app-kit",
        version: "0.3.2",
        sha256: "d49e936b501e5c5bf01fda3a9452ff86dc3ea98ad5f283e1455153142d97518c",
        patches: &[patch!("objc2-app-kit", "6-nstextalignment.patch")],
        generated: Generated::Yes,
        expect: Expect {
            gates_removed: 80,
            gates_added: 2,
            bridging_apple_only: 10,
            dependencies_moved: 3,
            returns_not_retained: 2,
            ..EXPECT_NONE
        },
    },
    Pinned {
        name: "objc2-core-graphics",
        version: "0.3.2",
        sha256: "e022c9d066895efa1345f8e33e584b9f958da2fd4cd116792e15e07e4720a807",
        patches: &[],
        generated: Generated::Yes,
        expect: Expect {
            gates_added: 46,
            gates_changed: 12,
            darwin_items: 46,
            dependencies_moved: 2,
            returns_nullable: 2,
            ..EXPECT_NONE
        },
    },
    Pinned {
        name: "objc2-quartz-core",
        version: "0.3.2",
        sha256: "96c1358452b371bf9f104e21ec536d37a650eb10f7ee379fff67d2e08d537f1f",
        patches: &[],
        generated: Generated::Yes,
        expect: Expect { gates_added: 19, darwin_items: 2, dependencies_moved: 2, ..EXPECT_NONE },
    },
    Pinned {
        name: "objc2-core-text",
        version: "0.3.2",
        sha256: "0cde0dfb48d25d2b4862161a4d5fcc0e3c24367869ad306b0c9ec0073bfed92d",
        patches: &[],
        generated: Generated::Yes,
        expect: EXPECT_NONE,
    },
    Pinned {
        name: "objc2-image-io",
        version: "0.3.2",
        sha256: "32b0446e98cf4a784cc7a0177715ff317eeaa8463841c616cfc78aa4f953c4ea",
        patches: &[],
        generated: Generated::Yes,
        expect: EXPECT_NONE,
    },
];

const STAMP: &str = ".objc2-overlay-stamp";

fn stamp(krate: &Pinned) -> String {
    let mut input = Vec::new();
    for part in [TOOL, krate.name, krate.version, krate.sha256].into_iter().chain(SOURCES.iter().copied()) {
        input.extend_from_slice(part.as_bytes());
        input.push(0);
    }
    for patch in krate.patches {
        input.extend_from_slice(patch.name.as_bytes());
        input.push(0);
        input.extend_from_slice(patch.text.as_bytes());
        input.push(0);
    }
    format!("{TOOL}\n{}\n", sha256::hex(&sha256::sha256(&input)))
}

fn dir_name(krate: &Pinned) -> String {
    format!("{}-{}", krate.name, krate.version)
}

fn is_fresh(out: &Path, krate: &Pinned) -> bool {
    let dir = out.join(dir_name(krate));
    fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s == stamp(krate)) && dir.join("Cargo.toml").is_file()
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|e| format!("{:?}: {e}", command.get_program()))?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed ({status})")) }
}

fn crate_file(krate: &Pinned) -> String {
    format!("{}.crate", dir_name(krate))
}

/// Where the tool keeps the `.crate` files it has checked, so rebuilding a
/// crate (after the tool or its patches change) needs no network.
fn kept_crates(out: &Path) -> PathBuf {
    out.join(".crates")
}

/// A `.crate` file's contents, if it matches the pinned checksum.
fn verified(path: &Path, krate: &Pinned) -> Option<Vec<u8>> {
    fs::read(path).ok().filter(|data| sha256::hex(&sha256::sha256(data)) == krate.sha256)
}

fn cargo_home() -> Option<PathBuf> {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
}

/// Cargo's download cache, if the crate is in it.
fn cargo_cached(krate: &Pinned) -> Option<Vec<u8>> {
    fs::read_dir(cargo_home()?.join("registry").join("cache"))
        .ok()?
        .flatten()
        .find_map(|registry| verified(&registry.path().join(crate_file(krate)), krate))
}

fn download(krate: &Pinned, work: &Path, out: &Path) -> Result<Vec<u8>, String> {
    let file = work.join(crate_file(krate));
    let url = format!("https://static.crates.io/crates/{0}/{0}-{1}.crate", krate.name, krate.version);
    let status = Command::new("curl")
        .args(["--proto", "=https", "--tlsv1.2", "-sSfL", "--retry", "3", "-o"])
        .arg(&file)
        .arg(&url)
        .status();
    let failure = match status {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("curl: {status}")),
        Err(e) => Some(format!("curl: {e}")),
    };
    if let Some(failure) = failure {
        let cargo_cache = cargo_home().map_or("$CARGO_HOME".into(), |home| home.join("registry").join("cache"));
        return Err(format!(
            "could not download {} {} from {url} ({failure}), and it is in neither {} nor Cargo's download cache \
             ({}/*/). Connect to the network, or put {} in {}.",
            krate.name,
            krate.version,
            kept_crates(out).display(),
            cargo_cache.display(),
            crate_file(krate),
            kept_crates(out).display(),
        ));
    }
    let data = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let actual = sha256::hex(&sha256::sha256(&data));
    if actual != krate.sha256 {
        return Err(format!("{url}: checksum {actual}, expected {}", krate.sha256));
    }
    Ok(data)
}

/// The crate's `.crate` file, checked against its pinned checksum: the one
/// the tool kept, else Cargo's copy, else a download, which is then kept.
fn fetch(krate: &Pinned, work: &Path, out: &Path) -> Result<PathBuf, String> {
    let kept = kept_crates(out).join(crate_file(krate));
    if verified(&kept, krate).is_some() {
        return Ok(kept);
    }
    let data = match cargo_cached(krate) {
        Some(data) => data,
        None => download(krate, work, out)?,
    };
    let file = work.join(crate_file(krate));
    fs::write(&file, data).map_err(|e| format!("{}: {e}", file.display()))?;
    // Keeping it is a convenience: a crate that can't be kept still builds.
    if fs::create_dir_all(kept_crates(out)).is_ok() && fs::rename(&file, &kept).is_ok() {
        return Ok(kept);
    }
    Ok(file)
}

fn read_generated(dir: &Path) -> Result<Vec<generated::File>, String> {
    let mut files = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            return Err(format!("{}: nested generated modules aren't supported", path.display()));
        }
        if path.extension().is_some_and(|e| e == "rs") {
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            files.push(generated::File { path: name, lines: text.lines().map(str::to_string).collect() });
        }
    }
    Ok(files)
}

/// Builds one crate in `work`, and returns its directory there.
fn build(krate: &Pinned, work: &Path, out: &Path) -> Result<(PathBuf, String), String> {
    let archive = fetch(krate, work, out)?;
    run(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(work))?;
    let dir = work.join(dir_name(krate));
    let mut report = Vec::new();

    for patch in krate.patches {
        let files = patch::apply(&dir, patch.text).map_err(|e| format!("{}: {e}", patch.name))?;
        report.push(format!("{} ({})", patch.name, files.join(", ")));
    }

    if krate.generated == Generated::Yes {
        let generated_dir = dir.join("src").join("generated");
        let mut files = read_generated(&generated_dir)?;
        let before: Vec<Vec<String>> = files.iter().map(|f| f.lines.clone()).collect();
        let g = generated::apply(krate.name, &mut files)?;
        let r = returns::apply(krate.name, &mut files)?;
        for (file, old) in files.iter().zip(before) {
            if file.lines != old {
                let mut text = file.lines.join("\n");
                text.push('\n');
                let path = generated_dir.join(&file.path);
                fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }

        let manifest_path = dir.join("Cargo.toml");
        let text = fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let (text, m) = manifest::apply(krate.name, &text)?;
        fs::write(&manifest_path, text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

        let actual = Expect {
            gates_removed: g.gates_removed,
            gates_added: g.gates_added,
            gates_changed: g.gates_changed,
            darwin_items: g.darwin_items,
            bridging_apple_only: g.bridging_apple_only,
            items_removed: g.items_removed,
            dependencies_moved: m.dependencies_moved,
            returns_not_retained: r.not_retained,
            returns_nullable: r.nullable,
        };
        if actual != krate.expect {
            return Err(format!("{}: the rules changed {actual:?}, expected {:?}", krate.name, krate.expect));
        }
        report.push(format!(
            "{} items and {} dependencies checked; gates {} removed, {} added, {} changed; {} Darwin-only; {} items removed; {} dependencies moved; returns {} not retained, {} nullable",
            g.items_checked,
            m.dependencies_checked,
            g.gates_removed,
            g.gates_added,
            g.gates_changed,
            g.darwin_items,
            g.items_removed,
            m.dependencies_moved,
            r.not_retained,
            r.nullable,
        ));
    }

    fs::write(dir.join(STAMP), stamp(krate)).map_err(|e| e.to_string())?;
    Ok((dir, report.join("; ")))
}

/// Replaces `dest` with `built` as close to atomically as directories
/// allow: a reader sees the old crate or the new one, never a mix.
fn install(built: &Path, dest: &Path, work: &Path) -> Result<(), String> {
    let old = work.join("old");
    if dest.exists() {
        fs::rename(dest, &old).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    fs::rename(built, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    if old.exists() {
        fs::remove_dir_all(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    }
    Ok(())
}

/// Whether two crate directories hold the same files, stamps aside.
fn same_files(a: &Path, b: &Path) -> bool {
    fn list(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                list(root, &path, out)?;
            } else if path.file_name().is_some_and(|n| n != STAMP) {
                out.push(path.strip_prefix(root).map_err(io::Error::other)?.to_path_buf());
            }
        }
        Ok(())
    }
    let (mut in_a, mut in_b) = (Vec::new(), Vec::new());
    if list(a, a, &mut in_a).is_err() || list(b, b, &mut in_b).is_err() {
        return false;
    }
    in_a.sort();
    in_b.sort();
    in_a == in_b
        && in_a.iter().all(|file| match (fs::read(a.join(file)), fs::read(b.join(file))) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        })
}

/// Each build works in `DIR/.work-<crate>-<pid>` and removes it when done.
const WORK_PREFIX: &str = ".work-";

fn work_dirs(out: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(out) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(WORK_PREFIX))
        .map(|entry| entry.path())
        .collect()
}

fn patch_table(out: &Path) -> String {
    let mut table = String::from("[patch.crates-io]\n");
    for krate in CRATES {
        let path = out.join(dir_name(krate));
        table.push_str(&format!("{} = {{ path = \"{}\" }}\n", krate.name, path.display()));
    }
    table
}

struct Options {
    out: PathBuf,
    out_given: bool,
    check: bool,
    quiet: bool,
}

const USAGE: &str = "\
usage: objc2-overlay [--out DIR] [--check] [--quiet]

Writes objc2's published crates, with the fixes from Sidestep's objc2 fork,
to DIR/<crate>-<version>, and prints the [patch.crates-io] table for DIR.
Crates that are already up to date are left alone.

  --out DIR   where to write the crates (default: .objc2-overlay in the
              Sidestep checkout this tool belongs to)
  --check     only check that DIR is up to date (exit status 1 if not)
  --quiet     print nothing but errors and the table
";

fn options() -> Result<Options, String> {
    let default_out = sidestep_root().join(".objc2-overlay");
    let mut options = Options { out: default_out, out_given: false, check: false, quiet: false };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                options.out = args.next().ok_or("--out needs a directory")?.into();
                options.out_given = true;
            }
            "--check" => options.check = true,
            "--quiet" | "-q" => options.quiet = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument `{arg}`\n\n{USAGE}")),
        }
    }
    Ok(options)
}

/// The Sidestep checkout this tool is part of (`tools/objc2-overlay`).
fn sidestep_root() -> &'static Path {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(Path::parent).unwrap_or(manifest_dir)
}

/// How the table spells DIR: as given with `--out`, else relative to the
/// Sidestep checkout when run from there.
fn display_out(options: &Options) -> PathBuf {
    if options.out_given {
        return options.out.clone();
    }
    let root = fs::canonicalize(sidestep_root()).ok();
    let cwd = env::current_dir().ok().and_then(|d| fs::canonicalize(d).ok());
    match (root, fs::canonicalize(&options.out)) {
        (Some(root), Ok(out)) if cwd.as_ref() == Some(&root) => {
            out.strip_prefix(&root).map(Path::to_path_buf).unwrap_or(out)
        }
        (_, Ok(out)) => out,
        _ => options.out.clone(),
    }
}

fn main_inner() -> Result<bool, String> {
    let options = options()?;
    let out = &options.out;
    let stale: Vec<&Pinned> = CRATES.iter().filter(|k| !is_fresh(out, k)).collect();

    if options.check {
        for krate in &stale {
            eprintln!("objc2-overlay: {} is missing or stale", dir_name(krate));
        }
        if stale.is_empty() {
            print!("{}", patch_table(&display_out(&options)));
        }
        return Ok(stale.is_empty());
    }

    if !stale.is_empty() || !work_dirs(out).is_empty() {
        fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
        // One writer at a time; a second run waits, then finds everything
        // up to date.
        let lock = fs::File::create(out.join(".lock")).map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| format!("locking {}: {e}", out.display()))?;
        // With the lock held no other run is writing, so any work
        // directory left is from a run that was interrupted.
        for dir in work_dirs(out) {
            let _ = fs::remove_dir_all(dir);
        }
        for krate in CRATES.iter().filter(|k| !is_fresh(out, k)) {
            let work = out.join(format!("{WORK_PREFIX}{}-{}", dir_name(krate), std::process::id()));
            fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
            let dest = out.join(dir_name(krate));
            let result = build(krate, &work, out).and_then(|(dir, report)| {
                if same_files(&dir, &dest) {
                    // Only the tool changed: keep the files, and with them
                    // their times, so Cargo rebuilds nothing.
                    fs::rename(dir.join(STAMP), dest.join(STAMP)).map_err(|e| format!("{}: {e}", dest.display()))?;
                    return Ok(format!("{report}; unchanged"));
                }
                install(&dir, &dest, &work)?;
                Ok(report)
            });
            let _ = fs::remove_dir_all(&work);
            let report = result.map_err(|e| format!("{}: {e}", dir_name(krate)))?;
            if !options.quiet {
                eprintln!("objc2-overlay: {}: {report}", dir_name(krate));
            }
        }
    } else if !options.quiet {
        eprintln!("objc2-overlay: {} crates up to date in {}", CRATES.len(), out.display());
    }
    print!("{}", patch_table(&display_out(&options)));
    io::stdout().flush().map_err(|e| e.to_string())?;
    Ok(true)
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("objc2-overlay: error: {e}");
            ExitCode::from(2)
        }
    }
}
