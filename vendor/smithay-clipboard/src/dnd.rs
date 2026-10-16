//! Guppy addition: files dropped onto the window from other apps.
//!
//! Some compositors (Hyprland) send drag and drop only to the first
//! `wl_data_device` of a client, which is the clipboard's. So the clipboard
//! takes drops too and hands them to a hook the app sets.

use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering};

pub const URI_LIST: &str = "text/uri-list";

/// What happens with files dragged over the window from another app.
#[derive(Debug, Clone)]
pub enum DndEvent {
    /// Files are over the window at a surface-local position.
    Motion { x: f64, y: f64 },
    /// The drag left the window without a drop.
    Leave,
    /// Dropped: the `text/uri-list` as sent, and whether the source asked to move.
    Drop { x: f64, y: f64, uri_list: String, moved: bool },
}

type Hook = Box<dyn Fn(DndEvent) + Send + Sync>;

static HOOK: OnceLock<Hook> = OnceLock::new();

/// The action the compositor picked for the current drag (`DndAction` bits).
pub(crate) static ACTION: AtomicU32 = AtomicU32::new(0);

/// Sets the function that receives drops; only the first call counts.
pub fn set_dnd_hook(f: impl Fn(DndEvent) + Send + Sync + 'static) {
    let _ = HOOK.set(Box::new(f));
}

pub(crate) fn enabled() -> bool {
    HOOK.get().is_some()
}

pub(crate) fn emit(e: DndEvent) {
    if let Some(f) = HOOK.get() {
        f(e);
    }
}

pub(crate) fn set_action(bits: u32) {
    ACTION.store(bits, Ordering::Relaxed);
}

pub(crate) fn action() -> u32 {
    ACTION.load(Ordering::Relaxed)
}

static SENDER: OnceLock<Mutex<sctk::reexports::calloop::channel::Sender<crate::worker::Command>>> = OnceLock::new();

pub(crate) fn set_sender(s: sctk::reexports::calloop::channel::Sender<crate::worker::Command>) {
    let _ = SENDER.set(Mutex::new(s));
}

/// Starts dragging files out of the window, given as a `text/uri-list`. Call
/// it while the mouse button that started the drag is still held. Returns
/// false when there is no Wayland clipboard to do it.
pub fn start_drag(uri_list: String) -> bool {
    SENDER
        .get()
        .and_then(|s| s.lock().ok())
        .is_some_and(|s| s.send(crate::worker::Command::StartDrag(uri_list)).is_ok())
}
