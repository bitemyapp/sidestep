//! Sidestep's own stories: sidestep-ui's system colors and type, drawn,
//! and the ui-gallery program in its states. See `sidestep-stories`.
//!
//! ```sh
//! cargo run -p stories -- list
//! cargo run -p stories -- render ui-gallery/typed dark@2x
//! cargo run -p stories -- check            # against examples/stories/golden
//! cargo run -p stories -- check --update   # write the golden images
//! ```

#[cfg(target_vendor = "apple")]
fn main() {
    eprintln!("stories: sidestep-ui is Linux's; on macOS, programs use AppKit through objc2");
}

#[cfg(not(target_vendor = "apple"))]
fn main() -> std::process::ExitCode {
    let mut book = sidestep_stories::Book::new(concat!(env!("CARGO_MANIFEST_DIR"), "/golden"));
    drawn::add(&mut book);
    gallery::add(&mut book);
    book.run()
}

/// The toolkit's colors and type, on a canvas.
#[cfg(not(target_vendor = "apple"))]
mod drawn {
    use sidestep_stories::{Book, Knobs};
    use sidestep_ui::kurbo::{Point, Rect, RoundedRect, Size};
    use sidestep_ui::*;

    const COLORS: [(&str, SystemColor); 12] = [
        ("Window", SystemColor::WindowBackground),
        ("Content", SystemColor::ContentBackground),
        ("Label", SystemColor::Label),
        ("Secondary", SystemColor::SecondaryLabel),
        ("Tertiary", SystemColor::TertiaryLabel),
        ("Accent", SystemColor::Accent),
        ("Selected", SystemColor::SelectedContentBackground),
        ("Separator", SystemColor::Separator),
        ("Text", SystemColor::Text),
        ("Text bg", SystemColor::TextBackground),
        ("Placeholder", SystemColor::PlaceholderText),
        ("Insertion", SystemColor::TextInsertionPoint),
    ];

    pub fn add(book: &mut Book) {
        let all = [Knobs::LIGHT, Knobs::DARK, Knobs::LIGHT.high_contrast(), Knobs::DARK.accent(0xe66100).at(2.0)];
        book.drawn("colors/system", Size::new(420.0, 236.0), |canvas, _| system_colors(canvas)).variants(all);
        book.drawn("type/styles", Size::new(420.0, 200.0), |canvas, _| styles(canvas))
            .variants([Knobs::LIGHT, Knobs::DARK.at(1.5)]);
    }

    /// A swatch and its name for each system color, on the window's
    /// background.
    fn system_colors(canvas: &mut Canvas) {
        let size = canvas.size();
        let background = canvas.system_color(SystemColor::WindowBackground);
        canvas.fill_rect(Rect::from_origin_size(Point::ZERO, size), background);
        let label = TextStyle::new(Font::system(12.0), canvas.system_color(SystemColor::Label));
        let edge = canvas.system_color(SystemColor::Separator);
        for (i, (name, color)) in COLORS.iter().enumerate() {
            let (column, row) = ((i % 3) as f64, (i / 3) as f64);
            let at = Point::new(16.0 + column * 134.0, 16.0 + row * 54.0);
            let swatch = RoundedRect::from_rect(Rect::from_origin_size(at, (32.0, 32.0)), 6.0);
            let fill = canvas.system_color(*color);
            canvas.fill(&swatch, fill);
            canvas.stroke(&swatch, &StrokeStyle::new(1.0), edge);
            canvas.draw_label(name, &label, Point::new(at.x + 40.0, at.y + 8.0));
        }
    }

    /// The system fonts at a few sizes and weights.
    fn styles(canvas: &mut Canvas) {
        let size = canvas.size();
        let background = canvas.system_color(SystemColor::WindowBackground);
        canvas.fill_rect(Rect::from_origin_size(Point::ZERO, size), background);
        let color = canvas.system_color(SystemColor::Label);
        let lines = [
            ("Heading, bold", Font::system(24.0).bold()),
            ("Body text at fifteen points", Font::system(15.0)),
            ("Semibold label", Font::system(13.0).with_weight(600.0)),
            ("fn monospace() -> &'static str", Font::monospace(13.0)),
            ("Italic serif", Font::serif(16.0).italic()),
        ];
        let mut y = 16.0;
        for (text, font) in lines {
            let style = TextStyle::new(font, color);
            canvas.draw_label(text, &style, Point::new(16.0, y));
            y += measure(text, &style).height + 10.0;
        }
    }
}

/// ui-gallery's window: as it opens, its button clicked, its field typed
/// into.
#[cfg(not(target_vendor = "apple"))]
mod gallery {
    use sidestep_stories::{Book, Knobs};
    use sidestep_ui::NamedKey;
    use ui_gallery::gallery::Gallery;

    pub fn add(book: &mut Book) {
        let variants = [Knobs::LIGHT, Knobs::DARK.at(2.0)];
        book.app("ui-gallery/opened", |_| Gallery::new(None)).variants(variants);
        book.app("ui-gallery/clicked-twice", |_| Gallery::new(None))
            .step(|g, d| d.click(g.button_rect().center()))
            .step(|g, d| d.click(g.button_rect().center()))
            .variants(variants);
        book.app("ui-gallery/typed", |_| Gallery::new(None))
            .step(|g, d| d.click(g.field_rect().center()))
            .step(|_, d| {
                // Select all, then type over it.
                d.shortcut('a', sidestep_ui::Modifiers::CONTROL);
                d.type_text("Hello, stories");
            })
            .step(|_, d| d.key(NamedKey::ArrowLeft))
            .variants(variants);
    }
}
