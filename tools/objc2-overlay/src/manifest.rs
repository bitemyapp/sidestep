//! Rules for the framework crates' `Cargo.toml`, as `cargo publish` wrote
//! it: which dependencies are platform-specific (header-translator's
//! `library.rs`, from `PlatformCfg`), docs.rs targets, and QuartzCore's
//! runtime features (fork commit 3).

use crate::platform::{self, Cfg, Model, NEWLY_GNUSTEP, canonical};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Dependency tables checked against the published configs.
    pub dependencies_checked: usize,
    /// Dependency tables that moved.
    pub dependencies_moved: usize,
    pub docs_targets_added: usize,
    pub features_forwarded: usize,
}

/// `[dependencies.NAME]` or `[target.'cfg(PREDICATE)'.dependencies.NAME]`.
fn dependency_header(line: &str) -> Option<(Option<String>, String)> {
    let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    if let Some(name) = inner.strip_prefix("dependencies.") {
        return Some((None, name.to_string()));
    }
    let rest = inner.strip_prefix("target.")?;
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = &rest[1..];
    let close = rest.find(quote)?;
    let predicate = rest[..close].strip_prefix("cfg(")?.strip_suffix(')')?;
    let name = rest[close + 1..].strip_prefix(".dependencies.")?;
    Some((Some(predicate.to_string()), name.to_string()))
}

fn dependency_cfg(krate: &str, dependency: &str, model: Model) -> Result<Option<String>, String> {
    let own = platform::library(krate, model).ok_or(format!("unknown crate {krate}"))?;
    let dep = platform::library(dependency, model)
        .ok_or(format!("{krate} depends on `{dependency}`, a library this tool doesn't know"))?;
    let mut cfg = Cfg::from_library(own);
    cfg.dependency(dep);
    Ok(cfg.cfgs())
}

/// QuartzCore's `gnustep-*` features forward to the crates it uses, as in
/// the fork's `Cargo.modified.toml`.
fn quartz_core_runtime_feature(version: &str) -> String {
    const ORDER: [&str; 5] = ["1-7", "1-8", "1-9", "2-0", "2-1"];
    let i = ORDER.iter().position(|v| *v == version).unwrap();
    let mut entries = Vec::new();
    if i > 0 {
        entries.push(format!("gnustep-{}", ORDER[i - 1]));
    }
    for dep in ["objc2", "block2?", "objc2-foundation"] {
        entries.push(format!("{dep}/gnustep-{version}"));
    }
    let body: String = entries.iter().map(|e| format!("    \"{e}\",\n")).collect();
    format!("gnustep-{version} = [\n{body}]")
}

pub fn apply(krate: &str, manifest: &str) -> Result<(String, Stats), String> {
    let mut stats = Stats::default();
    let newly_gnustep = NEWLY_GNUSTEP.contains(&krate);
    let mut table = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut in_docs_targets = false;
    let mut seen_dependencies: Vec<String> = Vec::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[\"") {
            table = trimmed.to_string();
            if let Some((published, name)) = dependency_header(line) {
                stats.dependencies_checked += 1;
                let predicted = dependency_cfg(krate, &name, Model::Published)?;
                if predicted.as_deref().map(canonical) != published.as_deref().map(canonical) {
                    return Err(format!(
                        "{krate}: `{trimmed}` doesn't match the published configs, which give {predicted:?}"
                    ));
                }
                let fork = dependency_cfg(krate, &name, Model::Fork)?;
                let header = match &fork {
                    None => format!("[dependencies.{name}]"),
                    Some(cfg) => format!("[target.'cfg({cfg})'.dependencies.{name}]"),
                };
                if fork != predicted {
                    stats.dependencies_moved += 1;
                }
                if seen_dependencies.contains(&name) {
                    return Err(format!("{krate}: `{name}` is listed twice"));
                }
                seen_dependencies.push(name);
                out.push(if fork == predicted { line.to_string() } else { header });
                continue;
            }
            out.push(line.to_string());
            continue;
        }

        if newly_gnustep && table == "[package.metadata.docs.rs]" {
            if trimmed == "targets = [" {
                in_docs_targets = true;
            } else if in_docs_targets && trimmed == "]" {
                in_docs_targets = false;
                for target in ["x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu"] {
                    if out.iter().any(|l| l.contains(&format!("\"{target}\""))) {
                        return Err(format!("{krate}: docs.rs already builds {target}"));
                    }
                    out.push(format!("    \"{target}\","));
                    stats.docs_targets_added += 1;
                }
            }
        }

        if krate == "objc2-quartz-core"
            && table == "[features]"
            && let Some(version) = trimmed.strip_prefix("gnustep-").and_then(|v| v.strip_suffix(" = []"))
        {
            out.push(quartz_core_runtime_feature(version));
            stats.features_forwarded += 1;
            continue;
        }
        out.push(line.to_string());
    }

    if newly_gnustep && stats.docs_targets_added != 2 {
        return Err(format!("{krate}: no docs.rs targets list"));
    }
    if krate == "objc2-quartz-core" && stats.features_forwarded != 5 {
        return Err(format!("{krate}: expected 5 empty gnustep features, found {}", stats.features_forwarded));
    }
    let mut text = out.join("\n");
    text.push('\n');
    Ok((text, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUARTZ_LIKE: &str = r#"[package]
name = "objc2-quartz-core"

[package.metadata.docs.rs]
targets = [
    "aarch64-apple-darwin",
]

[features]
CALayer = []
gnustep-1-7 = []
gnustep-1-8 = []
gnustep-1-9 = []
gnustep-2-0 = []
gnustep-2-1 = []

[dependencies.objc2]
version = "0.6.2"

[dependencies.objc2-metal]
version = "0.3.2"
optional = true

[target.'cfg(target_os = "macos")'.dependencies.objc2-open-gl]
version = "0.3.2"
"#;

    #[test]
    fn quartz_core() {
        let (out, stats) = apply("objc2-quartz-core", QUARTZ_LIKE).unwrap();
        assert_eq!(
            stats,
            Stats { dependencies_checked: 3, dependencies_moved: 1, docs_targets_added: 2, features_forwarded: 5 }
        );
        assert!(out.contains("[target.'cfg(target_vendor = \"apple\")'.dependencies.objc2-metal]\n"));
        assert!(out.contains("[dependencies.objc2]\n"));
        assert!(out.contains("[target.'cfg(target_os = \"macos\")'.dependencies.objc2-open-gl]\n"));
        assert!(out.contains(
            "    \"aarch64-apple-darwin\",\n    \"x86_64-unknown-linux-gnu\",\n    \"i686-unknown-linux-gnu\",\n]"
        ));
        assert!(out.contains(
            "gnustep-1-8 = [\n    \"gnustep-1-7\",\n    \"objc2/gnustep-1-8\",\n    \"block2?/gnustep-1-8\",\n    \"objc2-foundation/gnustep-1-8\",\n]\n"
        ));
        assert!(out.contains("gnustep-1-7 = [\n    \"objc2/gnustep-1-7\","));
    }

    #[test]
    fn app_kit() {
        let manifest = "[package]\nname = \"objc2-app-kit\"\n\n[dependencies.objc2-foundation]\nversion = \"0.3.2\"\n\n[target.'cfg(target_vendor = \"apple\")'.dependencies.objc2-core-graphics]\nversion = \"0.3.2\"\n\n[target.'cfg(target_vendor = \"apple\")'.dependencies.objc2-core-image]\nversion = \"0.3.2\"\n";
        let (out, stats) = apply("objc2-app-kit", manifest).unwrap();
        assert_eq!(stats.dependencies_moved, 1);
        assert!(out.contains("[dependencies.objc2-core-graphics]\n"));
        assert!(out.contains("[target.'cfg(target_vendor = \"apple\")'.dependencies.objc2-core-image]\n"));
    }

    #[test]
    fn mismatch_is_an_error() {
        let manifest = "[package]\n\n[dependencies.objc2-core-image]\nversion = \"0.3.2\"\n";
        assert!(apply("objc2-app-kit", manifest).is_err());
    }
}
