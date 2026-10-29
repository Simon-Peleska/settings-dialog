use std::time::{Duration, Instant};

use egui::{Frame, Margin, Rect, RichText, Stroke, Ui, UiBuilder, pos2, vec2};

use super::overlay::Overlay;
use super::{Action, Cx};
use crate::bt::{self, Device, Request, Snapshot, manufacturer_name, service_name, signal_percent};
use crate::finder::{Finder, Source};
use crate::form::{self, Form, Icon, Press, Row};
use crate::theme::Palette;

/// Often enough to watch the signal move while discovery runs.
const REFRESH: Duration = Duration::from_secs(2);
/// How long to wait for a discovery that was asked for before asking again.
const RETRY_SCAN: Duration = Duration::from_secs(5);
const PANE_WIDTH: f32 = 600.0;
const GAP: f32 = 20.0;
const LABEL: f32 = 170.0;
const PADDING: i8 = 16;

pub struct Bluetooth {
    snap: Option<Result<Snapshot, String>>,
    scan_error: Option<String>,
    selected: Option<String>,
    refreshed: Option<Instant>,
    scan_asked: Option<Instant>,
    finder: Option<Finder>,
}

struct Line {
    header: Option<String>,
    address: String,
    text: String,
    detail: String,
    signal: Option<u8>,
    switch: Option<bool>,
}

impl Bluetooth {
    pub fn new() -> Self {
        Self {
            snap: None,
            scan_error: None,
            selected: None,
            refreshed: None,
            scan_asked: None,
            finder: None,
        }
    }

    /// The screen was left: stop the find sound.
    pub fn closed(&mut self) {
        self.finder = None;
    }

    /// The screen was switched to: read the devices straight away.
    pub fn opened(&mut self) {
        self.refreshed = None;
        self.scan_asked = None;
    }

    pub fn snapshot(&mut self, snap: Result<Snapshot, String>) {
        self.snap = Some(snap);
    }

    pub fn scanned(&mut self, result: Result<(), String>) {
        self.scan_error = result.err();
    }

    /// Shows the switch's new position until the next snapshot confirms it.
    pub fn powered(&mut self, on: bool) {
        if let Some(Ok(Snapshot {
            adapter: Some(adapter),
            ..
        })) = &mut self.snap
        {
            adapter.powered = on;
        }
    }

    pub fn show(&mut self, cx: &mut Cx<'_>, form: &mut Form<'_>, rect: Rect) -> Option<Action> {
        if self.refreshed.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.refreshed = Some(Instant::now());
            cx.bt.push(Request::Refresh);
        }
        cx.ctx.request_repaint_after(REFRESH);

        let snap = match &self.snap {
            Some(Ok(snap)) => Some(snap),
            _ => None,
        };
        let adapter = snap.and_then(|snap| snap.adapter.as_ref());
        let powered = adapter.is_some_and(|a| a.powered);
        let discovering = adapter.is_some_and(|a| a.discovering);
        // Signal strengths only exist while discovering, so it runs the whole
        // time the screen is open, like in GNOME's settings.
        if powered && !discovering && self.scan_asked.is_none_or(|at| at.elapsed() >= RETRY_SCAN) {
            self.scan_asked = Some(Instant::now());
            cx.bt.push(Request::Scan);
        }

        let lines = snap.map(|snap| lines(snap, powered)).unwrap_or_default();
        let rows: Vec<Row<'_>> = lines
            .iter()
            .map(|line| match &line.header {
                Some(header) => Row::Header(header),
                None => Row::Item {
                    text: &line.text,
                    detail: Some(&line.detail)
                        .filter(|d| !d.is_empty())
                        .map(|d| d.as_str()),
                    signal: line.signal,
                    switch: line.switch,
                },
            })
            .collect();
        let mut selected = self
            .selected
            .as_ref()
            .and_then(|address| {
                lines
                    .iter()
                    .position(|l| l.header.is_none() && &l.address == address)
            })
            .unwrap_or(1);

        let mut on = powered;
        let switch = adapter.map(|_| ("bluetooth", "Bluetooth", &mut on));
        let top = form.top_bar(
            cx.ctx,
            discovering,
            &[("bt-nix", Icon::Save, "Save to NixOS")],
            switch,
        );
        if top == Some(0) {
            self.scan_error = crate::nix::write("bluetooth.nix", &crate::nix::bluetooth(powered))
                .err()
                .or_else(super::not_imported);
        }

        let status = match (&self.snap, adapter) {
            (None, _) => Some(("Reading BlueZ…".to_owned(), cx.pal.dim)),
            (Some(Err(err)), _) => Some((err.clone(), cx.pal.danger)),
            (Some(Ok(_)), None) => Some(("No Bluetooth adapter found.".to_owned(), cx.pal.dim)),
            (Some(Ok(_)), Some(adapter)) if !adapter.powered => {
                Some(("Bluetooth is off.".to_owned(), cx.pal.dim))
            }
            _ => self.scan_error.clone().map(|err| (err, cx.pal.danger)),
        };
        let device = lines
            .get(selected)
            .filter(|l| l.header.is_none())
            .and_then(|l| snap?.devices.iter().find(|d| d.address == l.address));
        if let Some(finder) = &mut self.finder {
            finder.poll();
        }
        if self.finder.as_ref().is_some_and(|finder| {
            !powered || device.is_none_or(|device| device.address != finder.key)
        }) {
            self.finder = None;
        }
        let pal = cx.pal;
        let mut press = None;
        let mut pane_action = None;
        form::screen(cx.ctx, pal, "bluetooth", "Bluetooth", rect, |ui| {
            if let Some((text, color)) = status {
                ui.label(RichText::new(text).color(color));
                ui.add_space(8.0);
            }
            let area = ui.available_rect_before_wrap();
            let Some(device) = device else {
                press = form.list(ui, "list", &rows, &mut selected, area.size());
                return;
            };
            let split = area.right() - PANE_WIDTH;
            let list = Rect::from_min_max(area.min, pos2(split - GAP, area.bottom()));
            let pane = Rect::from_min_max(pos2(split, area.top()), area.max);
            ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
                press = form.list(ui, "list", &rows, &mut selected, list.size());
            });
            ui.scope_builder(UiBuilder::new().max_rect(pane), |ui| {
                let mark = form.mark();
                let frame = Frame::new()
                    .stroke(Stroke::new(1.0, pal.outline))
                    .corner_radius(3)
                    .show(ui, |ui| {
                        let inner = pane.size() - vec2(2.0, 2.0);
                        ui.set_min_size(inner);
                        ui.set_max_width(inner.x);
                        pane_action = details(ui, form, device, powered, &mut self.finder);
                    });
                if form.focus_since(mark) {
                    ui.painter().rect_stroke(
                        frame.response.rect,
                        3,
                        Stroke::new(1.0, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            });
        });

        let line = lines.get(selected).filter(|l| l.header.is_none());
        self.selected = line.map(|l| l.address.clone());
        if on != powered {
            log::info!("switching Bluetooth {}", if on { "on" } else { "off" });
            self.powered(on);
            cx.bt.push(Request::Power(on));
            return None;
        }
        if pane_action.is_some() {
            return pane_action;
        }
        let device = device?;
        match press? {
            Press::Row if line?.switch.is_some() => Some(toggle(device)),
            Press::Row => None,
            Press::Toggle => Some(toggle(device)),
        }
    }
}

fn toggle(device: &Device) -> Action {
    let address = device.address.clone();
    let name = device.alias.clone();
    log::debug!("toggling '{name}'");
    Action::Bluetooth(if device.connected {
        Request::Disconnect { address, name }
    } else if device.paired {
        Request::Connect { address, name }
    } else {
        Request::Pair { address, name }
    })
}

fn lines(snap: &Snapshot, powered: bool) -> Vec<Line> {
    let header = |text: String| Line {
        header: Some(text),
        address: String::new(),
        text: String::new(),
        detail: String::new(),
        signal: None,
        switch: None,
    };
    let line = |device: &Device| {
        let mut detail = Vec::new();
        if let Some(battery) = device.battery {
            detail.push(format!("battery {battery} %"));
        }
        if let Some(rssi) = device.rssi {
            detail.push(format!("{rssi} dBm"));
        }
        Line {
            header: None,
            address: device.address.clone(),
            text: device.alias.clone(),
            detail: detail.join("   "),
            signal: device.rssi.map(signal_percent),
            switch: powered.then_some(device.connected),
        }
    };

    let mut paired: Vec<&Device> = snap.devices.iter().filter(|d| d.paired).collect();
    paired.sort_by_key(|d| (!d.connected, d.alias.to_lowercase()));
    let strongest_first = |devices: &mut Vec<&Device>| {
        devices.sort_by_key(|d| {
            (
                std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)),
                d.alias.to_lowercase(),
            )
        })
    };
    // Most of what discovery finds nearby is nameless LE beacons, so they get
    // their own section below the named ones.
    let (mut nearby, mut unnamed): (Vec<&Device>, Vec<&Device>) = snap
        .devices
        .iter()
        .filter(|d| !d.paired)
        .partition(|d| d.name.is_some());
    strongest_first(&mut nearby);
    strongest_first(&mut unnamed);

    let mut lines = Vec::new();
    if !paired.is_empty() {
        lines.push(header("My devices".to_owned()));
        lines.extend(paired.into_iter().map(line));
    }
    if !nearby.is_empty() {
        lines.push(header("Nearby devices".to_owned()));
        lines.extend(nearby.into_iter().map(line));
    }
    if !unnamed.is_empty() {
        lines.push(header("Nearby devices without a name".to_owned()));
        lines.extend(unnamed.into_iter().map(line));
    }
    lines
}

fn details(
    ui: &mut Ui,
    form: &mut Form<'_>,
    device: &Device,
    powered: bool,
    finder: &mut Option<Finder>,
) -> Option<Action> {
    let pal = form.pal().clone();
    let finding = finder.as_ref().filter(|f| f.key == device.address);
    let (action_icon, action_tip) = if device.connected {
        (Icon::Unplug, "Disconnect")
    } else if device.paired {
        (Icon::Plug, "Connect")
    } else {
        (Icon::Pair, "Pair and connect")
    };
    let buttons = f32::from(2 * u8::from(powered) + u8::from(device.paired));
    let (mut find, mut pressed, mut remove) = (false, false, false);
    let header = Margin {
        bottom: 10,
        ..Margin::same(PADDING)
    };
    Frame::new().inner_margin(header).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&device.alias)
                    .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            let width = buttons * form::FIELD_HEIGHT
                + (buttons - 1.0).max(0.0) * ui.spacing().item_spacing.x;
            ui.add_space((ui.available_width() - width).max(0.0));
            if powered {
                let (icon, tip) = match finding {
                    Some(_) => (Icon::Stop, "Stop the sound"),
                    None => (Icon::Find, "Find by sound"),
                };
                find = form.icon_button(ui, "bt-find", icon, tip);
                pressed = form.icon_button(ui, "bt-action", action_icon, action_tip);
            }
            if device.paired {
                remove = form.icon_button(ui, "bt-remove", Icon::Trash, "Remove device");
            }
        });
    });
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));

    let (mut trusted, mut blocked) = (device.trusted, device.blocked);
    let (mut trust, mut block) = (false, false);
    egui::ScrollArea::both()
        .id_salt("bluetooth-device")
        .max_height(ui.available_height().max(60.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new().inner_margin(PADDING).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(err) = finding.and_then(|f| f.error.as_ref()) {
                    ui.label(RichText::new(err).color(pal.danger));
                }
                heading(ui, &pal, "DEVICE");
                let status = match (device.connected, device.paired) {
                    (true, _) => "Connected",
                    (false, true) => "Paired, not connected",
                    (false, false) => "Not paired",
                };
                let signal = match (finding, device.rssi) {
                    (Some(finder), _) => finder.text(),
                    (None, Some(rssi)) => format!("{rssi} dBm"),
                    (None, None) => {
                        "unknown — measured only while the device advertises".to_owned()
                    }
                };
                let mut rows = vec![
                    ("Type", device.kind().to_owned()),
                    ("Status", status.to_owned()),
                    ("Signal", signal),
                    (
                        "Transmit power",
                        device
                            .tx_power
                            .map_or(String::new(), |p| format!("{p} dBm")),
                    ),
                    (
                        "Battery",
                        device.battery.map_or(String::new(), |b| format!("{b} %")),
                    ),
                    (
                        "Address",
                        format!("{} ({})", device.address, device.address_type),
                    ),
                    ("Device ID", device.device_id()),
                    (
                        "Manufacturer",
                        device
                            .manufacturers
                            .iter()
                            .map(|id| manufacturer_name(*id))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                ];
                rows.retain(|(_, value)| !value.is_empty());
                for (label, value) in rows {
                    // Top-aligned, so a label stays on the first line of a list.
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            vec2(LABEL, 0.0),
                            egui::Layout::right_to_left(egui::Align::Min),
                            |ui| ui.label(RichText::new(label).color(pal.secondary)),
                        );
                        ui.label(RichText::new(value).color(pal.text));
                    });
                }
                if device.paired {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL + ui.spacing().item_spacing.x);
                        trust = form.check(ui, "bt-trust", "Trusted", &mut trusted);
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL + ui.spacing().item_spacing.x);
                        block = form.check(ui, "bt-block", "Blocked", &mut blocked);
                    });
                }
                if !device.uuids.is_empty() {
                    ui.add_space(4.0);
                    heading(ui, &pal, "SERVICES");
                    for uuid in &device.uuids {
                        ui.horizontal(|ui| {
                            ui.add_space(LABEL + ui.spacing().item_spacing.x);
                            ui.label(RichText::new(service_name(uuid)).color(pal.text));
                        });
                    }
                }
            });
        });

    if find {
        *finder = match finding {
            Some(_) => None,
            None => {
                log::info!("finding '{}' by sound", device.alias);
                let source = Source::Bluetooth {
                    address: device.address.clone(),
                    path: device.path.clone(),
                    connected: device.connected,
                };
                Some(Finder::start(
                    ui.ctx().clone(),
                    device.address.clone(),
                    source,
                ))
            }
        };
    }
    let address = device.address.clone();
    if remove {
        return Some(Action::Overlay(Overlay::confirm_remove(
            &address,
            &device.alias,
        )));
    }
    if trust {
        log::info!("trust '{}': {trusted}", device.alias);
        return Some(Action::Bluetooth(bt::Request::Trust {
            address,
            on: trusted,
        }));
    }
    if block {
        log::info!("block '{}': {blocked}", device.alias);
        return Some(Action::Bluetooth(bt::Request::Block {
            address,
            on: blocked,
        }));
    }
    pressed.then(|| toggle(device))
}

fn heading(ui: &mut Ui, pal: &Palette, title: &str) {
    ui.label(
        RichText::new(title)
            .color(pal.info)
            .font(fastframe_fonts::Weight::SemiBold.font_id(13.0)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(
        alias: &str,
        paired: bool,
        connected: bool,
        rssi: Option<i16>,
        named: bool,
    ) -> Device {
        Device {
            address: alias.to_owned(),
            name: named.then(|| alias.to_owned()),
            alias: alias.to_owned(),
            paired,
            connected,
            rssi,
            ..Device::default()
        }
    }

    #[test]
    fn paired_devices_come_first_then_nearby_ones_by_signal() {
        let snap = Snapshot {
            adapter: None,
            devices: vec![
                device("Speaker", true, false, None, true),
                device("Weak", false, false, Some(-90), true),
                device("Headset", true, true, None, true),
                device("12-34", false, false, Some(-40), false),
                device("Strong", false, false, Some(-50), true),
            ],
        };
        let shown_lines = lines(&snap, true);
        let shown: Vec<(&str, Option<bool>)> = shown_lines
            .iter()
            .map(|l| (l.header.as_deref().unwrap_or(&l.text), l.switch))
            .collect();
        assert_eq!(
            shown,
            [
                ("My devices", None),
                ("Headset", Some(true)),
                ("Speaker", Some(false)),
                ("Nearby devices", None),
                ("Strong", Some(false)),
                ("Weak", Some(false)),
                ("Nearby devices without a name", None),
                ("12-34", Some(false)),
            ]
        );
        assert_eq!(shown_lines[4].detail, "-50 dBm");
        assert!(lines(&snap, false).iter().all(|l| l.switch.is_none()));
    }

    #[test]
    fn the_switch_pairs_connects_or_disconnects() {
        let request = |d: &Device| match toggle(d) {
            Action::Bluetooth(Request::Pair { .. }) => "pair",
            Action::Bluetooth(Request::Connect { .. }) => "connect",
            Action::Bluetooth(Request::Disconnect { .. }) => "disconnect",
            _ => "other",
        };
        assert_eq!(request(&device("a", false, false, None, true)), "pair");
        assert_eq!(request(&device("a", true, false, None, true)), "connect");
        assert_eq!(request(&device("a", true, true, None, true)), "disconnect");
    }
}
