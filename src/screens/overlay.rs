use std::time::{Duration, Instant};

use egui::RichText;

use super::graph::Graph;
use super::{Action, Cx, radar};
use crate::bt;
use crate::finder::{Finder, Source};
use crate::form::{self, Form, FormState, Icon, Press, Row, Sort, Table};
use crate::lan;
use crate::nm::{Beacon, Connection, Request};
use crate::pw;

const BEACONS: Duration = Duration::from_secs(5);
/// Often enough that links made elsewhere show up while you look.
const GRAPH: Duration = Duration::from_secs(1);
/// iwd often scans one band at a time and the kernel then forgets the other,
/// so an access point stays on the radar this long after it was last seen.
const KEEP_BEACONS: Duration = Duration::from_secs(60);

pub struct Overlay {
    form: FormState,
    kind: OverlayKind,
}

enum OverlayKind {
    Message {
        title: String,
        text: String,
    },
    ConfirmDelete {
        uuid: String,
        name: String,
    },
    ConfirmRemove {
        address: String,
        name: String,
    },
    Password {
        ssid: String,
        device: String,
        password: String,
        show: bool,
    },
    Wait(String),
    Radar {
        /// Each access point with when it was last seen.
        beacons: Option<Result<Vec<(Instant, Beacon)>, String>>,
        /// Set while the beacons come from nmcli because iw failed.
        iw_error: Option<String>,
        asked: Option<Instant>,
    },
    Devices {
        title: String,
        device: String,
        /// Boxed to keep `Action` small.
        scan: Box<lan::Scan>,
        /// Beeping for one of the devices in the list, when the radio has a
        /// signal for it. Boxed to keep `Action` small.
        finder: Option<Box<Finder>>,
        /// `None` keeps the order the devices answered in.
        sort: Option<Sort>,
        /// The MAC address of the selected device.
        selected: Option<String>,
    },
    Graph {
        snap: Option<Box<Result<pw::Snapshot, String>>>,
        /// The last link change that failed, until the next one works.
        error: Option<String>,
        asked: Option<Instant>,
        graph: Box<Graph>,
    },
}

impl Overlay {
    fn with(focus: &str, kind: OverlayKind) -> Self {
        Self {
            form: FormState::focused(focus),
            kind,
        }
    }

    pub fn message(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self::with(
            "ok",
            OverlayKind::Message {
                title: title.into(),
                text: text.into(),
            },
        )
    }

    pub fn confirm_delete(connection: &Connection) -> Self {
        Self::with(
            "cancel",
            OverlayKind::ConfirmDelete {
                uuid: connection.uuid.clone(),
                name: connection.name.clone(),
            },
        )
    }

    pub fn confirm_remove(address: &str, name: &str) -> Self {
        Self::with(
            "cancel",
            OverlayKind::ConfirmRemove {
                address: address.to_owned(),
                name: name.to_owned(),
            },
        )
    }

    pub fn password(ssid: String, device: String) -> Self {
        Self::with(
            "password",
            OverlayKind::Password {
                ssid,
                device,
                password: String::new(),
                show: false,
            },
        )
    }

    pub fn wait(text: impl Into<String>) -> Self {
        Self::with("", OverlayKind::Wait(text.into()))
    }

    pub fn radar() -> Self {
        Self::with(
            "close",
            OverlayKind::Radar {
                beacons: None,
                iw_error: None,
                asked: None,
            },
        )
    }

    pub fn devices(ctx: egui::Context, title: String, device: String) -> Self {
        let scan = Box::new(lan::Scan::start(ctx, device.clone()));
        Self::with(
            "close",
            OverlayKind::Devices {
                title,
                device,
                scan,
                finder: None,
                sort: None,
                selected: None,
            },
        )
    }

    pub fn graph() -> Self {
        Self::with(
            "graph",
            OverlayKind::Graph {
                snap: None,
                error: None,
                asked: None,
                graph: Box::default(),
            },
        )
    }

    pub fn sound(&mut self, result: &Result<pw::Snapshot, String>) {
        if let OverlayKind::Graph { snap, .. } = &mut self.kind {
            *snap = Some(Box::new(result.clone()));
        }
    }

    /// Whether the result was for this popup's links.
    pub fn sound_done(&mut self, result: &Result<(), String>) -> bool {
        let OverlayKind::Graph { error, .. } = &mut self.kind else {
            return false;
        };
        *error = result.clone().err();
        true
    }

    pub fn beacons(&mut self, result: Result<Vec<Beacon>, String>, error: Option<String>) {
        let OverlayKind::Radar {
            beacons, iw_error, ..
        } = &mut self.kind
        else {
            return;
        };
        *iw_error = error;
        let fresh = match result {
            Ok(fresh) => fresh,
            Err(err) => {
                *beacons = Some(Err(err));
                return;
            }
        };
        let mut kept = match beacons.take() {
            Some(Ok(kept)) => kept,
            _ => Vec::new(),
        };
        kept.retain(|(seen, old)| {
            seen.elapsed() < KEEP_BEACONS && !fresh.iter().any(|b| b.bssid == old.bssid)
        });
        let now = Instant::now();
        kept.extend(fresh.into_iter().map(|beacon| (now, beacon)));
        *beacons = Some(Ok(kept));
    }

    pub fn is_wait(&self) -> bool {
        matches!(self.kind, OverlayKind::Wait(_))
    }

    pub fn show(&mut self, cx: &mut Cx<'_>) -> Option<Action> {
        let (pal, snap) = (cx.pal, cx.snap);
        let mut form = Form::new(&mut self.form, cx.keys, pal);
        let mut action = None;
        match &mut self.kind {
            OverlayKind::Message { title, text } => {
                form::popup(cx.ctx, pal, "message", title, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(text.as_str());
                    ui.add_space(12.0);
                    if form::button_row(&mut form, ui, &[("ok", "OK")]).is_some() {
                        action = Some(Action::Pop);
                    }
                });
            }
            OverlayKind::ConfirmDelete { uuid, name } => {
                form::popup(cx.ctx, pal, "delete", "Delete Connection", |ui| {
                    ui.set_max_width(360.0);
                    ui.label(format!(
                        "Are you sure you want to delete the connection '{name}'?"
                    ));
                    ui.add_space(12.0);
                    action = match form::button_row(
                        &mut form,
                        ui,
                        &[("cancel", "Cancel"), ("delete", "Delete")],
                    ) {
                        Some(0) => Some(Action::Pop),
                        Some(_) => Some(Action::Send(Request::Delete(uuid.clone()))),
                        None => None,
                    };
                });
            }
            OverlayKind::ConfirmRemove { address, name } => {
                form::popup(cx.ctx, pal, "remove", "Remove Device", |ui| {
                    ui.set_max_width(360.0);
                    ui.label(format!(
                        "Remove '{name}'? It has to be paired again before it can connect."
                    ));
                    ui.add_space(12.0);
                    action = match form::button_row(
                        &mut form,
                        ui,
                        &[("cancel", "Cancel"), ("remove", "Remove")],
                    ) {
                        Some(0) => Some(Action::Pop),
                        Some(_) => Some(Action::Bluetooth(bt::Request::Remove {
                            address: address.clone(),
                            name: name.clone(),
                        })),
                        None => None,
                    };
                });
            }
            OverlayKind::Password {
                ssid,
                device,
                password,
                show,
            } => {
                form::popup(
                    cx.ctx,
                    pal,
                    "password",
                    "Authentication required by wireless network",
                    |ui| {
                        ui.set_max_width(440.0);
                        ui.label(format!(
                        "Passwords or encryption keys are required to access the wireless network '{ssid}'."
                    ));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            form::label(ui, pal, "Password", 80.0);
                            form.entry(ui, "password", password, 280.0, !*show);
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(80.0 + ui.spacing().item_spacing.x);
                            form.check(ui, "show", "Show password", show);
                        });
                        ui.add_space(12.0);
                        action = match form::button_row(
                            &mut form,
                            ui,
                            &[("cancel", "Cancel"), ("ok", "OK")],
                        ) {
                            Some(0) => Some(Action::Pop),
                            Some(_) => Some(Action::Send(Request::Connect {
                                ssid: ssid.clone(),
                                password: Some(password.clone()),
                                device: device.clone(),
                            })),
                            None => None,
                        };
                    },
                );
            }
            OverlayKind::Radar {
                beacons,
                iw_error,
                asked,
            } => {
                if asked.is_none_or(|at| at.elapsed() >= BEACONS) {
                    *asked = Some(Instant::now());
                    let devices = snap.devices.iter().filter(|d| d.kind == "wifi");
                    cx.send
                        .push(Request::Beacons(devices.map(|d| d.name.clone()).collect()));
                }
                cx.ctx.request_repaint_after(BEACONS);
                form::popup(cx.ctx, pal, "radar", "Wi-Fi channels", |ui| {
                    if let Some(err) = iw_error {
                        ui.set_max_width(760.0);
                        ui.label(
                            RichText::new(format!(
                                "{err}. The channels come from nmcli instead; with iwd they are wrong for networks you are not connected to."
                            ))
                            .color(pal.warning),
                        );
                        ui.add_space(8.0);
                    }
                    match beacons {
                        None => {
                            ui.label(RichText::new("Reading the scan results…").color(pal.dim));
                        }
                        Some(Err(err)) => {
                            ui.label(RichText::new(err.as_str()).color(pal.danger));
                        }
                        Some(Ok(beacons)) => {
                            radar::chart(ui, pal, beacons.iter().map(|(_, b)| b).collect());
                        }
                    }
                    ui.add_space(12.0);
                    if form::button_row(&mut form, ui, &[("close", "Close")]).is_some() {
                        action = Some(Action::Pop);
                    }
                });
            }
            OverlayKind::Devices {
                title,
                device,
                scan,
                finder,
                sort,
                selected,
            } => {
                scan.poll();
                if let Some(finder) = finder.as_mut() {
                    finder.poll();
                }
                let height = (cx.ctx.content_rect().height() - 240.0).max(160.0);
                let mut find = None;
                let mut rescan = false;
                form::popup(cx.ctx, pal, "devices", title, |ui| {
                    let width = (cx.ctx.content_rect().width() - 200.0).clamp(860.0, 1400.0);
                    ui.set_min_width(width);
                    match &scan.found {
                        None => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().color(pal.accent));
                                ui.label(RichText::new("Looking for devices…").color(pal.dim));
                            });
                        }
                        Some(Err(err)) => {
                            ui.label(RichText::new(err.as_str()).color(pal.danger));
                        }
                        Some(Ok(found)) => {
                            ui.horizontal(|ui| {
                                let count = match found.hosts.len() {
                                    1 => "1 device".to_owned(),
                                    n => format!("{n} devices"),
                                };
                                let on = if found.subnet.is_empty() {
                                    device.clone()
                                } else {
                                    format!("{} ({device})", found.subnet)
                                };
                                ui.label(RichText::new(format!("{count} on {on}")).color(pal.dim));
                                if !found.done {
                                    ui.add(egui::Spinner::new().color(pal.accent));
                                }
                            });
                            ui.add_space(6.0);
                            let mut hosts: Vec<&lan::Host> = found.hosts.iter().collect();
                            if let Some(sort) = *sort {
                                sort_hosts(&mut hosts, sort);
                            }
                            let cells: Vec<[String; 3]> = hosts
                                .iter()
                                .map(|host| {
                                    let ping =
                                        host.ping.map_or(String::new(), |ms| format!("{ms:.0} ms"));
                                    [host.ipv4.clone(), host.vendor.clone(), ping]
                                })
                                .collect();
                            let links: Vec<[Option<String>; 2]> = hosts
                                .iter()
                                .map(|host| [name_url(host), form::web_url(&host.ipv4)])
                                .collect();
                            let rows: Vec<Row<'_>> = hosts
                                .iter()
                                .zip(&cells)
                                .zip(&links)
                                .map(|((host, cells), links)| Row::Item {
                                    text: match host.hostname.as_str() {
                                        "" => "unknown",
                                        name => name,
                                    },
                                    detail: role(host),
                                    signal: None,
                                    switch: None,
                                    tree: &[],
                                    columns: cells,
                                    links,
                                })
                                .collect();
                            let mut index = selected
                                .as_ref()
                                .and_then(|mac| hosts.iter().position(|h| &h.mac == mac))
                                .unwrap_or(0);
                            let wanted =
                                form::HEADS_HEIGHT + rows.len() as f32 * form::ROW_HEIGHT + 12.0;
                            let size =
                                egui::vec2(ui.available_width(), wanted.max(360.0).min(height));
                            let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                            let split = area.right() - HOST_PANE;
                            let list = egui::Rect::from_min_max(
                                area.min,
                                egui::pos2(split - 20.0, area.bottom()),
                            );
                            let pane =
                                egui::Rect::from_min_max(egui::pos2(split, area.top()), area.max);
                            ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
                                let press =
                                    form.table(ui, "hosts", &HOSTS, sort, &rows, &mut index);
                                let host = hosts.get(index);
                                *selected = host.map(|host| host.mac.clone());
                                if press == Some(Press::Row)
                                    && let Some(host) = host.filter(|host| host.radio)
                                {
                                    find = Some((host.mac.clone(), host.ipv4.clone()));
                                }
                            });
                            if let Some(host) = hosts.get(index) {
                                let found = finder.as_deref().filter(|f| f.key == host.mac);
                                ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
                                    if host_pane(ui, &mut form, pane, host, found) {
                                        find = Some((host.mac.clone(), host.ipv4.clone()));
                                    }
                                });
                            }
                        }
                    }
                    ui.add_space(12.0);
                    let buttons = [("rescan", "Scan again"), ("close", "Close")];
                    match form::button_row(&mut form, ui, &buttons).map(|i| buttons[i].0) {
                        Some("rescan") => rescan = true,
                        Some(_) => action = Some(Action::Pop),
                        None => {}
                    }
                });
                if let Some((mac, ip)) = find {
                    *finder = match finder.as_ref().filter(|f| f.key == mac) {
                        Some(_) => None,
                        None => {
                            log::info!("finding device {mac} on {device} by sound");
                            let source = Source::Station {
                                device: device.clone(),
                                mac: mac.clone(),
                                ip,
                            };
                            Some(Box::new(Finder::start(cx.ctx.clone(), mac, source)))
                        }
                    };
                }
                if rescan {
                    **scan = lan::Scan::start(cx.ctx.clone(), device.clone());
                    *finder = None;
                }
            }
            OverlayKind::Graph {
                snap,
                error,
                asked,
                graph,
            } => {
                if asked.is_none_or(|at| at.elapsed() >= GRAPH) {
                    *asked = Some(Instant::now());
                    cx.sound.push(pw::Request::Refresh);
                }
                cx.ctx.request_repaint_after(GRAPH);
                let size = cx.ctx.content_rect().size() - egui::vec2(160.0, 140.0);
                form::popup(cx.ctx, pal, "graph", "Node graph", |ui| {
                    ui.set_width(size.x);
                    if let Some(err) = error {
                        ui.label(RichText::new(err.as_str()).color(pal.danger));
                    }
                    if let Some(err) = graph.meter_error() {
                        ui.label(
                            RichText::new(format!("No signal levels: {err}")).color(pal.warning),
                        );
                    }
                    ui.add_space(8.0);
                    let mut doomed = Vec::new();
                    let height = (size.y - 120.0).max(200.0);
                    ui.allocate_ui(egui::vec2(size.x, height), |ui| {
                        ui.set_min_size(egui::vec2(size.x, height));
                        match snap.as_deref() {
                            None => {
                                ui.label(RichText::new("Reading PipeWire…").color(pal.dim));
                            }
                            Some(Err(err)) => {
                                ui.label(RichText::new(err.as_str()).color(pal.danger));
                            }
                            Some(Ok(snap)) => {
                                if let Some(request) = graph.show(ui, &mut form, snap) {
                                    *error = None;
                                    cx.sound.push(request);
                                }
                                doomed = graph.doomed(snap);
                            }
                        }
                    });
                    ui.add_space(12.0);
                    let buttons: &[(&str, &str)] = if doomed.is_empty() {
                        &[("close", "Close")]
                    } else {
                        &[("disconnect", "Disconnect"), ("close", "Close")]
                    };
                    match form::button_row(&mut form, ui, buttons) {
                        Some(0) if !doomed.is_empty() => {
                            *error = None;
                            cx.sound.push(pw::Request::Unlink(doomed));
                        }
                        Some(_) => action = Some(Action::Pop),
                        None => {}
                    }
                });
            }
            OverlayKind::Wait(text) => {
                form::popup(cx.ctx, pal, "wait", "Please wait", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().color(pal.accent));
                        ui.label(RichText::new(text.as_str()));
                    });
                });
            }
        }
        let escape = form.escape();
        cx.entry = form.finish(cx.ctx);
        if escape
            && let OverlayKind::Graph { graph, .. } = &mut self.kind
            && graph.cancel()
        {
            return action;
        }
        if escape && !self.is_wait() {
            return Some(Action::Pop);
        }
        action
    }
}

const HOSTS: Table = Table {
    name: "Name",
    columns: &[("Address", 150.0), ("Manufacturer", 220.0), ("Ping", 80.0)],
};
const HOST_PANE: f32 = 460.0;
const HOST_LABEL: f32 = 120.0;

fn name_url(host: &lan::Host) -> Option<String> {
    (!host.hostname.is_empty()).then(|| format!("http://{}/", host.hostname))
}

fn role(host: &lan::Host) -> Option<&'static str> {
    match (host.router, host.me) {
        (true, _) => Some("router"),
        (_, true) => Some("this computer"),
        _ => None,
    }
}

/// Everything about the selected device. Returns whether Find by sound (or
/// its stop) was pressed.
fn host_pane(
    ui: &mut egui::Ui,
    form: &mut Form<'_>,
    pane: egui::Rect,
    host: &lan::Host,
    finding: Option<&Finder>,
) -> bool {
    let pal = form.pal().clone();
    let mut pressed = false;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, pal.outline))
        .corner_radius(3)
        .show(ui, |ui| {
            let inner = pane.size() - egui::vec2(2.0, 2.0);
            ui.set_min_size(inner);
            ui.set_max_width(inner.x);
            let header = egui::Margin {
                bottom: 10,
                ..egui::Margin::same(16)
            };
            egui::Frame::new().inner_margin(header).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let name = match host.hostname.as_str() {
                        "" => "unknown",
                        name => name,
                    };
                    ui.label(
                        RichText::new(name)
                            .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                            .color(pal.text),
                    );
                    if host.radio {
                        ui.add_space((ui.available_width() - form::FIELD_HEIGHT).max(0.0));
                        let (icon, tip) = match finding {
                            Some(_) => (Icon::Stop, "Stop the sound"),
                            None => (Icon::Find, "Find by sound"),
                        };
                        pressed = form.icon_button(ui, "host-find", icon, tip);
                    }
                });
            });
            let y = ui.cursor().top();
            ui.painter().hline(
                ui.max_rect().x_range(),
                y,
                egui::Stroke::new(1.0, pal.outline),
            );
            egui::Frame::new().inner_margin(16).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(err) = finding.and_then(|f| f.error.as_deref()) {
                    ui.label(RichText::new(err).color(pal.danger));
                }
                let field = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(HOST_LABEL, 0.0),
                            egui::Layout::right_to_left(egui::Align::Min),
                            |ui| ui.label(RichText::new(label).color(pal.secondary)),
                        );
                        ui.vertical(add);
                    });
                };
                let text = |value: &str| RichText::new(value).color(pal.text);
                if let Some(role) = role(host) {
                    field(ui, "Role", &mut |ui| {
                        ui.label(text(role));
                    });
                }
                if let Some(url) = name_url(host) {
                    field(ui, "Hostname", &mut |ui| {
                        form::link(ui, &pal, RichText::new(&host.hostname), &url)
                    });
                }
                if !host.ipv4.is_empty() {
                    field(
                        ui,
                        "IPv4 address",
                        &mut |ui| match form::web_url(&host.ipv4) {
                            Some(url) => form::link(ui, &pal, RichText::new(&host.ipv4), &url),
                            None => {
                                ui.label(text(&host.ipv4));
                            }
                        },
                    );
                }
                if !host.ipv6.is_empty() {
                    field(ui, "IPv6 addresses", &mut |ui| {
                        for ip in &host.ipv6 {
                            match form::web_url(ip) {
                                Some(url) => form::link(ui, &pal, RichText::new(ip), &url),
                                None => {
                                    ui.label(text(ip));
                                }
                            }
                        }
                    });
                }
                let mut rows = vec![
                    ("MAC address", host.mac.clone()),
                    ("Manufacturer", host.vendor.clone()),
                    (
                        "Ping",
                        host.ping.map_or(String::new(), |ms| format!("{ms:.0} ms")),
                    ),
                ];
                if host.radio {
                    let signal = finding.map_or("in range".to_owned(), |f| f.text());
                    rows.push(("Signal", signal));
                }
                for (label, value) in rows.into_iter().filter(|(_, v)| !v.is_empty()) {
                    field(ui, label, &mut |ui| {
                        ui.add(egui::Label::new(text(&value)).wrap());
                    });
                }
            });
        });
    pressed
}

fn sort_hosts(hosts: &mut [&lan::Host], sort: Sort) {
    let ipv4 = |host: &lan::Host| host.ipv4.parse::<std::net::Ipv4Addr>().ok();
    let ping = |host: &lan::Host| host.ping.unwrap_or(-1.0);
    hosts.sort_by(|a, b| {
        let order = match sort.column {
            0 => form::by_text(&a.hostname, &b.hostname),
            1 => ipv4(a).cmp(&ipv4(b)),
            2 => form::by_text(&a.vendor, &b.vendor),
            _ => ping(a).total_cmp(&ping(b)),
        };
        if sort.descending {
            order.reverse()
        } else {
            order
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_sort_by_address_as_numbers_and_by_ping() {
        let host = |ipv4: &str, ping| lan::Host {
            ipv4: ipv4.to_owned(),
            ping,
            ..lan::Host::default()
        };
        let all = [
            host("192.168.1.10", Some(3.0)),
            host("192.168.1.9", None),
            host("192.168.1.100", Some(12.0)),
        ];
        let shown = |column, descending| {
            let mut hosts: Vec<&lan::Host> = all.iter().collect();
            sort_hosts(&mut hosts, Sort { column, descending });
            hosts.iter().map(|h| h.ipv4.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(
            shown(1, false),
            ["192.168.1.9", "192.168.1.10", "192.168.1.100"]
        );
        assert_eq!(
            shown(3, true),
            ["192.168.1.100", "192.168.1.10", "192.168.1.9"]
        );
    }
}
