//! Files dropped onto Guppy from other apps (Wayland).
//!
//! winit has no drag and drop on Wayland, so Guppy talks the protocol itself:
//! like smithay-clipboard it joins iced's Wayland connection with its own event
//! queue and its own `wl_data_device`, on a thread of its own. Drops arrive as
//! `text/uri-list`, the format every file manager offers.

use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;

use iced::Subscription;
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::{
    delegate_data_device, delegate_registry, delegate_seat, registry_handlers,
};

const URI_LIST: &str = "text/uri-list";

#[derive(Debug, Clone)]
pub enum Event {
    /// Files from another app are over the window at (x, y), or left it (`None`).
    Hover(Option<iced::Point>),
    /// Files were dropped at a point; `copy` is false when the source asked to move.
    Drop { at: iced::Point, paths: Vec<PathBuf>, copy: bool },
}

fn channel() -> &'static (smol::channel::Sender<Event>, smol::channel::Receiver<Event>) {
    static CH: OnceLock<(smol::channel::Sender<Event>, smol::channel::Receiver<Event>)> = OnceLock::new();
    CH.get_or_init(smol::channel::unbounded)
}

/// Drops and hovers from other apps.
pub fn events() -> Subscription<Event> {
    Subscription::run(|| channel().1.clone())
}

/// Joins the Wayland connection behind `display` (a `wl_display*` from iced's
/// window). Does nothing for other display kinds, or when called twice.
pub fn start(display: usize) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if display == 0 || STARTED.set(()).is_err() {
        return;
    }
    // SAFETY: the pointer comes from iced's live window and stays valid for as
    // long as the app runs; this thread never outlives it in practice.
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display as *mut _) };
    let conn = Connection::from_backend(backend);
    std::thread::Builder::new()
        .name("guppy-dnd".into())
        .spawn(move || {
            if let Err(e) = run(conn) {
                eprintln!("guppy: drag and drop from other apps unavailable: {e}");
            }
        })
        .ok();
}

fn run(conn: Connection) -> Result<(), String> {
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        manager: DataDeviceManagerState::bind(&globals, &qh).map_err(|e| e.to_string())?,
        devices: Vec::new(),
        action: DndAction::Copy,
    };
    // sctk only reports seats that appear later; the ones already there are taken here.
    for seat in state.seats.seats().collect::<Vec<_>>() {
        state.new_seat(&conn, &qh, seat);
    }
    loop {
        queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    manager: DataDeviceManagerState,
    devices: Vec<(WlSeat, DataDevice)>,
    /// The action the compositor picked for the current drag.
    action: DndAction,
}

impl State {
    fn offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.devices.iter().find(|(_, d)| d.inner() == device).and_then(|(_, d)| d.data().drag_offer())
    }
}

/// `GUPPY_DND_DEBUG=1`: log what arrives from the compositor.
fn debug(what: std::fmt::Arguments) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("GUPPY_DND_DEBUG").is_some()) {
        eprintln!("guppy-dnd: {what}");
    }
}

fn send(e: Event) {
    let _ = channel().0.try_send(e);
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice, x: f64, y: f64, _: &WlSurface) {
        debug(format_args!("enter {x:.0},{y:.0} offer={}", self.offer(device).is_some()));
        // Drags that started inside Guppy have no offer; those are handled by iced.
        let Some(offer) = self.offer(device) else { return };
        let files = offer.with_mime_types(|m| m.iter().any(|m| m == URI_LIST));
        offer.accept_mime_type(offer.serial, files.then(|| URI_LIST.to_string()));
        if files {
            offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
            send(Event::Hover(Some(iced::Point::new(x as f32, y as f32))));
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        debug(format_args!("leave"));
        send(Event::Hover(None));
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice, x: f64, y: f64) {
        if self.offer(device).is_some() {
            send(Event::Hover(Some(iced::Point::new(x as f32, y as f32))));
        }
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, conn: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        debug(format_args!("drop offer={} action={:?}", self.offer(device).is_some(), self.action));
        let Some(offer) = self.offer(device) else { return };
        let at = iced::Point::new(offer.x as f32, offer.y as f32);
        let copy = !self.action.contains(DndAction::Move);
        let Ok(mut pipe) = offer.receive(URI_LIST.into()) else {
            offer.destroy();
            return;
        };
        // The source writes only once the request has reached it.
        let _ = conn.flush();
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        debug(format_args!("received {text:?}"));
        let paths: Vec<PathBuf> = text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| crate::clip::from_uri(l.trim())).collect();
        offer.finish();
        offer.destroy();
        send(Event::Hover(None));
        if !paths.is_empty() {
            send(Event::Drop { at, paths, copy });
        }
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, _: DndAction) {
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
    }

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, action: DndAction) {
        self.action = action;
    }
}

// Needed by `delegate_data_device!`; Guppy offers nothing to drag out yet.
impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        if !self.devices.iter().any(|(s, _)| *s == seat) {
            debug(format_args!("data device for seat {seat:?}"));
            let device = self.manager.get_data_device(qh, &seat);
            self.devices.push((seat, device));
        }
    }
    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.devices.retain(|(s, _)| *s != seat);
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![SeatState];
}

delegate_data_device!(State);
delegate_seat!(State);
delegate_registry!(State);
