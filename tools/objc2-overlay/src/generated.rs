//! Rules for `src/generated/**`: what regenerating the bindings with the
//! fork's configs and generator changes, applied to the published text.
//!
//! The rules key on the text's structure only: `#[cfg(...)]` attribute
//! lines, the crate features they name, item headers and extents, and the
//! names of `libc` types. Nothing here is copied from the generated files.
//!
//! Every item's platform `cfg` is derived twice with the generator's own
//! logic (`platform::Cfg`): once from the published configs, which must
//! reproduce the published `cfg` exactly (otherwise the tool stops, since
//! its model of the generator would be wrong), and once from the fork's,
//! which is what gets written.

use std::collections::{HashMap, HashSet};

use crate::platform::{self, Cfg, LIBC_DARWIN, Model, NEWLY_GNUSTEP, Platforms, canonical};

/// A generated source file, as lines without their newlines.
pub struct File {
    /// Relative to `src/generated`.
    pub path: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Items whose platform `cfg` was checked against the published configs.
    pub items_checked: usize,
    pub gates_removed: usize,
    pub gates_added: usize,
    pub gates_changed: usize,
    /// Items using Darwin-only `libc` types (or items depending on them).
    pub darwin_items: usize,
    /// Toll-free bridging impls that stay Apple-only.
    pub bridging_apple_only: usize,
    pub link_lines: usize,
    pub items_removed: usize,
}

#[derive(Debug, PartialEq)]
enum AttrKind {
    Features(Vec<String>),
    Platform(String),
    Other,
}

#[derive(Debug)]
pub struct Item {
    /// First attribute or doc comment line.
    pub start: usize,
    /// The line the item itself starts on.
    pub header: usize,
    /// Its last line.
    pub end: usize,
    pub indent: usize,
    features: Vec<String>,
    /// Last line of the (last) feature `cfg`.
    feature_attr_end: Option<usize>,
    /// Lines and predicate of the platform `cfg`.
    platform: Option<(usize, usize, String)>,
    parent: Option<usize>,
    children: Vec<usize>,
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_attr_start(trimmed: &str) -> bool {
    trimmed.starts_with("#[")
}

fn is_doc(trimmed: &str) -> bool {
    trimmed.starts_with("///") || (trimmed.starts_with("//") && !trimmed.starts_with("//!"))
}

/// The last line of the attribute starting on `start` (brackets balanced,
/// string literals skipped).
fn attr_end(lines: &[String], start: usize) -> Result<usize, String> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, line) in lines.iter().enumerate().skip(start) {
        for c in line.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth = depth.checked_sub(1).ok_or(format!("unbalanced attribute on line {}", start + 1))?;
                    if depth == 0 {
                        return Ok(i);
                    }
                }
                _ => {}
            }
        }
    }
    Err(format!("unterminated attribute on line {}", start + 1))
}

fn classify(text: &str) -> Result<AttrKind, String> {
    let canon = canonical(text);
    let Some(predicate) = canon.strip_prefix("#[cfg(").and_then(|p| p.strip_suffix(")]")) else {
        return Ok(AttrKind::Other);
    };
    if !predicate.contains("feature=") {
        return Ok(AttrKind::Platform(predicate.to_string()));
    }
    // Feature gates are always `feature = "a"` or `all(feature = "a", ...)`.
    let list = predicate.strip_prefix("all(").and_then(|p| p.strip_suffix(')')).unwrap_or(predicate);
    let mut features = Vec::new();
    for part in list.split(',') {
        let name = part
            .strip_prefix("feature=\"")
            .and_then(|p| p.strip_suffix('"'))
            .ok_or_else(|| format!("unexpected cfg `{}`", text.trim()))?;
        features.push(name.to_string());
    }
    Ok(AttrKind::Features(features))
}

/// The last line of the item whose first line is `header`, from the
/// indentation of the (rustfmt-formatted) generated code.
fn item_end(lines: &[String], header: usize, indent: usize) -> usize {
    let first = lines[header].trim_end();
    if first.ends_with(';') || first.ends_with(',') || first.ends_with('}') {
        return header;
    }
    let mut last = header;
    for (k, line) in lines.iter().enumerate().skip(header + 1) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let ind = indent_of(line);
        if ind > indent {
            last = k;
            continue;
        }
        if ind < indent {
            return last;
        }
        if t.starts_with('}') || t.starts_with(')') || t.starts_with(']') {
            if t.ends_with('{') || t.ends_with('(') || t.ends_with('[') {
                // `) -> Foo {`, `} else {`
                last = k;
                continue;
            }
            return k;
        }
        if t == "where" || t == "{" {
            last = k;
            continue;
        }
        // The next item at the same level.
        return last;
    }
    last
}

pub fn parse_items(lines: &[String]) -> Result<Vec<Item>, String> {
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if !(is_attr_start(t) || t.starts_with("///")) {
            i += 1;
            continue;
        }
        let indent = indent_of(&lines[i]);
        let start = i;
        let mut features = Vec::new();
        let mut feature_attr_end = None;
        let mut platform = None;
        let mut j = i;
        while j < lines.len() && indent_of(&lines[j]) == indent {
            let tj = lines[j].trim_start();
            if is_doc(tj) {
                j += 1;
            } else if is_attr_start(tj) {
                let end = attr_end(lines, j)?;
                match classify(&lines[j..=end].join("\n")).map_err(|e| format!("line {}: {e}", j + 1))? {
                    AttrKind::Features(names) => {
                        features.extend(names);
                        feature_attr_end = Some(end);
                    }
                    AttrKind::Platform(predicate) => {
                        if platform.is_some() {
                            return Err(format!("line {}: two platform cfgs on one item", j + 1));
                        }
                        platform = Some((j, end, predicate));
                    }
                    AttrKind::Other => {}
                }
                j = end + 1;
            } else {
                break;
            }
        }
        let header = j;
        let header_ok = header < lines.len()
            && indent_of(&lines[header]) == indent
            && !lines[header].trim().is_empty()
            && !lines[header].trim_start().starts_with(['}', ')', ']']);
        if !header_ok {
            if features.is_empty() && platform.is_none() {
                // Stray docs or attributes without an item; nothing to gate.
                i = j.max(i + 1);
                continue;
            }
            return Err(format!("line {}: gated attributes without an item", start + 1));
        }
        let end = item_end(lines, header, indent);
        items.push(Item {
            start,
            header,
            end,
            indent,
            features,
            feature_attr_end,
            platform,
            parent: None,
            children: Vec::new(),
        });
        // Continue inside the item: it may contain gated items itself.
        i = header + 1;
    }

    // Nesting, from the extents.
    let mut stack: Vec<usize> = Vec::new();
    for k in 0..items.len() {
        while let Some(&top) = stack.last() {
            if items[top].end < items[k].start {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some(&top) = stack.last()
            && items[top].indent < items[k].indent
            && items[top].header < items[k].start
        {
            items[k].parent = Some(top);
            items[top].children.push(k);
        }
        if items[k].end > items[k].header {
            stack.push(k);
        }
    }
    Ok(items)
}

/// The item's own lines: its header and body, without its attributes and
/// without the items nested in it.
fn own_text(lines: &[String], items: &[Item], k: usize) -> String {
    let item = &items[k];
    let mut skip: Vec<(usize, usize)> = item.children.iter().map(|&c| (items[c].start, items[c].end)).collect();
    skip.sort();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(item.end + 1).skip(item.header) {
        if skip.iter().any(|&(s, e)| s <= i && i <= e) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|s| !s.is_empty())
}

/// The types `libc` only defines on Apple targets, which fork commit 3 maps
/// to the Apple-only `__libc_darwin__` library (`malloc_zone_t` only on
/// `sidestep-main`: the tag's generator doesn't map `Darwin.malloc`).
const DARWIN_LIBC_TYPES: &[&str] = &["mach_port_t", "cpu_type_t", "boolean_t", "malloc_zone_t"];

fn uses_darwin_libc(text: &str) -> bool {
    text.match_indices("libc::").any(|(at, _)| {
        let rest = &text[at + "libc::".len()..];
        let name = rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or("");
        DARWIN_LIBC_TYPES.contains(&name)
    })
}

/// The name a top-level item defines, if any (`pub struct Foo`,
/// `pub unsafe extern "C-unwind" fn Foo(`, ...).
pub fn defined_name(header: &str) -> Option<&str> {
    let mut words = header.split_whitespace();
    while let Some(word) = words.next() {
        if matches!(word, "struct" | "type" | "fn" | "static" | "const" | "enum" | "union") {
            let name = words.next()?;
            let end = name.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(name.len());
            return Some(&name[..end]).filter(|n| !n.is_empty() && *n != "_");
        }
        if matches!(word, "impl" | "use" | "mod") || word.ends_with('!') || word.ends_with("!(") {
            return None;
        }
    }
    None
}

/// The library behind a feature, or `None` for the crate's own module
/// features.
fn feature_library(feature: &str, model: Model) -> Result<Option<Platforms>, String> {
    if let Some(lib) = platform::library(feature, model) {
        return Ok(Some(lib));
    }
    if feature.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!("feature `{feature}` names a library this tool doesn't know"));
    }
    Ok(None)
}

struct Evaluation {
    published: Option<String>,
    fork: Option<String>,
}

fn evaluate(
    krate: &str,
    items: &[Item],
    k: usize,
    darwin: &HashSet<usize>,
    bridging: bool,
) -> Result<Evaluation, String> {
    let mut out = [None, None];
    for (slot, model) in out.iter_mut().zip([Model::Published, Model::Fork]) {
        let own = platform::library(krate, model).ok_or(format!("unknown crate {krate}"))?;
        let mut cfg = Cfg::from_library(own);
        let item = &items[k];
        let mut required_non_cf = false;
        for feature in &item.features {
            if let Some(lib) = feature_library(feature, model)? {
                cfg.dependency(lib);
                required_non_cf |= feature != "objc2-core-foundation";
            }
        }
        if model == Model::Fork && darwin.contains(&k) {
            cfg.dependency(LIBC_DARWIN);
        }
        // Toll-free bridging is Apple-only unless the bridged type is in
        // CoreFoundation (fork commit 3, on `sidestep-main`).
        if model == Model::Fork && bridging && required_non_cf {
            cfg.apple_only();
        }
        let mut parent = item.parent;
        while let Some(p) = parent {
            for feature in &items[p].features {
                if let Some(lib) = feature_library(feature, model)? {
                    cfg.implied(lib);
                }
            }
            if model == Model::Fork && darwin.contains(&p) {
                cfg.implied(LIBC_DARWIN);
            }
            parent = items[p].parent;
        }
        cfg.implied(own);
        *slot = cfg.cfgs();
    }
    let [published, fork] = out;
    Ok(Evaluation { published, fork })
}

fn is_bridging_impl(header: &str) -> bool {
    header.trim_start().starts_with("impl AsRef<")
}

/// Fork commit 4: the generator no longer emits `NSStringEncoding` and the
/// encodings that don't fit in GNUStep's `int`; objc2-foundation defines
/// them by hand.
fn remove_string_encoding_items(files: &mut [File], stats: &mut Stats) -> Result<(), String> {
    fn literal(value: &str) -> Option<u64> {
        let value = value.trim().trim_end_matches(';').trim();
        match value.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16).ok(),
            None => value.replace('_', "").parse().ok(),
        }
    }

    /// Removes the top-level items `is_target` names (with their attributes
    /// and docs), and returns their names.
    fn remove_where(file: &mut File, is_target: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, String> {
        let items = parse_items(&file.lines)?;
        let mut ranges = Vec::new();
        let mut names = Vec::new();
        for (i, line) in file.lines.iter().enumerate() {
            if indent_of(line) != 0 {
                continue;
            }
            if let Some(name) = is_target(line.trim()) {
                let range = items.iter().find(|item| item.header == i).map_or((i, i), |item| (item.start, item.end));
                ranges.push(range);
                names.push(name);
            }
        }
        for (start, end) in ranges.into_iter().rev() {
            file.lines.drain(start..=end);
            // Don't leave two blank lines (or a leading one) behind.
            let blank = |i: usize| file.lines.get(i).is_some_and(|l| l.trim().is_empty());
            if blank(start) && (start == 0 || blank(start - 1)) {
                file.lines.remove(start);
            }
        }
        Ok(names)
    }

    let string = files.iter_mut().find(|f| f.path == "NSString.rs").ok_or("objc2-foundation has no NSString.rs")?;
    let names = remove_where(string, &|line| {
        if line.starts_with("pub type NSStringEncoding = ") {
            return Some("NSStringEncoding".to_string());
        }
        let rest = line.strip_prefix("pub const ")?;
        let (name, rest) = rest.split_once(": NSStringEncoding = ")?;
        (literal(rest)? > i32::MAX as u64).then(|| name.to_string())
    })?;
    let module = files.iter_mut().find(|f| f.path == "mod.rs").ok_or("objc2-foundation has no mod.rs")?;
    let reexports = remove_where(module, &|line| {
        let name = line.strip_prefix("pub use self::__NSString::")?.strip_suffix(';')?;
        names.iter().any(|n| n == name).then(|| name.to_string())
    })?;
    let (in_string, in_module) = (names.len(), reexports.len());
    // The typedef and exactly the five encodings above `i32::MAX`, and their
    // re-exports.
    if in_string != 6 || in_module != 6 {
        return Err(format!(
            "expected to remove NSStringEncoding and 5 constants (and 6 re-exports), found {in_string} and {in_module}"
        ));
    }
    stats.items_removed += in_string + in_module;
    Ok(())
}

/// Fork commit 3: crates that support GNUStep only link their framework on
/// Apple platforms.
fn gate_link_line(krate: &str, files: &mut [File], stats: &mut Stats) -> Result<(), String> {
    let module = files.iter_mut().find(|f| f.path == "mod.rs").ok_or(format!("{krate} has no mod.rs"))?;
    for line in &mut module.lines {
        let t = line.trim();
        if let Some(args) = t.strip_prefix("#[link(").and_then(|a| a.strip_suffix(")]"))
            && canonical(args).ends_with(",kind=\"framework\"")
        {
            let indent = " ".repeat(indent_of(line));
            *line = format!("{indent}#[cfg_attr(target_vendor = \"apple\", link({args}))]");
            stats.link_lines += 1;
        }
    }
    if stats.link_lines != 1 {
        return Err(format!("{krate}: expected one framework link line, found {}", stats.link_lines));
    }
    Ok(())
}

/// Applies every rule to the generated files of `krate`.
pub fn apply(krate: &str, files: &mut [File]) -> Result<Stats, String> {
    let mut stats = Stats::default();

    if krate == "objc2-foundation" {
        remove_string_encoding_items(files, &mut stats)?;
    }
    if NEWLY_GNUSTEP.contains(&krate) {
        gate_link_line(krate, files, &mut stats)?;
    }

    let parsed: Vec<Vec<Item>> = files
        .iter()
        .map(|f| parse_items(&f.lines).map_err(|e| format!("{}: {e}", f.path)))
        .collect::<Result<_, _>>()?;

    // Items that need the Darwin-only part of `libc`: those using its types
    // directly, then (to a fixed point) those naming a top-level item that
    // does, such as a struct with a Mach port field, or a re-export.
    let own: Vec<Vec<String>> = files
        .iter()
        .zip(&parsed)
        .map(|(f, items)| (0..items.len()).map(|k| own_text(&f.lines, items, k)).collect())
        .collect();
    let mut darwin: Vec<HashSet<usize>> = vec![HashSet::new(); files.len()];
    let mut names: HashSet<String> = HashSet::new();
    loop {
        let mut changed = false;
        for (fi, items) in parsed.iter().enumerate() {
            for (k, item) in items.iter().enumerate() {
                if darwin[fi].contains(&k) {
                    continue;
                }
                let text = &own[fi][k];
                if uses_darwin_libc(text) || identifiers(text).any(|id| names.contains(id)) {
                    darwin[fi].insert(k);
                    if item.indent == 0
                        && let Some(name) = defined_name(&files[fi].lines[item.header])
                    {
                        names.insert(name.to_string());
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut mismatches = Vec::new();
    for ((file, items), darwin) in files.iter_mut().zip(&parsed).zip(&darwin) {
        let mut remove: HashSet<usize> = HashSet::new();
        let mut insert_after: HashMap<usize, String> = HashMap::new();
        for (k, item) in items.iter().enumerate() {
            let bridging = is_bridging_impl(&file.lines[item.header]);
            let eval = evaluate(krate, items, k, darwin, bridging).map_err(|e| format!("{}: {e}", file.path))?;
            stats.items_checked += 1;
            let published = item.platform.as_ref().map(|(_, _, p)| p.clone());
            if eval.published.as_deref().map(canonical) != published {
                mismatches.push(format!(
                    "{}:{}: published cfg {:?}, but the published configs give {:?}",
                    file.path,
                    item.header + 1,
                    published,
                    eval.published
                ));
                continue;
            }
            if darwin.contains(&k) {
                stats.darwin_items += 1;
            }
            if eval.fork == eval.published {
                if bridging && eval.fork.is_some() && item.features.iter().any(|f| f != "objc2-core-foundation") {
                    stats.bridging_apple_only += 1;
                }
                continue;
            }
            let indent = " ".repeat(item.indent);
            if let Some((start, end, _)) = &item.platform {
                remove.extend(*start..=*end);
            }
            match (&eval.published, &eval.fork) {
                (Some(_), None) => stats.gates_removed += 1,
                (None, Some(_)) => stats.gates_added += 1,
                _ => stats.gates_changed += 1,
            }
            if let Some(fork) = &eval.fork {
                let anchor = match &item.platform {
                    Some((start, _, _)) => start.checked_sub(1).ok_or(format!("{}: cfg on line 1", file.path))?,
                    None => item.feature_attr_end.ok_or_else(|| {
                        format!("{}:{}: needs a platform cfg but has no feature cfg", file.path, item.header + 1)
                    })?,
                };
                if bridging {
                    stats.bridging_apple_only += 1;
                }
                insert_after.insert(anchor, format!("{indent}#[cfg({fork})]"));
            }
        }
        if remove.is_empty() && insert_after.is_empty() {
            continue;
        }
        let mut lines = Vec::with_capacity(file.lines.len() + insert_after.len());
        for (i, line) in file.lines.drain(..).enumerate() {
            if !remove.contains(&i) {
                lines.push(line);
            }
            if let Some(new) = insert_after.remove(&i) {
                lines.push(new);
            }
        }
        file.lines = lines;
    }
    if !mismatches.is_empty() {
        let shown: Vec<&String> = mismatches.iter().take(20).collect();
        return Err(format!(
            "{krate}: {} items don't match the model of the generator:\n{}",
            mismatches.len(),
            shown.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> File {
        File { path: path.to_string(), lines: text.lines().map(str::to_string).collect() }
    }

    fn text(file: &File) -> String {
        file.lines.join("\n") + "\n"
    }

    // Synthetic inputs in the generator's layout; the names are made up.

    const APPKIT_FILE: &str = r#"use objc2::__framework_prelude::*;
#[cfg(feature = "objc2-core-graphics")]
#[cfg(target_vendor = "apple")]
use objc2_core_graphics::*;
#[cfg(feature = "objc2-core-text")]
#[cfg(target_vendor = "apple")]
use objc2_core_text::*;

#[cfg(feature = "objc2-core-text")]
#[cfg(target_vendor = "apple")]
impl AsRef<NSWidget> for CTWidget {
    #[inline]
    fn as_ref(&self) -> &NSWidget {
        unsafe { &*((self as *const Self).cast()) }
    }
}

#[cfg(feature = "objc2-quartz-core")]
#[cfg(target_vendor = "apple")]
impl NSWidgetLayer {
    extern_methods!(
        #[cfg(feature = "NSView")]
        #[unsafe(method(view))]
        pub fn view(&self) -> Option<Retained<NSView>>;

        #[cfg(all(
            feature = "NSOpenGL",
            feature = "objc2-core-video"
        ))]
        #[unsafe(method(drawAtTime:))]
        pub unsafe fn drawAtTime(
            &self,
            ts: NonNull<CVTimeStamp>,
        );
    );
}

impl NSWidget {
    extern_methods!(
        #[cfg(feature = "objc2-core-graphics")]
        #[cfg(target_vendor = "apple")]
        /// Docs.
        #[unsafe(method(widgetColor))]
        pub fn widgetColor(&self) -> Retained<CGColor>;

        #[cfg(feature = "objc2-core-image")]
        #[cfg(target_vendor = "apple")]
        #[unsafe(method(filter))]
        pub fn filter(&self) -> Retained<CIFilter>;
    );
}
"#;

    const APPKIT_FIXED: &str = r#"use objc2::__framework_prelude::*;
#[cfg(feature = "objc2-core-graphics")]
use objc2_core_graphics::*;
#[cfg(feature = "objc2-core-text")]
use objc2_core_text::*;

#[cfg(feature = "objc2-core-text")]
#[cfg(target_vendor = "apple")]
impl AsRef<NSWidget> for CTWidget {
    #[inline]
    fn as_ref(&self) -> &NSWidget {
        unsafe { &*((self as *const Self).cast()) }
    }
}

#[cfg(feature = "objc2-quartz-core")]
impl NSWidgetLayer {
    extern_methods!(
        #[cfg(feature = "NSView")]
        #[unsafe(method(view))]
        pub fn view(&self) -> Option<Retained<NSView>>;

        #[cfg(all(
            feature = "NSOpenGL",
            feature = "objc2-core-video"
        ))]
        #[cfg(target_vendor = "apple")]
        #[unsafe(method(drawAtTime:))]
        pub unsafe fn drawAtTime(
            &self,
            ts: NonNull<CVTimeStamp>,
        );
    );
}

impl NSWidget {
    extern_methods!(
        #[cfg(feature = "objc2-core-graphics")]
        /// Docs.
        #[unsafe(method(widgetColor))]
        pub fn widgetColor(&self) -> Retained<CGColor>;

        #[cfg(feature = "objc2-core-image")]
        #[cfg(target_vendor = "apple")]
        #[unsafe(method(filter))]
        pub fn filter(&self) -> Retained<CIFilter>;
    );
}
"#;

    #[test]
    fn appkit_gates() {
        let mut files = [file("NSWidget.rs", APPKIT_FILE)];
        let stats = apply("objc2-app-kit", &mut files).unwrap();
        assert_eq!(text(&files[0]), APPKIT_FIXED);
        assert_eq!(stats.gates_removed, 4);
        assert_eq!(stats.gates_added, 1);
        assert_eq!(stats.bridging_apple_only, 1);
    }

    #[test]
    fn mismatch_is_an_error() {
        // CoreImage is Apple-only, so a missing gate means the model is off.
        let wrong = APPKIT_FILE.replacen(
            "        #[cfg(feature = \"objc2-core-image\")]\n        #[cfg(target_vendor = \"apple\")]\n",
            "        #[cfg(feature = \"objc2-core-image\")]\n",
            1,
        );
        let mut files = [file("NSWidget.rs", &wrong)];
        let err = apply("objc2-app-kit", &mut files).unwrap_err();
        assert!(err.contains("NSWidget.rs:48"), "{err}");
    }

    const FOUNDATION_LIKE: &str = r#"#[repr(C)]
pub struct CFWidgetContext {
    pub version: CFIndex,
}

/// Docs.
#[cfg(feature = "libc")]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CFWidgetContext1 {
    pub version: CFIndex,
    pub getPort: Option<unsafe extern "C-unwind" fn(*mut c_void) -> libc::mach_port_t>,
}

#[cfg(all(feature = "libc", feature = "objc2"))]
unsafe impl Encode for CFWidgetContext1 {
    const ENCODING: Encoding = Encoding::Struct("?", &[<CFIndex>::ENCODING]);
}

impl CFWidget {
    #[doc(alias = "CFWidgetGetPort")]
    #[cfg(feature = "libc")]
    #[inline]
    pub fn port(&self) -> libc::mach_port_t {
        extern "C-unwind" {
            fn CFWidgetGetPort(port: &CFWidget) -> libc::mach_port_t;
        }
        unsafe { CFWidgetGetPort(self) }
    }

    #[cfg(feature = "libc")]
    #[inline]
    pub fn owner(&self) -> libc::uid_t {
        0
    }
}
"#;

    const FOUNDATION_LIKE_MOD: &str = r#"#[cfg(all(feature = "CFWidget", feature = "libc"))]
pub use self::__CFWidget::CFWidgetContext1;
#[cfg(feature = "CFWidget")]
pub use self::__CFWidget::CFWidgetContext;
"#;

    #[test]
    fn darwin_libc() {
        let mut files = [file("CFWidget.rs", FOUNDATION_LIKE), file("mod.rs", FOUNDATION_LIKE_MOD)];
        let stats = apply("objc2-core-foundation", &mut files).unwrap();
        assert_eq!(stats.darwin_items, 4, "struct, its Encode impl, the method, the re-export");
        assert_eq!(stats.gates_added, 4);
        let out = text(&files[0]);
        assert!(out.contains("#[cfg(feature = \"libc\")]\n#[cfg(target_vendor = \"apple\")]\n#[repr(C)]"));
        assert!(out.contains("#[cfg(all(feature = \"libc\", feature = \"objc2\"))]\n#[cfg(target_vendor = \"apple\")]\nunsafe impl Encode"));
        assert!(out.contains(
            "    #[cfg(feature = \"libc\")]\n    #[cfg(target_vendor = \"apple\")]\n    #[inline]\n    pub fn port"
        ));
        assert!(out.contains("    #[cfg(feature = \"libc\")]\n    #[inline]\n    pub fn owner"));
        assert_eq!(
            text(&files[1]),
            "#[cfg(all(feature = \"CFWidget\", feature = \"libc\"))]\n#[cfg(target_vendor = \"apple\")]\npub use self::__CFWidget::CFWidgetContext1;\n#[cfg(feature = \"CFWidget\")]\npub use self::__CFWidget::CFWidgetContext;\n"
        );
    }

    #[test]
    fn link_lines_and_metal() {
        let module = "#[link(name = \"CoreWidgets\", kind = \"framework\")]\nextern \"C\" {}\n\n#[cfg(feature = \"objc2-metal\")]\n#[cfg(not(target_os = \"watchos\"))]\npub use self::__CGWidgetMetal::CGWidgetDevice;\n";
        let mut files = [file("mod.rs", module)];
        let stats = apply("objc2-core-graphics", &mut files).unwrap();
        assert_eq!(stats.link_lines, 1);
        assert_eq!(stats.gates_changed, 1);
        assert_eq!(
            text(&files[0]),
            "#[cfg_attr(target_vendor = \"apple\", link(name = \"CoreWidgets\", kind = \"framework\"))]\nextern \"C\" {}\n\n#[cfg(feature = \"objc2-metal\")]\n#[cfg(any(target_os = \"macos\", target_os = \"ios\", target_os = \"tvos\", target_os = \"visionos\"))]\npub use self::__CGWidgetMetal::CGWidgetDevice;\n"
        );
    }

    #[test]
    fn string_encoding() {
        let string = "/// Docs.\npub type NSStringEncoding = NSUInteger;\n\n/// Docs.\npub const NSSmallEncoding: NSStringEncoding = 4;\n\n/// Docs.\npub const NSBigEncoding: NSStringEncoding = 0x90000100;\n\n/// Docs.\npub const NSB2: NSStringEncoding = 0x94000100;\n\npub const NSB3: NSStringEncoding = 0x8c000100;\n/// Docs.\npub const NSB4: NSStringEncoding = 0x98000100;\n/// Docs.\npub const NSB5: NSStringEncoding = 0x9c000100;\n\nextern_class!(\n    pub struct NSString;\n);\n";
        let module = "#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSBigEncoding;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSSmallEncoding;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSStringEncoding;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSB2;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSB3;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSB4;\n#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSB5;\n";
        let mut files = [file("NSString.rs", string), file("mod.rs", module)];
        let stats = apply("objc2-foundation", &mut files).unwrap();
        assert_eq!(stats.items_removed, 12);
        assert_eq!(
            text(&files[0]),
            "/// Docs.\npub const NSSmallEncoding: NSStringEncoding = 4;\n\nextern_class!(\n    pub struct NSString;\n);\n"
        );
        assert_eq!(text(&files[1]), "#[cfg(feature = \"NSString\")]\npub use self::__NSString::NSSmallEncoding;\n");
    }

    #[test]
    fn extents() {
        let lines: Vec<String> = APPKIT_FILE.lines().map(str::to_string).collect();
        let items = parse_items(&lines).unwrap();
        let headers: Vec<&str> = items.iter().map(|i| lines[i.header].trim()).collect();
        assert_eq!(headers[2], "impl AsRef<NSWidget> for CTWidget {");
        assert_eq!(lines[items[2].end], "}");
        // `drawAtTime` spans several lines and sits inside the impl.
        let draw = items.iter().position(|i| lines[i.header].contains("drawAtTime(")).unwrap();
        assert_eq!(lines[items[draw].end].trim(), ");");
        let layer = items.iter().position(|i| lines[i.header] == "impl NSWidgetLayer {").unwrap();
        assert_eq!(items[draw].parent, Some(layer));
        assert_eq!(lines[items[layer].end], "}");
        assert_eq!(defined_name("pub unsafe extern \"C-unwind\" fn CFWidgetGetPort("), Some("CFWidgetGetPort"));
        assert_eq!(defined_name("pub struct CFWidgetContext1 {"), Some("CFWidgetContext1"));
        assert_eq!(defined_name("unsafe impl Encode for CFWidgetContext1 {"), None);
    }
}
