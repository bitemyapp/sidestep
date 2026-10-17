//! A virtual pointer: a client of its own, moving and clicking where
//! it's told, in the output's points (the headless sway's one output,
//! 1280 × 800 unless `SIZE` says otherwise, at scale 1). The Wayland
//! tests include it (`#[path]`).

use std::sync::mpsc::Receiver;

use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use smithay_client_toolkit::reexports::client::protocol::wl_registry::WlRegistry;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

pub enum Do {
    MoveTo(u32, u32),
    Click,
    /// Detents down (up if negative).
    Wheel(i32),
    Done,
}

const BTN_LEFT: u32 = 0x110;

struct State;

pub fn run(commands: Receiver<Do>) {
    let conn = Connection::connect_to_env().expect("a compositor");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("its registry");
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("a seat");
    let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("virtual pointers");
    let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
    queue.roundtrip(&mut State).expect("a round trip");
    let (width, height) = std::env::var("SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((1280, 800));
    let time = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
    for command in commands {
        match command {
            Do::MoveTo(x, y) => {
                pointer.motion_absolute(time(), x, y, width, height);
                pointer.frame();
            }
            Do::Click => {
                pointer.button(time(), BTN_LEFT, ButtonState::Pressed);
                pointer.frame();
                pointer.button(time(), BTN_LEFT, ButtonState::Released);
                pointer.frame();
            }
            Do::Wheel(detents) => {
                pointer.axis_source(AxisSource::Wheel);
                pointer.axis_discrete(time(), Axis::VerticalScroll, 15.0 * f64::from(detents), detents);
                pointer.frame();
            }
            Do::Done => break,
        }
        conn.flush().expect("sent");
        // Clicks of a double click come well inside its 400 ms.
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    pointer.destroy();
    let _ = conn.flush();
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerManagerV1,
        _: <ZwlrVirtualPointerManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerV1,
        _: <ZwlrVirtualPointerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
