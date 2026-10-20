//! A window drawn with sidestep-ui, Sidestep's native Rust toolkit, with
//! no AppKit and no Objective-C: shapes, gradients, shadows, wrapped and
//! turned text, a button that counts its clicks, and a text field that
//! takes typing, input methods, selection and the clipboard. The widgets
//! are written here, on the toolkit's events, canvas and text layout.
//!
//! `UI_QUIT_AFTER=<seconds>` quits by itself; `UI_LOG=1` prints every
//! window event. Under a headless compositor:
//!
//! ```sh
//! SHOT=/work/target/ui.png scripts/linux-run scripts/headless-wayland /target/debug/ui-gallery
//! ```

#[cfg(target_vendor = "apple")]
fn main() {
    eprintln!("ui-gallery: sidestep-ui is Linux's; on macOS, programs use AppKit through objc2");
}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    let quit_after = std::env::var("UI_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok());
    if let Err(e) = sidestep_ui::App::new().run(ui_gallery::gallery::Gallery::new(quit_after)) {
        eprintln!("ui-gallery: {e}");
        std::process::exit(1);
    }
}
