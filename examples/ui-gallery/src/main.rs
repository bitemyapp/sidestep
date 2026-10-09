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
    if let Err(e) = sidestep_ui::App::new().run(gallery::Gallery::new(quit_after)) {
        eprintln!("ui-gallery: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_vendor = "apple"))]
mod gallery {
    use std::time::Duration;

    use sidestep_ui::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Size, Vec2};
    use sidestep_ui::*;

    const MARGIN: f64 = 24.0;
    const BLINK: Duration = Duration::from_millis(530);

    /// A button: where it is, and whether the pointer is over it or
    /// pressing it.
    #[derive(Default)]
    struct Button {
        rect: Rect,
        hovered: bool,
        pressed: bool,
        clicks: u32,
    }

    /// A one-line text field: its text, the selection (`anchor` to `caret`,
    /// bytes), what an input method is composing, and how far its text is
    /// scrolled.
    #[derive(Default)]
    struct Field {
        rect: Rect,
        text: String,
        caret: usize,
        anchor: usize,
        preedit: String,
        focused: bool,
        caret_on: bool,
        scroll: f64,
        selecting: bool,
    }

    impl Field {
        fn selection(&self) -> std::ops::Range<usize> {
            self.caret.min(self.anchor)..self.caret.max(self.anchor)
        }

        /// The text shown: the text with what's being composed at the caret.
        fn shown(&self) -> String {
            let mut s = self.text.clone();
            s.insert_str(self.caret, &self.preedit);
            s
        }

        fn replace_selection(&mut self, with: &str) {
            let range = self.selection();
            self.text.replace_range(range.clone(), with);
            self.caret = range.start + with.len();
            self.anchor = self.caret;
        }

        fn previous(&self, at: usize) -> usize {
            self.text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
        }

        fn next(&self, at: usize) -> usize {
            self.text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
        }

        /// The start of the word before `at`, or the end of the one after.
        fn word(&self, at: usize, forward: bool) -> usize {
            if forward {
                let rest = &self.text[at..];
                let start = rest.find(|c: char| c.is_alphanumeric()).unwrap_or(rest.len());
                at + rest[start..].find(|c: char| !c.is_alphanumeric()).map_or(rest.len(), |e| start + e)
            } else {
                let before = &self.text[..at];
                let end = before.rfind(|c: char| c.is_alphanumeric()).map_or(0, |i| i + 1);
                before[..end].rfind(|c: char| !c.is_alphanumeric()).map_or(0, |i| i + 1)
            }
        }
    }

    pub struct Gallery {
        window: Option<WindowId>,
        quit_after: Option<f64>,
        blink: Option<TimerId>,
        button: Button,
        field: Field,
        /// The pointer, last seen.
        pointer: Point,
        modifiers: Modifiers,
        /// The window has the keyboard.
        window_focused: bool,
        /// The window's size, the paragraph laid out for it, and where the
        /// rule under it goes.
        size: Size,
        paragraph: Option<TextLayout>,
        rule: f64,
    }

    impl Gallery {
        pub fn new(quit_after: Option<f64>) -> Gallery {
            Gallery {
                window: None,
                quit_after,
                blink: None,
                button: Button::default(),
                field: Field { text: "Type here".into(), caret: 9, anchor: 0, ..Field::default() },
                pointer: Point::ZERO,
                modifiers: Modifiers::NONE,
                window_focused: false,
                size: Size::ZERO,
                paragraph: None,
                rule: 0.0,
            }
        }

        fn style(&self, size: f64, color: Color) -> TextStyle {
            TextStyle::new(Font::system(size), color)
        }

        /// The field's text laid out, and where its text starts.
        fn field_layout(&self, canvas_color: Color) -> TextLayout {
            let style = self.style(15.0, canvas_color);
            let shown = self.field.shown();
            let mut builder = TextLayout::builder(&shown, &style).wrap(Wrap::None);
            if !self.field.preedit.is_empty() {
                let at = self.field.caret..self.field.caret + self.field.preedit.len();
                builder = builder.style(at, style.clone().underlined());
            }
            builder.build()
        }

        fn field_origin(&self, layout: &TextLayout) -> Point {
            let r = self.field.rect;
            Point::new(r.x0 + 10.0 - self.field.scroll, r.y0 + (r.height() - layout.size().height) / 2.0)
        }

        /// Keep the caret in view, and tell the input method where it is.
        fn caret_moved(&mut self, cx: &mut Cx) {
            let layout = self.field_layout(Color::BLACK);
            let caret = layout.caret(self.field.caret + self.field.preedit.len(), false);
            let inner = self.field.rect.width() - 20.0;
            if caret.x - self.field.scroll > inner {
                self.field.scroll = caret.x - inner;
            } else if caret.x < self.field.scroll {
                self.field.scroll = caret.x;
            }
            self.field.caret_on = true;
            let Some(w) = self.window else { return };
            if let Some(mut window) = cx.window(w) {
                let origin = self.field_origin(&layout);
                let at = caret.rect(1.0) + origin.to_vec2();
                window.set_text_input(self.field.focused, Some(at));
                window.invalidate(self.field.rect.inflate(4.0, 4.0));
            }
        }

        fn focus_field(&mut self, cx: &mut Cx, focused: bool) {
            if self.field.focused == focused {
                return;
            }
            self.field.focused = focused;
            if let Some(t) = self.blink.take() {
                cx.cancel_timer(t);
            }
            if focused {
                self.blink = Some(cx.set_repeating_timer(BLINK));
            } else {
                self.field.preedit.clear();
            }
            self.caret_moved(cx);
        }

        fn key(&mut self, cx: &mut Cx, key: KeyEvent) {
            if !key.pressed {
                return;
            }
            let m = key.modifiers;
            if m.control() {
                match &key.key {
                    Key::Character(c) if c == "q" => cx.quit(),
                    Key::Character(c) if c == "a" && self.field.focused => {
                        self.field.anchor = 0;
                        self.field.caret = self.field.text.len();
                    }
                    Key::Character(c) if (c == "c" || c == "x") && self.field.focused => {
                        let range = self.field.selection();
                        if !range.is_empty() {
                            cx.set_clipboard_text(&self.field.text[range]);
                            if c == "x" {
                                self.field.replace_selection("");
                            }
                        }
                    }
                    Key::Character(c) if c == "v" && self.field.focused => {
                        if let Some(text) = cx.clipboard_text() {
                            let line = text.lines().next().unwrap_or("");
                            self.field.replace_selection(line);
                        }
                    }
                    _ => {}
                }
                if self.field.focused {
                    self.caret_moved(cx);
                }
                return;
            }
            if !self.field.focused {
                if key.key == Key::Named(NamedKey::Tab) {
                    self.focus_field(cx, true);
                }
                return;
            }
            let f = &mut self.field;
            let collapse = |f: &mut Field, to: usize, extend: bool| {
                f.caret = to;
                if !extend {
                    f.anchor = to;
                }
            };
            match key.key {
                Key::Named(NamedKey::Backspace) => {
                    if f.selection().is_empty() {
                        f.anchor = f.previous(f.caret);
                    }
                    f.replace_selection("");
                }
                Key::Named(NamedKey::Delete) => {
                    if f.selection().is_empty() {
                        f.anchor = f.next(f.caret);
                    }
                    f.replace_selection("");
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    let to = if m.alt() { f.word(f.caret, false) } else { f.previous(f.caret) };
                    let to = if !m.shift() && !f.selection().is_empty() { f.selection().start } else { to };
                    collapse(f, to, m.shift());
                }
                Key::Named(NamedKey::ArrowRight) => {
                    let to = if m.alt() { f.word(f.caret, true) } else { f.next(f.caret) };
                    let to = if !m.shift() && !f.selection().is_empty() { f.selection().end } else { to };
                    collapse(f, to, m.shift());
                }
                Key::Named(NamedKey::Home) => collapse(f, 0, m.shift()),
                Key::Named(NamedKey::End) => collapse(f, f.text.len(), m.shift()),
                Key::Named(NamedKey::Escape | NamedKey::Tab) => {
                    self.focus_field(cx, false);
                    return;
                }
                _ => match key.text {
                    Some(text) => f.replace_selection(&text),
                    None => return,
                },
            }
            self.caret_moved(cx);
        }

        fn pointer(&mut self, cx: &mut Cx, at: Point) {
            self.pointer = at;
            let over_button = self.button.rect.contains(at);
            let over_field = self.field.rect.contains(at);
            if over_button != self.button.hovered {
                self.button.hovered = over_button;
                self.invalidate(cx, self.button.rect.inflate(12.0, 12.0));
            }
            if self.field.selecting {
                self.field.caret = self.field_hit(at);
                self.caret_moved(cx);
            }
            let cursor = if over_button {
                Cursor::Pointer
            } else if over_field {
                Cursor::Text
            } else {
                Cursor::Default
            };
            if let Some(mut window) = self.window.and_then(|w| cx.window(w)) {
                window.set_cursor(cursor);
            }
        }

        fn field_hit(&self, at: Point) -> usize {
            let layout = self.field_layout(Color::BLACK);
            let origin = self.field_origin(&layout);
            layout.hit_test(at - origin.to_vec2()).offset.min(self.field.text.len())
        }

        fn invalidate(&self, cx: &mut Cx, rect: Rect) {
            if let Some(mut window) = self.window.and_then(|w| cx.window(w)) {
                window.invalidate(rect);
            }
        }

        /// Where things go in a window of `size`: the paragraph wrapped to
        /// its width (laid out once here, not in every pass; its colors
        /// are the appearance's), the controls under it.
        fn lay_out(&mut self, size: Size, appearance: Appearance) {
            self.size = size;
            let width = (size.width - 2.0 * MARGIN).max(100.0);
            let body = TextStyle::new(Font::system(14.0), appearance.color(SystemColor::SecondaryLabel));
            let mono = TextStyle::new(Font::monospace(13.0), appearance.color(SystemColor::Accent));
            let text = "This window is drawn with sidestep-ui: no AppKit, no Objective-C runtime. \
                        The render thread rasterizes what the canvas records, and the text engine shapes \
                        every script — العربية, 日本語, emoji 🎉 — with the desktop's fonts.";
            let mark = text.find("sidestep-ui").unwrap_or(0);
            let paragraph = TextLayout::builder(text, &body)
                .style(mark..mark + "sidestep-ui".len(), mono)
                .width(width)
                .line_spacing(2.0)
                .build();
            self.rule = MARGIN + 44.0 + paragraph.size().height + 16.0;
            self.paragraph = Some(paragraph);
            let top = self.rule + 24.0;
            self.button.rect = Rect::from_origin_size((MARGIN, top), (200.0, 40.0));
            self.field.rect = Rect::from_origin_size((MARGIN + 216.0, top), ((width - 216.0).max(120.0), 40.0));
        }
    }

    impl Handler for Gallery {
        fn launched(&mut self, cx: &mut Cx) {
            let options = WindowOptions::new("Sidestep UI").size(720.0, 520.0).min_size(480.0, 420.0);
            self.window = Some(cx.open_window(options));
            if let Some(seconds) = self.quit_after {
                cx.set_timer(Duration::from_secs_f64(seconds));
            }
        }

        fn window_event(&mut self, cx: &mut Cx, _window: WindowId, event: WindowEvent) {
            if std::env::var_os("UI_LOG").is_some() {
                eprintln!("ui-gallery: {event:?}");
            }
            match event {
                WindowEvent::Resized(size) => self.lay_out(size, cx.appearance()),
                WindowEvent::Focused(focused) => {
                    // The field keeps the focus; it shows only while the
                    // window has the keyboard.
                    self.window_focused = focused;
                    self.invalidate(cx, self.field.rect.inflate(4.0, 4.0));
                }
                WindowEvent::ModifiersChanged(m) => self.modifiers = m,
                WindowEvent::Key(key) => self.key(cx, key),
                WindowEvent::PointerEntered(at) => self.pointer(cx, at),
                WindowEvent::PointerMoved { position, .. } => self.pointer(cx, position),
                WindowEvent::PointerLeft => self.pointer(cx, Point::new(-1.0, -1.0)),
                WindowEvent::PointerButton { position, button: PointerButton::Left, pressed, clicks, .. } => {
                    if self.button.rect.contains(position) || self.button.pressed {
                        if !pressed && self.button.pressed && self.button.rect.contains(position) {
                            self.button.clicks += 1;
                        }
                        self.button.pressed = pressed && self.button.rect.contains(position);
                        self.invalidate(cx, self.button.rect.inflate(12.0, 12.0));
                    }
                    if pressed && self.field.rect.contains(position) {
                        self.focus_field(cx, true);
                        let at = self.field_hit(position);
                        if clicks >= 2 {
                            (self.field.anchor, self.field.caret) =
                                (self.field.word(at, false), self.field.word(at, true));
                        } else {
                            self.field.caret = at;
                            if !self.modifiers.shift() {
                                self.field.anchor = at;
                            }
                            self.field.selecting = true;
                        }
                        self.caret_moved(cx);
                    } else if pressed {
                        self.focus_field(cx, false);
                    }
                    if !pressed {
                        self.field.selecting = false;
                    }
                }
                WindowEvent::Ime(Ime::Commit(text)) => {
                    self.field.preedit.clear();
                    self.field.replace_selection(&text);
                    self.caret_moved(cx);
                }
                WindowEvent::Ime(Ime::Preedit { text, .. }) => {
                    self.field.preedit = text;
                    self.caret_moved(cx);
                }
                _ => {}
            }
        }

        fn appearance_changed(&mut self, _cx: &mut Cx, appearance: Appearance) {
            // The paragraph's colors are the old appearance's.
            self.lay_out(self.size, appearance);
        }

        fn timer(&mut self, cx: &mut Cx, timer: TimerId) {
            if Some(timer) == self.blink {
                self.field.caret_on = !self.field.caret_on;
                self.invalidate(cx, self.field.rect);
            } else {
                cx.quit();
            }
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            let size = canvas.size();
            let label = canvas.system_color(SystemColor::Label);
            let accent = canvas.system_color(SystemColor::Accent);
            let separator = canvas.system_color(SystemColor::Separator);

            // A heading, and the paragraph laid out for the window's width.
            let heading = TextStyle::new(Font::system(24.0).bold(), label);
            canvas.draw_label("Sidestep, natively", &heading, Point::new(MARGIN, MARGIN));
            if let Some(paragraph) = &self.paragraph {
                canvas.draw_text(paragraph, Point::new(MARGIN, MARGIN + 44.0));
            }
            canvas.fill_rect(Rect::new(MARGIN, self.rule, size.width - MARGIN, self.rule + 1.0), separator);

            // The button: the accent, lighter under the pointer, darker
            // pressed, with a soft shadow.
            let b = &self.button;
            let face = if b.pressed {
                accent.mix(Color::BLACK, 0.2)
            } else if b.hovered {
                accent.mix(Color::WHITE, 0.15)
            } else {
                accent
            };
            canvas.saved(|c| {
                c.set_shadow(Some(Shadow {
                    offset: Vec2::new(0.0, 2.0),
                    blur: 6.0,
                    color: Color::rgba(0.0, 0.0, 0.0, 0.25),
                }));
                c.fill(&RoundedRect::from_rect(b.rect, 8.0), face);
            });
            let caption = match b.clicks {
                0 => "Click me".to_owned(),
                1 => "Clicked once".to_owned(),
                n => format!("Clicked {n} times"),
            };
            let style = TextStyle::new(Font::system(15.0).with_weight(600.0), Color::WHITE);
            let caption_size = measure(&caption, &style);
            let at = b.rect.center() - caption_size.to_vec2() / 2.0;
            canvas.draw_label(&caption, &style, at);

            // The text field.
            let f = &self.field;
            let active = f.focused && self.window_focused;
            let content = canvas.system_color(SystemColor::TextBackground);
            let border = if active { accent } else { separator };
            let frame = RoundedRect::from_rect(f.rect, 6.0);
            canvas.fill(&frame, content);
            let edge = StrokeStyle::new(if active { 2.0 } else { 1.0 });
            canvas.stroke(&frame.rect().inset(-0.5).to_rounded_rect(6.0), &edge, border);
            let layout = self.field_layout(canvas.system_color(SystemColor::Text));
            let origin = self.field_origin(&layout);
            canvas.saved(|c| {
                c.clip_rect(f.rect.inset(-4.0));
                if f.focused {
                    let selected = c.system_color(if active {
                        SystemColor::SelectedTextBackground
                    } else {
                        SystemColor::UnemphasizedSelectedContentBackground
                    });
                    for r in layout.selection_rects(f.selection()) {
                        c.fill_rect(r + origin.to_vec2(), selected);
                    }
                }
                c.draw_text(&layout, origin);
                if active && f.caret_on && f.selection().is_empty() {
                    let caret = layout.caret(f.caret + f.preedit.len(), false);
                    let insertion = c.system_color(SystemColor::TextInsertionPoint);
                    c.fill_rect(caret.rect(1.5) + origin.to_vec2(), insertion);
                }
            });

            // Shapes: a radial gradient, a dashed star, a turned label.
            let top = self.button.rect.y1 + 36.0;
            let ball = Circle::new((MARGIN + 50.0, top + 60.0), 50.0);
            canvas.fill(
                &ball,
                RadialGradient {
                    start_center: Point::new(MARGIN + 35.0, top + 42.0),
                    start_radius: 4.0,
                    center: ball.center,
                    radius: 50.0,
                    stops: vec![(0.0, Color::WHITE), (1.0, accent)],
                },
            );
            let star = star(Point::new(MARGIN + 190.0, top + 60.0), 50.0, 22.0);
            canvas.fill(
                &star,
                LinearGradient::new(
                    Point::new(0.0, top),
                    Point::new(0.0, top + 120.0),
                    Color::hex(0xf6d32d),
                    Color::hex(0xe66100),
                ),
            );
            canvas.stroke(
                &star,
                &StrokeStyle::new(2.0).with_join(LineJoin::Round).with_dash(vec![6.0, 4.0], 0.0),
                label.with_alpha(0.6),
            );
            canvas.saved(|c| {
                c.translate((MARGIN + 330.0, top + 60.0));
                c.rotate(-0.35);
                let style = TextStyle::new(Font::serif(22.0).italic(), label);
                let s = measure("turned text", &style);
                c.draw_label("turned text", &style, Point::new(-s.width / 2.0, -s.height / 2.0));
            });
            canvas.group(0.6, |c| {
                let a = Rect::from_origin_size((MARGIN + 430.0, top + 20.0), (80.0, 80.0));
                c.fill(&RoundedRect::from_rect(a, 10.0), Color::hex(0x26a269));
                c.fill(&RoundedRect::from_rect(a + Vec2::new(30.0, 20.0), 10.0), Color::hex(0x1c71d8));
            });

            let hint = "Click the field and type · Tab to focus · Ctrl+C, X, V, A · Ctrl+Q quits";
            let hint_style = TextStyle::new(Font::system(12.0), canvas.system_color(SystemColor::TertiaryLabel));
            canvas.draw_label(hint, &hint_style, Point::new(MARGIN, size.height - MARGIN - 14.0));
        }
    }

    /// A five-pointed star around `center`.
    fn star(center: Point, outer: f64, inner: f64) -> BezPath {
        let mut path = BezPath::new();
        for i in 0..10 {
            let r = if i % 2 == 0 { outer } else { inner };
            let angle = std::f64::consts::PI * f64::from(i) / 5.0 - std::f64::consts::FRAC_PI_2;
            let p = center + (Affine::rotate(angle) * Point::new(r, 0.0)).to_vec2();
            if i == 0 { path.move_to(p) } else { path.line_to(p) }
        }
        path.close_path();
        path
    }
}
