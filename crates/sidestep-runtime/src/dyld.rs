//! dyld's image list (`_dyld_image_count`, `_dyld_get_image_name`): what
//! programs ask to learn which libraries are loaded, answered from the
//! dynamic linker's list (`dl_iterate_phdr`).
//!
//! The main executable comes first, by its path, as on macOS. The names are
//! kept for the program's life: dyld's stay valid as long as their image
//! is loaded, and nothing here unloads one.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;

/// Every image name handed out, so the pointers stay valid.
static NAMES: Mutex<Vec<CString>> = Mutex::new(Vec::new());

/// The loaded images' paths, the executable's first.
fn images() -> Vec<CString> {
    unsafe extern "C" fn each(info: *mut libc::dl_phdr_info, _: usize, out: *mut c_void) -> c_int {
        // SAFETY: the dynamic linker passes a valid entry, and `out` is the
        // vector below, for the duration of the call.
        let (info, out) = unsafe { (&*info, &mut *out.cast::<Vec<CString>>()) };
        if !info.dlpi_name.is_null() {
            // SAFETY: a NUL-terminated name the dynamic linker owns.
            let name = unsafe { CStr::from_ptr(info.dlpi_name) };
            // The executable's entry has no name, and the vDSO a bare one.
            if name.to_bytes().starts_with(b"/") {
                out.push(name.to_owned());
            }
        }
        0
    }
    let mut found: Vec<CString> = Vec::new();
    if let Some(exe) =
        std::env::current_exe().ok().and_then(|p| CString::new(p.into_os_string().into_encoded_bytes()).ok())
    {
        found.push(exe);
    }
    // SAFETY: `each` only reads its entries and writes `found`.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut found).cast()) };
    found
}

/// How many images are loaded.
#[unsafe(no_mangle)]
pub extern "C" fn _dyld_image_count() -> u32 {
    images().len() as u32
}

/// The path of image `index` (0 is the executable), or null past the end.
#[unsafe(no_mangle)]
pub extern "C" fn _dyld_get_image_name(index: u32) -> *const c_char {
    let Some(name) = images().into_iter().nth(index as usize) else { return std::ptr::null() };
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(kept) = names.iter().find(|n| **n == name) {
        return kept.as_ptr();
    }
    names.push(name);
    names.last().map_or(std::ptr::null(), |n| n.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_comes_first_and_libraries_follow() {
        let count = _dyld_image_count();
        assert!(count >= 2, "the executable and at least the C library");
        // SAFETY: names stay valid for the program's life.
        let name = |i| unsafe { CStr::from_ptr(_dyld_get_image_name(i)) }.to_string_lossy().into_owned();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(name(0), exe.to_string_lossy());
        assert!((1..count).any(|i| name(i).contains("libc.so")), "the C library is listed");
        assert!(_dyld_get_image_name(count + 10).is_null());
        assert_eq!(_dyld_get_image_name(0), _dyld_get_image_name(0), "the same name, the same pointer");
    }
}
