//! Rules for fork commits 7 and 8: functions whose return value the fork's
//! configs describe differently from the published bindings, rewritten the
//! way the generator writes such a return.
//!
//! A function is found by its name, as the configs name it: the free
//! function of that name, and the associated function the generator also
//! emits for it on a type, which carries the name as its `doc(alias)`. Only
//! the signature's return type and the statements after the call change,
//! and each rewrite expects exactly the text the generator writes for the
//! published return, so anything else stops the tool.

use crate::generated::{File, Item, defined_name, parse_items};

/// Fork commit 7 (`fn.<name>.returns-retained = false`): the object these
/// return is autoreleased, not owned by the caller.
const NOT_RETAINED: &[(&str, &[&str])] =
    &[("objc2-app-kit", &["NSCreateFilenamePboardType", "NSCreateFileContentsPboardType"])];

/// Fork commit 8 (`fn.<name>.return.nullability = "nullable"`): these may
/// return null.
const NULLABLE: &[(&str, &[&str])] = &[("objc2-core-graphics", &["CGColorSpaceCopyBaseColorSpace"])];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Functions (free and associated) whose return is no longer retained.
    pub not_retained: usize,
    /// Functions (free and associated) whose return became optional.
    pub nullable: usize,
}

/// Applies the rules for `krate`.
pub fn apply(krate: &str, files: &mut [File]) -> Result<Stats, String> {
    let mut stats = Stats::default();
    for &(_, names) in NOT_RETAINED.iter().filter(|(k, _)| *k == krate) {
        // A closure: `not_retained` needs only a slice (clippy's `ptr_arg`
        // rejects a `&mut Vec` it doesn't grow), `nullable` the vector.
        stats.not_retained += rewrite(files, names, |body| not_retained(body))?;
    }
    for &(_, names) in NULLABLE.iter().filter(|(k, _)| *k == krate) {
        stats.nullable += rewrite(files, names, nullable)?;
    }
    Ok(stats)
}

type Rewrite = fn(&mut Vec<String>) -> Result<(), String>;

/// Rewrites each function called one of `names` (its lines from the header
/// to its end) with `f`, and returns how many there were; each name must
/// have at least one.
fn rewrite(files: &mut [File], names: &[&str], f: Rewrite) -> Result<usize, String> {
    let mut total = 0;
    for name in names {
        let mut found = 0;
        for file in files.iter_mut() {
            let items = parse_items(&file.lines).map_err(|e| format!("{}: {e}", file.path))?;
            // Last first, so that a rewrite changing the number of lines
            // leaves the extents of those before it valid.
            let targets: Vec<&Item> = items.iter().filter(|item| is_function(&file.lines, item, name)).collect();
            for item in targets.into_iter().rev() {
                let mut body = file.lines[item.header..=item.end].to_vec();
                f(&mut body).map_err(|e| format!("{}:{}: {name}: {e}", file.path, item.header + 1))?;
                file.lines.splice(item.header..=item.end, body);
                found += 1;
            }
        }
        if found == 0 {
            return Err(format!("no function {name}"));
        }
        total += found;
    }
    Ok(total)
}

/// Whether `item` is the free function `name`, or an associated function
/// the generator made from it.
fn is_function(lines: &[String], item: &Item, name: &str) -> bool {
    let header = lines[item.header].trim();
    if !header.contains("fn ") {
        return false;
    }
    let alias = format!("#[doc(alias = \"{name}\")]");
    (item.indent == 0 && defined_name(header) == Some(name))
        || lines[item.start..item.header].iter().any(|line| line.trim() == alias)
}

/// `unsafe { Retained::from_raw(ret) }` (with its `.expect(...)` if the
/// return is non-null) becomes `retain_autoreleased`.
fn not_retained(body: &mut [String]) -> Result<(), String> {
    let mut changed = 0;
    for line in body.iter_mut() {
        if line.trim_start().starts_with("unsafe { Retained::from_raw(ret") {
            *line = line.replacen("Retained::from_raw(", "Retained::retain_autoreleased(", 1);
            changed += 1;
        }
    }
    if changed != 1 {
        return Err(format!("expected one `Retained::from_raw(ret)`, found {changed}"));
    }
    Ok(())
}

/// A non-null CoreFoundation-style return (`let ret = ret.expect(...);`,
/// then `unsafe { CFRetained::from_raw(ret) }` or `::retain(ret)`) becomes
/// optional: the return type is wrapped in `Option`, the `expect` goes and
/// the conversion maps over the result.
fn nullable(body: &mut Vec<String>) -> Result<(), String> {
    // The signature's last line, `) -> CFRetained<T> {` or the whole
    // signature on one line.
    let signature = body
        .iter()
        .position(|line| line.trim_end().ends_with('{') && line.contains(") -> "))
        .ok_or("no return type")?;
    let line = &body[signature];
    let at = line.rfind(") -> ").ok_or("no return type")? + ") -> ".len();
    let ty = line[at..].trim_end().strip_suffix('{').ok_or("no return type")?.trim_end();
    if ty.starts_with("Option<") {
        return Err(format!("the return type `{ty}` is already optional"));
    }
    body[signature] = format!("{}Option<{ty}> {{", &line[..at]);

    // `let ret = ret.expect(...);`, on one line or two.
    let first = body
        .iter()
        .position(|line| line.trim_start().starts_with("let ret =") && !line.contains("unsafe {"))
        .ok_or("no `let ret = ret.expect(...)`")?;
    let last = (first..body.len()).find(|&i| body[i].trim_end().ends_with(';')).ok_or("unterminated `let`")?;
    let statement: String = body[first..=last].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    if !statement.starts_with("let ret = ret.expect(") || last > first + 1 {
        return Err(format!("expected `let ret = ret.expect(...);`, found `{statement}`"));
    }
    body.drain(first..=last);

    let conversion = body
        .iter()
        .position(|line| {
            let t = line.trim();
            t.starts_with("unsafe { ")
                && t.ends_with("(ret) }")
                && (t.contains("::from_raw(") || t.contains("::retain("))
        })
        .ok_or("no conversion of `ret`")?;
    let line = &body[conversion];
    let indent = &line[..line.len() - line.trim_start().len()];
    body[conversion] = format!("{indent}ret.map(|ret| {})", line.trim());
    Ok(())
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

    const OBJECTS: &str = r#"#[cfg(feature = "NSWidget")]
#[inline]
pub extern "C-unwind" fn NSCreateWidgetName(kind: &NSString) -> Option<Retained<NSString>> {
    extern "C-unwind" {
        fn NSCreateWidgetName(kind: &NSString) -> *mut NSString;
    }
    let ret = unsafe { NSCreateWidgetName(kind) };
    unsafe { Retained::from_raw(ret) }
}

#[inline]
pub extern "C-unwind" fn NSCreateWidgetTitle(kind: &NSString) -> Retained<NSString> {
    extern "C-unwind" {
        fn NSCreateWidgetTitle(kind: &NSString) -> *mut NSString;
    }
    let ret = unsafe { NSCreateWidgetTitle(kind) };
    unsafe { Retained::from_raw(ret) }
        .expect("function was marked as returning non-null, but actually returned NULL")
}

#[inline]
pub extern "C-unwind" fn NSCreateWidgetOther(kind: &NSString) -> Option<Retained<NSString>> {
    extern "C-unwind" {
        fn NSCreateWidgetOther(kind: &NSString) -> *mut NSString;
    }
    let ret = unsafe { NSCreateWidgetOther(kind) };
    unsafe { Retained::from_raw(ret) }
}
"#;

    #[test]
    fn not_retained_objects() {
        let mut files = [file("NSWidget.rs", OBJECTS)];
        let names = ["NSCreateWidgetName", "NSCreateWidgetTitle"];
        assert_eq!(rewrite(&mut files, &names, |body| not_retained(body)), Ok(2));
        let out = text(&files[0]);
        assert_eq!(out.matches("Retained::retain_autoreleased(ret) }").count(), 2);
        assert_eq!(out.matches("Retained::from_raw(ret) }").count(), 1, "NSCreateWidgetOther is left alone");
        assert!(out.contains("    unsafe { Retained::retain_autoreleased(ret) }\n        .expect("));
        assert_eq!(out.lines().count(), OBJECTS.lines().count());
        assert!(rewrite(&mut files, &["NSCreateWidgetMissing"], |body| not_retained(body)).is_err());
    }

    const CF: &str = r#"impl CGWidget {
    #[doc(alias = "CGWidgetCopyBase")]
    #[inline]
    pub fn copy_base(&self) -> CFRetained<CGWidget> {
        extern "C-unwind" {
            fn CGWidgetCopyBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
        }
        let ret = unsafe { CGWidgetCopyBase(self) };
        let ret =
            ret.expect("function was marked as returning non-null, but actually returned NULL");
        unsafe { CFRetained::from_raw(ret) }
    }

    #[doc(alias = "CGWidgetGetBase")]
    #[inline]
    pub fn base(&self) -> CFRetained<CGWidget> {
        extern "C-unwind" {
            fn CGWidgetGetBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
        }
        let ret = unsafe { CGWidgetGetBase(self) };
        let ret = ret.expect("function was marked as returning non-null, but actually returned NULL");
        unsafe { CFRetained::retain(ret) }
    }
}

#[deprecated = "renamed to `CGWidget::copy_base`"]
#[inline]
pub extern "C-unwind" fn CGWidgetCopyBase(
    widget: &CGWidget,
) -> CFRetained<CGWidget> {
    extern "C-unwind" {
        fn CGWidgetCopyBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
    }
    let ret = unsafe { CGWidgetCopyBase(widget) };
    let ret = ret.expect("function was marked as returning non-null, but actually returned NULL");
    unsafe { CFRetained::from_raw(ret) }
}
"#;

    const CF_FIXED: &str = r#"impl CGWidget {
    #[doc(alias = "CGWidgetCopyBase")]
    #[inline]
    pub fn copy_base(&self) -> Option<CFRetained<CGWidget>> {
        extern "C-unwind" {
            fn CGWidgetCopyBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
        }
        let ret = unsafe { CGWidgetCopyBase(self) };
        ret.map(|ret| unsafe { CFRetained::from_raw(ret) })
    }

    #[doc(alias = "CGWidgetGetBase")]
    #[inline]
    pub fn base(&self) -> CFRetained<CGWidget> {
        extern "C-unwind" {
            fn CGWidgetGetBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
        }
        let ret = unsafe { CGWidgetGetBase(self) };
        let ret = ret.expect("function was marked as returning non-null, but actually returned NULL");
        unsafe { CFRetained::retain(ret) }
    }
}

#[deprecated = "renamed to `CGWidget::copy_base`"]
#[inline]
pub extern "C-unwind" fn CGWidgetCopyBase(
    widget: &CGWidget,
) -> Option<CFRetained<CGWidget>> {
    extern "C-unwind" {
        fn CGWidgetCopyBase(widget: &CGWidget) -> Option<NonNull<CGWidget>>;
    }
    let ret = unsafe { CGWidgetCopyBase(widget) };
    ret.map(|ret| unsafe { CFRetained::from_raw(ret) })
}
"#;

    #[test]
    fn nullable_cf_returns() {
        let mut files = [file("CGWidget.rs", CF)];
        assert_eq!(rewrite(&mut files, &["CGWidgetCopyBase"], nullable), Ok(2), "the method and the function");
        assert_eq!(text(&files[0]), CF_FIXED);
        // Once optional, a second pass has nothing to do and says so.
        assert!(rewrite(&mut files, &["CGWidgetCopyBase"], nullable).is_err());
    }
}
