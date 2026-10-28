use std::time::{Duration, Instant};

use egui::RichText;

use super::graph::Graph;
use super::{Action, Cx, radar};
use crate::bt;
use crate::form::{self, Form, FormState};
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
        scan: lan::Scan,
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
        let scan = lan::Scan::start(ctx, device.clone());
        Self::with(
            "close",
            OverlayKind::Devices {
                title,
                device,
                scan,
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
            } => {
                scan.poll();
                let height = (cx.ctx.content_rect().height() - 240.0).max(160.0);
                form::popup(cx.ctx, pal, "devices", title, |ui| {
                    ui.set_min_width(860.0);
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
                            egui::ScrollArea::vertical()
                                .max_height(height)
                                .show(ui, |ui| hosts(ui, pal, &found.hosts));
                        }
                    }
                    ui.add_space(12.0);
                    match form::button_row(
                        &mut form,
                        ui,
                        &[("rescan", "Scan again"), ("close", "Close")],
                    ) {
                        Some(0) => *scan = lan::Scan::start(cx.ctx.clone(), device.clone()),
                        Some(_) => action = Some(Action::Pop),
                        None => {}
                    }
                });
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

/// One row per device: name with its role, addresses (IPv6 dimmed under the
/// IPv4 one), MAC, manufacturer and ping.
fn hosts(ui: &mut egui::Ui, pal: &crate::theme::Palette, hosts: &[lan::Host]) {
    egui::Grid::new("lan_hosts")
        .num_columns(5)
        .spacing(egui::vec2(20.0, 8.0))
        .striped(false)
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for heading in ["Name", "Address", "MAC address", "Manufacturer", "Ping"] {
                ui.label(RichText::new(heading).color(pal.dim));
            }
            ui.end_row();
            for host in hosts {
                ui.vertical(|ui| {
                    match host.hostname.as_str() {
                        "" => {
                            ui.label(RichText::new("unknown").color(pal.dim));
                        }
                        name => {
                            let url = format!("http://{name}/");
                            form::link(ui, pal, RichText::new(name), &url);
                        }
                    }
                    let role = match (host.router, host.me) {
                        (true, _) => "router",
                        (_, true) => "this computer",
                        _ => "",
                    };
                    if !role.is_empty() {
                        ui.label(RichText::new(role).color(pal.info).small());
                    }
                });
                ui.vertical(|ui| {
                    if let Some(url) = form::web_url(&host.ipv4) {
                        form::link(ui, pal, RichText::new(&host.ipv4).monospace(), &url);
                    }
                    for ip in &host.ipv6 {
                        let text = RichText::new(ip).monospace().small();
                        match form::web_url(ip) {
                            Some(url) => form::link(ui, pal, text, &url),
                            None => {
                                ui.label(text.color(pal.dim));
                            }
                        }
                    }
                });
                ui.label(RichText::new(&host.mac).monospace());
                ui.label(&host.vendor);
                match host.ping {
                    Some(ms) => ui.label(format!("{ms:.0} ms")),
                    None => ui.label(""),
                };
                ui.end_row();
            }
        });
}
