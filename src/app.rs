use egui::{Rect, RichText, pos2, vec2};

use crate::bt;
use crate::form::{self, Form, FormState, Key, Press, Row};
use crate::hypr;
use crate::nm::{Backend, Op, Reply, Request, Snapshot};
use crate::pw;
use crate::screens::bluetooth::Bluetooth;
use crate::screens::displays::Displays;
use crate::screens::networks::Networks;
use crate::screens::overlay::Overlay;
use crate::screens::sound::Sound;
use crate::screens::{Action, Cx};
use crate::theme::Palette;

const MENU_WIDTH: f32 = 200.0;
const GAP: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Network,
    Bluetooth,
    Sound,
    Screens,
}

const PAGES: [(&str, Page); 4] = [
    ("Network", Page::Network),
    ("Bluetooth", Page::Bluetooth),
    ("Sound", Page::Sound),
    ("Screens", Page::Screens),
];

pub struct App {
    pal: Palette,
    backend: Backend,
    bt: bt::Backend,
    sound_backend: pw::Backend,
    screens_backend: hypr::Backend,
    snap: Snapshot,
    loaded: bool,
    /// One focus for the menu and the page beside it, so Tab moves between them.
    form: FormState,
    page: Page,
    networks: Networks,
    bluetooth: Bluetooth,
    sound: Sound,
    displays: Displays,
    overlay: Option<Overlay>,
    keys: Vec<Key>,
    /// Whether a text field had focus last frame, so it keeps its keys.
    entry: bool,
}

impl App {
    pub fn new(ctx: &egui::Context, pal: Palette) -> Self {
        let mut backend = Backend::start(ctx.clone());
        backend.send(Request::Refresh { rescan: false });
        Self {
            pal,
            backend,
            bt: bt::Backend::start(ctx.clone()),
            sound_backend: pw::Backend::start(ctx.clone()),
            screens_backend: hypr::Backend::start(ctx.clone()),
            snap: Snapshot::default(),
            loaded: false,
            form: FormState::focused("list"),
            page: Page::Network,
            networks: Networks::new(),
            bluetooth: Bluetooth::new(),
            sound: Sound::new(),
            displays: Displays::new(),
            overlay: None,
            keys: Vec::new(),
            entry: false,
        }
    }

    fn handle(&mut self, reply: Reply) {
        match reply {
            Reply::Snapshot(Ok(snap)) => {
                self.networks.scanning &= !snap.rescanned;
                self.networks.refreshed(&snap);
                self.snap = snap;
                self.loaded = true;
            }
            Reply::Snapshot(Err(err)) => {
                self.networks.scanning = false;
                log::warn!("refresh failed: {err}");
                if !self.loaded && self.overlay.is_none() {
                    self.overlay = Some(Overlay::message("Could not contact NetworkManager", err));
                }
            }
            Reply::Beacons { beacons, iw_error } => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.beacons(beacons, iw_error);
                }
            }
            Reply::Details { device, rows } => self.networks.details_arrived(device, rows),
            Reply::Loaded(result) => {
                if let Err(err) = self.networks.loaded(result) {
                    self.overlay = Some(Overlay::message("Could not load the connection", err));
                }
            }
            Reply::Done(op, result) => {
                self.close_wait();
                match (op, result) {
                    (op, Err(err)) => {
                        let title = match op {
                            Op::Save => "Could not save the connection",
                            Op::Delete => "Could not delete the connection",
                            Op::Activate => "Could not activate the connection",
                            Op::Deactivate => "Could not deactivate the connection",
                            Op::Wifi => "Could not switch Wi-Fi",
                            Op::Export => "Could not write the NixOS module",
                        };
                        self.overlay = Some(Overlay::message(title, err));
                    }
                    (Op::Save, Ok(())) => self.networks.reset_editor(),
                    _ => {}
                }
            }
        }
    }

    fn handle_bt(&mut self, reply: bt::Reply) {
        match reply {
            bt::Reply::Snapshot(snap) => self.bluetooth.snapshot(snap),
            bt::Reply::Scan(result) => self.bluetooth.scanned(result),
            bt::Reply::Passkey(code) => {
                if self.overlay.as_ref().is_some_and(Overlay::is_wait) {
                    self.overlay = Some(Overlay::wait(format!(
                        "Type {code} on the device, then press Enter."
                    )));
                }
            }
            bt::Reply::Done(op, result) => {
                self.close_wait();
                if let Err(err) = result {
                    let title = match op {
                        bt::Op::Power => "Could not switch Bluetooth",
                        bt::Op::Pair => "Could not pair the device",
                        bt::Op::Connect => "Could not connect the device",
                        bt::Op::Disconnect => "Could not disconnect the device",
                        bt::Op::Trust => "Could not change whether the device is trusted",
                        bt::Op::Block => "Could not change whether the device is blocked",
                        bt::Op::Remove => "Could not remove the device",
                    };
                    self.overlay = Some(Overlay::message(title, err));
                }
            }
        }
    }

    fn handle_sound(&mut self, reply: pw::Reply) {
        match reply {
            pw::Reply::Snapshot(snap) => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.sound(&snap);
                }
                self.sound.snapshot(snap);
            }
            pw::Reply::Done(op, result) => {
                let graph = !matches!(op, pw::Op::Current | pw::Op::Default);
                let shown = graph
                    && self
                        .overlay
                        .as_mut()
                        .is_some_and(|overlay| overlay.sound_done(&result));
                if !shown {
                    self.sound.done(result);
                }
            }
        }
    }

    fn handle_screens(&mut self, reply: hypr::Reply) {
        match reply {
            hypr::Reply::Snapshot(snap) => self.displays.snapshot(snap),
            hypr::Reply::Done(result) => self.displays.done(result),
        }
    }

    fn close_wait(&mut self) {
        if self.overlay.as_ref().is_some_and(Overlay::is_wait)
            && !self.backend.busy()
            && !self.bt.busy()
        {
            self.overlay = None;
        }
    }

    fn send_bt(&mut self, request: bt::Request) {
        let wait = match &request {
            bt::Request::Pair { name, .. } => Some(format!("Pairing with '{name}'…")),
            bt::Request::Connect { name, .. } => Some(format!("Connecting to '{name}'…")),
            bt::Request::Disconnect { name, .. } => Some(format!("Disconnecting '{name}'…")),
            bt::Request::Remove { name, .. } => Some(format!("Removing '{name}'…")),
            bt::Request::Refresh
            | bt::Request::Scan
            | bt::Request::StopScan
            | bt::Request::Power(_)
            | bt::Request::Trust { .. }
            | bt::Request::Block { .. } => None,
        };
        if let Some(text) = wait {
            self.overlay = Some(Overlay::wait(text));
        }
        self.bt.send(request);
    }

    fn send(&mut self, request: Request) {
        let wait = match &request {
            Request::Save(_) => Some("Saving the connection…".to_owned()),
            Request::Delete(_) => Some("Deleting the connection…".to_owned()),
            Request::Up(_) => Some("Activating the connection…".to_owned()),
            Request::Down(_) => Some("Deactivating the connection…".to_owned()),
            Request::Connect { ssid, .. } => Some(format!("Connecting to '{ssid}'…")),
            Request::Export(_) => Some("Writing the NixOS module…".to_owned()),
            Request::Refresh { .. }
            | Request::Load(_)
            | Request::SetWifi(_)
            | Request::Beacons(_)
            | Request::Details { .. } => None,
        };
        if let Some(text) = wait {
            self.overlay = Some(Overlay::wait(text));
        }
        self.backend.send(request);
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Pop => self.overlay = None,
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Send(request) => self.send(request),
            Action::Bluetooth(request) => self.send_bt(request),
            Action::Overlay(overlay) => self.overlay = Some(overlay),
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.keys = form::take_keys(raw_input, self.entry);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Mesa's eglSwapBuffers waits for the previous frame's Wayland frame callback,
        // which never comes while the window is hidden, so the UI thread would hang
        // and miss the compositor's pings. This makes winit hold back the next redraw
        // until that callback has arrived, so we never swap into the wait.
        if let Some(window) = frame.winit_window() {
            window.pre_present_notify();
        }
        let ctx = ui.ctx().clone();
        for reply in self.backend.poll() {
            self.handle(reply);
        }
        for reply in self.bt.poll() {
            self.handle_bt(reply);
        }
        for reply in self.sound_backend.poll() {
            self.handle_sound(reply);
        }
        for reply in self.screens_backend.poll() {
            self.handle_screens(reply);
        }
        ui.painter()
            .rect_filled(ctx.content_rect(), 0.0, self.pal.window);
        let area = form::content(&ctx);
        let menu = Rect::from_min_max(area.min, pos2(area.left() + MENU_WIDTH, area.bottom()));
        let page_rect = Rect::from_min_max(pos2(menu.right() + GAP, area.top()), area.max);

        let keys = std::mem::take(&mut self.keys);
        // The screen stays visible behind a popup; it gets no keys, the popup
        // blocks its clicks, and whatever it returns is dropped.
        let screen_keys: &[Key] = if self.overlay.is_some() { &[] } else { &keys };
        let mut cx = Cx {
            ctx: &ctx,
            pal: &self.pal,
            keys: screen_keys,
            snap: &self.snap,
            entry: false,
            wifi: self.snap.wifi_enabled,
            send: Vec::new(),
            bt: Vec::new(),
            sound: Vec::new(),
            screens: Vec::new(),
        };
        let mut form = Form::new(&mut self.form, screen_keys, &self.pal);

        let rows: Vec<Row<'_>> = PAGES
            .iter()
            .map(|(text, _)| Row::Item {
                text,
                detail: None,
                signal: None,
                switch: None,
            })
            .collect();
        let mut index = PAGES
            .iter()
            .position(|(_, page)| *page == self.page)
            .unwrap_or(0);
        let menu_press = form::screen(&ctx, &self.pal, "menu", "Settings", menu, |ui| {
            form.list(
                ui,
                "menu",
                &rows,
                &mut index,
                vec2(MENU_WIDTH, ui.available_height()),
            )
        });
        let page = PAGES[index].1;
        if page != self.page {
            log::debug!("page: {}", PAGES[index].0);
            if self.page == Page::Network {
                self.networks.closed();
            }
            if self.page == Page::Bluetooth {
                cx.bt.push(bt::Request::StopScan);
                self.bluetooth.closed();
            }
            if page == Page::Bluetooth {
                self.bluetooth.opened();
            }
            if page == Page::Sound {
                self.sound.opened();
            }
            if page == Page::Screens {
                self.displays.opened();
            }
            self.page = page;
        }
        let action = match self.page {
            Page::Network => self.networks.show(&mut cx, &mut form, page_rect),
            Page::Bluetooth => self.bluetooth.show(&mut cx, &mut form, page_rect),
            Page::Sound => self.sound.show(&mut cx, &mut form, page_rect),
            Page::Screens => self.displays.show(&mut cx, &mut form, page_rect),
        };
        let escape = form.escape();
        cx.entry = form.finish(&ctx);
        let mut actions: Vec<Action> = Vec::new();
        if self.overlay.is_none() {
            actions.extend(action);
        }
        if escape {
            match self.form.focus.as_deref() {
                Some("list" | "menu") => actions.push(Action::Quit),
                _ => self.form.focus = Some("list".to_owned()),
            }
        }
        if menu_press == Some(Press::Row) {
            self.form.focus = Some("list".to_owned());
        }

        let mut close = false;
        cx.keys = &keys;
        if let Some(overlay) = &mut self.overlay
            && let Some(action) = overlay.show(&mut cx)
        {
            close = true;
            if !matches!(action, Action::Pop) {
                actions.push(action);
            }
        }
        let (entry, wifi, mut send, bt_send, sound_send, screens_send) =
            (cx.entry, cx.wifi, cx.send, cx.bt, cx.sound, cx.screens);
        self.entry = entry;
        if wifi != self.snap.wifi_enabled {
            log::info!("switching Wi-Fi {}", if wifi { "on" } else { "off" });
            self.snap.wifi_enabled = wifi;
            send.push(Request::SetWifi(wifi));
        }
        if close {
            self.overlay = None;
        }
        for request in send {
            self.backend.send(request);
        }
        for request in bt_send {
            self.bt.send(request);
        }
        for request in sound_send {
            self.sound_backend.send(request);
        }
        for request in screens_send {
            self.screens_backend.send(request);
        }
        for action in actions {
            self.apply(&ctx, action);
        }
        if !self.loaded && self.page == Page::Network {
            ui.put(
                Rect::from_center_size(page_rect.center(), vec2(300.0, 20.0)),
                egui::Label::new(RichText::new("Reading NetworkManager…").color(self.pal.dim)),
            );
        }
    }
}
