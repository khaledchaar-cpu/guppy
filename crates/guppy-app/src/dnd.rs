//! Files dropped onto Guppy from other apps (Wayland).
//!
//! winit has no drag and drop on Wayland, and Hyprland sends it only to a
//! client's first `wl_data_device`, which belongs to iced's clipboard. So the
//! vendored smithay-clipboard takes the drops and hands them over here.

use std::path::PathBuf;
use std::sync::OnceLock;

use iced::Subscription;
use smithay_clipboard::DndEvent;

#[derive(Debug, Clone)]
pub enum Event {
    /// Files from another app are over the window at a point, or left it (`None`).
    Hover(Option<iced::Point>),
    /// Files were dropped at a point; `copy` is false when the source asked to move.
    Drop { at: iced::Point, paths: Vec<PathBuf>, copy: bool },
}

fn channel() -> &'static (smol::channel::Sender<Event>, smol::channel::Receiver<Event>) {
    static CH: OnceLock<(smol::channel::Sender<Event>, smol::channel::Receiver<Event>)> = OnceLock::new();
    CH.get_or_init(smol::channel::unbounded)
}

/// Starts taking drops; call before the window opens.
pub fn start() {
    smithay_clipboard::set_dnd_hook(|e| {
        let debug = std::env::var_os("GUPPY_DND_DEBUG").is_some();
        if debug {
            eprintln!("guppy-dnd: {e:?}");
        }
        let e = match e {
            DndEvent::Motion { x, y } => Event::Hover(Some(iced::Point::new(x as f32, y as f32))),
            DndEvent::Leave => Event::Hover(None),
            DndEvent::Drop { x, y, uri_list, moved } => {
                let paths: Vec<PathBuf> = uri_list.lines().filter(|l| !l.starts_with('#')).filter_map(|l| crate::clip::from_uri(l.trim())).collect();
                let _ = channel().0.try_send(Event::Hover(None));
                if paths.is_empty() {
                    return;
                }
                Event::Drop { at: iced::Point::new(x as f32, y as f32), paths, copy: !moved }
            }
        };
        let _ = channel().0.try_send(e);
    });
}

/// Drops and hovers from other apps.
pub fn events() -> Subscription<Event> {
    Subscription::run(|| channel().1.clone())
}
