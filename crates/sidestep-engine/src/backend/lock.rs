//! The session lock (ext-session-lock-v1): a lock screen's program asks
//! the compositor to lock the session (`ToRender::LockSession`), which
//! hides everything else and gives all input to the lock's surfaces, one
//! on each output, then says it has (`locked`); unlocking ends it.
//!
//! A lock surface is a window's content surface itself, as a layer
//! surface's is: no frame, no decorations, its role the root's, so it
//! takes the keyboard and pointer as any window's content. The
//! compositor sizes it (the output's size) with a configure. A lock the
//! compositor refuses or ends (`finished`) goes, and the main thread
//! hears it; its surfaces then show nothing. Unlocking a lock that was
//! never confirmed withdraws it.

use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::session_lock::{
    SessionLock, SessionLockHandler, SessionLockSurface, SessionLockSurfaceConfigure,
};

use super::{Role, Shell, State};
use crate::protocol::FromRender;

/// Ask for the lock, unless one is asked for or held.
pub(super) fn lock(state: &mut State) {
    if state.session_lock.is_some() {
        return;
    }
    match state.session_locks.lock(&state.qh) {
        Ok(lock) => state.session_lock = Some(lock),
        Err(_) => {
            eprintln!("sidestep: the compositor has no ext-session-lock");
            state.send(FromRender::SessionLockFinished);
        }
    }
}

/// Unlock, or withdraw a lock not yet confirmed (dropping the last handle
/// destroys it, which a compositor takes as that).
pub(super) fn unlock(state: &mut State) {
    if let Some(lock) = state.session_lock.take() {
        lock.unlock();
    }
}

/// `surface` as the lock's surface on output `output` (by its id), for
/// the compositor to configure; `None` without a lock asked for, without
/// that output, or with a lock surface there already (a second is a
/// protocol error).
pub(super) fn create(state: &State, surface: &WlSurface, output: u32) -> Option<SessionLockSurface> {
    let lock = state.session_lock.as_ref()?;
    if state.windows.values().any(|w| matches!(w.shell, Shell::Lock(_, on) if on == output)) {
        return None;
    }
    let on = super::layer::output(state, output)?;
    Some(lock.create_lock_surface(surface.clone(), &on, &state.qh))
}

impl SessionLockHandler for State {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        self.send(FromRender::SessionLocked);
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        // Refused or ended: the lock object goes, unlocked if it was
        // locked (as the protocol asks), else destroyed.
        unlock(self);
        self.send(FromRender::SessionLockFinished);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _: u32,
    ) {
        let Some(Role::Root(window)) = self.role(surface.wl_surface()) else { return };
        let (width, height) = configure.new_size;
        self.resize(window, width, height);
    }
}
