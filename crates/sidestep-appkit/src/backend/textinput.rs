//! Input methods on the render thread: a zwp_text_input_v3 per seat.
//!
//! Text input follows the keyboard. When it's on a window whose first
//! responder takes composed text (the main thread says so), the text input
//! is enabled, with the caret's rectangle for the input method to place its
//! candidate window by. What the input method sends between two `done`
//! events (text to insert, text being composed) goes to the main thread as
//! one message, which applies it through `NSTextInputClient`. Surrounding
//! text isn't sent, so input methods don't ask to delete around the caret.

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use super::{Role, State};
use crate::protocol::{FromRender, Rect, WindowId};

pub(crate) struct TextInputs {
    manager: Option<ZwpTextInputManagerV3>,
    inputs: Vec<Input>,
}

struct Input {
    seat: WlSeat,
    input: ZwpTextInputV3,
    /// The window text input is on.
    focus: Option<WindowId>,
    /// The surface it's on (a toplevel's frame, or a popup's content),
    /// whose coordinates carets are given in.
    surface: Option<Role>,
    enabled: bool,
    /// What arrived since the last `done`.
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
}

impl TextInputs {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        TextInputs { manager: globals.bind::<ZwpTextInputManagerV3, _, _>(qh, 1..=1, ()).ok(), inputs: Vec::new() }
    }
}

pub(super) fn add_seat(state: &mut State, seat: &WlSeat) {
    let Some(manager) = &state.text_inputs.manager else { return };
    let input = manager.get_text_input(seat, &state.qh, ());
    state.text_inputs.inputs.push(Input {
        seat: seat.clone(),
        input,
        focus: None,
        surface: None,
        enabled: false,
        preedit: None,
        commit: None,
    });
}

pub(super) fn remove_seat(state: &mut State, seat: &WlSeat) {
    state.text_inputs.inputs.retain(|i| {
        let keep = i.seat != *seat;
        if !keep {
            i.input.destroy();
        }
        keep
    });
}

/// The main thread says whether the window's first responder takes
/// composed text, and where its caret is.
pub(super) fn set_wanted(state: &mut State, window: WindowId, wanted: bool, caret: Option<Rect>) {
    let Some(win) = state.windows.get_mut(&window) else { return };
    win.text_input = wanted;
    let caret_changed = win.caret != caret;
    win.caret = caret;
    for i in 0..state.text_inputs.inputs.len() {
        if state.text_inputs.inputs[i].focus != Some(window) {
            continue;
        }
        let enabled = state.text_inputs.inputs[i].enabled;
        if wanted != enabled {
            apply(state, i, wanted);
        } else if wanted && caret_changed {
            set_caret(state, i);
            state.text_inputs.inputs[i].input.commit();
        }
    }
}

/// `window`'s content moved in the surface text input is on (its title bar
/// changed): the caret moved with it.
pub(super) fn content_moved(state: &State, window: WindowId) {
    for i in 0..state.text_inputs.inputs.len() {
        let input = &state.text_inputs.inputs[i];
        if input.focus == Some(window) && input.enabled {
            set_caret(state, i);
            input.input.commit();
        }
    }
}

/// Start over: the program dropped what was being composed.
pub(super) fn reset(state: &mut State, window: WindowId) {
    for i in 0..state.text_inputs.inputs.len() {
        let input = &state.text_inputs.inputs[i];
        if input.focus == Some(window) && input.enabled {
            apply(state, i, false);
            apply(state, i, true);
        }
    }
}

/// Enable or disable input `i` for its focused window, with the window's
/// caret.
fn apply(state: &mut State, i: usize, enable: bool) {
    let input = &state.text_inputs.inputs[i];
    if enable {
        input.input.enable();
        input.input.set_content_type(ContentHint::None, ContentPurpose::Normal);
        set_caret(state, i);
    } else {
        input.input.disable();
    }
    input.input.commit();
    state.text_inputs.inputs[i].enabled = enable;
}

/// Give input `i` its window's caret, if it has one, in the coordinates of
/// the surface the input is on.
fn set_caret(state: &State, i: usize) {
    let input = &state.text_inputs.inputs[i];
    let Some(r) = input.focus.and_then(|w| state.windows.get(&w)).and_then(|w| w.caret) else { return };
    let (dx, dy) = input.surface.map_or((0.0, 0.0), |s| state.content_offset(s));
    input.input.set_cursor_rectangle(
        (r.x0 - dx as f32).floor() as i32,
        (r.y0 - dy as f32).floor() as i32,
        (r.x1 - r.x0).ceil().max(1.0) as i32,
        (r.y1 - r.y0).ceil().max(1.0) as i32,
    );
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwpTextInputManagerV3,
        _: <ZwpTextInputManagerV3 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut State,
        proxy: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(i) = state.text_inputs.inputs.iter().position(|i| i.input == *proxy) else { return };
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                let role = state.role(&surface).filter(|r| matches!(r, Role::Root(_) | Role::Frame(_)));
                let window = role.map(Role::window);
                state.text_inputs.inputs[i].focus = window;
                state.text_inputs.inputs[i].surface = role;
                if window.and_then(|w| state.windows.get(&w)).is_some_and(|w| w.text_input) {
                    apply(state, i, true);
                }
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                let input = &mut state.text_inputs.inputs[i];
                let window = input.focus.take();
                input.surface = None;
                input.preedit = None;
                input.commit = None;
                if input.enabled {
                    input.input.disable();
                    input.input.commit();
                    input.enabled = false;
                }
                // Whatever was being composed goes away with the focus.
                if let Some(window) = window {
                    state.send(FromRender::TextInput { window, commit: None, preedit: (String::new(), 0, 0) });
                }
            }
            zwp_text_input_v3::Event::PreeditString { text, cursor_begin, cursor_end } => {
                state.text_inputs.inputs[i].preedit = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                state.text_inputs.inputs[i].commit = Some(text.unwrap_or_default());
            }
            zwp_text_input_v3::Event::DeleteSurroundingText { .. } => {
                // No surrounding text is sent, so there's nothing to delete.
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let input = &mut state.text_inputs.inputs[i];
                let commit = input.commit.take();
                // A done without a preedit event ends any composing.
                let preedit = input.preedit.take().unwrap_or((String::new(), 0, 0));
                if let Some(window) = input.focus {
                    state.send(FromRender::TextInput { window, commit, preedit });
                }
            }
            _ => {}
        }
    }
}
