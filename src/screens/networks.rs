use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Frame, Margin, Rect, RichText, Stroke, Ui, UiBuilder, pos2, vec2};

use super::editor::{Editor, Find};
use super::overlay::Overlay;
use super::{Action, Cx};
use crate::finder::{Finder, Source};
use crate::form::{self, Form, Icon, Press, Row};
use crate::nm::{AccessPoint, Connection, Kind, Request, Settings, Snapshot, Station};

const REFRESH: Duration = Duration::from_secs(5);
const TRAFFIC: Duration = Duration::from_secs(1);
const EDITOR_WIDTH: f32 = 600.0;
const GAP: f32 = 20.0;
const LABEL: f32 = 170.0;
const PADDING: i8 = 16;

pub struct Networks {
    selected: Option<String>,
    refreshed: Option<Instant>,
    /// A rescan was asked for and no snapshot has come back yet.
    pub scanning: bool,
    /// The row key the editor pane was last opened for; a new selection replaces it.
    followed: Option<String>,
    editor: Option<Box<Editor>>,
    /// Loads in the background while `editor` stays on screen, so moving through the
    /// list doesn't flash "Loading…".
    pending: Option<Box<Editor>>,
    /// Byte counters per connected device, sampled every second for the rates.
    traffic: HashMap<String, Traffic>,
    /// The device the edited connection is active on, and what it is linked to.
    details_of: Option<String>,
    details: Option<Result<Vec<(&'static str, String)>, String>>,
    finder: Option<Finder>,
}

struct Traffic {
    at: Instant,
    rx: u64,
    tx: u64,
    down: f64,
    up: f64,
}

struct Line {
    header: Option<String>,
    key: String,
    text: String,
    signal: Option<u8>,
    available: bool,
    active: Option<String>,
    /// The device the active connection runs on.
    device: Option<String>,
    profile: Option<Connection>,
    join: Option<Join>,
    station: Option<Station>,
}

struct Join {
    ssid: String,
    device: String,
    secured: bool,
}

impl Networks {
    pub fn new() -> Self {
        Self {
            selected: None,
            refreshed: None,
            scanning: false,
            followed: None,
            editor: None,
            pending: None,
            traffic: HashMap::new(),
            details_of: None,
            details: None,
            finder: None,
        }
    }

    /// The page was left: stop the find sound.
    pub fn closed(&mut self) {
        self.finder = None;
    }

    /// Reopens the editor for the selected row, dropping unsaved edits.
    pub fn reset_editor(&mut self) {
        self.followed = None;
    }

    pub fn refreshed(&mut self, snap: &Snapshot) {
        let edited = self.editor.as_ref().and_then(|editor| editor.uuid());
        if edited.is_some_and(|uuid| !snap.connections.iter().any(|c| c.uuid == uuid)) {
            log::info!("reopening the editor: its connection is gone");
            self.reset_editor();
        }
    }

    pub fn loaded(&mut self, result: Result<Box<Settings>, String>) -> Result<(), String> {
        let Some(pending) = &mut self.pending else {
            return Ok(());
        };
        match result {
            Ok(settings) if settings.uuid.as_deref() == pending.uuid() => {
                pending.loaded(*settings);
                self.editor = self.pending.take();
            }
            Ok(settings) => log::debug!("ignoring stale load of {:?}", settings.uuid),
            Err(err) => {
                self.pending = None;
                self.editor = None;
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn details_arrived(
        &mut self,
        device: String,
        rows: Result<Vec<(&'static str, String)>, String>,
    ) {
        if self.details_of.as_ref() == Some(&device) {
            self.details = Some(rows);
        }
    }

    pub fn show(&mut self, cx: &mut Cx<'_>, form: &mut Form<'_>, rect: Rect) -> Option<Action> {
        let rescan = self.refreshed.is_none();
        let mut due = self.refreshed.is_none_or(|at| at.elapsed() >= REFRESH);
        if due {
            self.refreshed = Some(Instant::now());
            self.scanning |= rescan;
            cx.send.push(Request::Refresh { rescan });
        }
        cx.ctx.request_repaint_after(REFRESH);

        let live = self
            .editor
            .as_ref()
            .and_then(|editor| editor.uuid())
            .and_then(|uuid| {
                cx.snap
                    .connections
                    .iter()
                    .find(|c| c.uuid == uuid && c.active && !c.device.is_empty())
            });
        if live.map(|c| &c.device) != self.details_of.as_ref() {
            self.details_of = live.map(|c| c.device.clone());
            self.details = None;
            due = true;
        }
        if let Some(connection) = live
            && due
        {
            cx.send.push(Request::Details {
                device: connection.device.clone(),
                wifi: connection.kind == Kind::Wifi,
            });
        }

        let lines = lines(cx.snap);
        let connected: Vec<&str> = lines.iter().filter_map(|l| l.device.as_deref()).collect();
        self.traffic
            .retain(|device, _| connected.contains(&device.as_str()));
        for device in &connected {
            sample_traffic(&mut self.traffic, device);
        }
        if !connected.is_empty() {
            cx.ctx.request_repaint_after(TRAFFIC);
        }
        let details: Vec<Option<String>> = lines
            .iter()
            .map(|line| {
                let signal = line.signal.map_or(String::new(), |s| format!("{s} %"));
                let Some(device) = line.device.as_deref() else {
                    return line.signal.is_some().then_some(signal);
                };
                let ip = cx
                    .snap
                    .devices
                    .iter()
                    .find(|d| d.name == device)
                    .map_or("", |d| d.ip.as_str());
                let rates = self.traffic.get(device).map_or(String::new(), |t| {
                    format!("↓ {}  ↑ {}", rate(t.down), rate(t.up))
                });
                Some(format!("{ip}   {rates}   {signal}").trim().to_owned())
            })
            .collect();
        let rows: Vec<Row<'_>> = lines
            .iter()
            .zip(&details)
            .map(|(line, detail)| match &line.header {
                Some(header) => Row::Header(header),
                None => Row::Item {
                    text: &line.text,
                    detail: detail.as_deref(),
                    signal: line.signal,
                    switch: line.available.then_some(line.active.is_some()),
                },
            })
            .collect();
        let mut selected = self
            .selected
            .as_ref()
            .and_then(|key| {
                lines
                    .iter()
                    .position(|l| l.header.is_none() && &l.key == key)
            })
            .unwrap_or(1);

        let top = form.top_bar(
            cx.ctx,
            self.scanning,
            &[
                ("radar", Icon::Radar, "Wi-Fi channels"),
                ("reload", Icon::Reload, "Search for networks"),
                ("add", Icon::Plus, "Add a connection"),
                ("net-nix", Icon::Save, "Save to NixOS"),
            ],
            Some(("wifi", "Wi-Fi", &mut cx.wifi)),
        );
        if let Some(finder) = &mut self.finder {
            finder.poll();
        }
        let shown = self
            .selected
            .as_ref()
            .and_then(|key| lines.iter().find(|l| l.header.is_none() && &l.key == key));
        let station = shown.and_then(|line| line.station.as_ref());
        let station_finding = station.and_then(|_| {
            let finder = self.finder.as_ref()?;
            (Some(&finder.key) == self.selected.as_ref())
                .then(|| (finder.text(), finder.error.clone()))
        });
        let find = shown
            .and_then(|line| Some((line, line.join.as_ref()?)))
            .filter(|_| cx.wifi)
            .map(|(line, join)| Find {
                finder: &mut self.finder,
                key: &line.key,
                device: &join.device,
                ssid: &join.ssid,
            });
        let mut press = None;
        let mut edited = None;
        let mut station_find = false;
        let live = self.details_of.as_deref().zip(self.details.as_ref());
        let editor = self.editor.as_mut().or(self.pending.as_mut());
        let (snap, pal) = (cx.snap, cx.pal);
        form::screen(cx.ctx, pal, "networks", "Networks", rect, |ui| {
            let area = ui.available_rect_before_wrap();
            if editor.is_none() && station.is_none() {
                press = form.list(ui, "list", &rows, &mut selected, area.size());
                return;
            }
            let split = area.right() - EDITOR_WIDTH;
            let list = Rect::from_min_max(area.min, pos2(split - GAP, area.bottom()));
            let pane = Rect::from_min_max(pos2(split, area.top()), area.max);
            ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
                press = form.list(ui, "list", &rows, &mut selected, list.size());
            });
            ui.scope_builder(UiBuilder::new().max_rect(pane), |ui| {
                let mark = form.mark();
                let frame = Frame::new()
                    .stroke(egui::Stroke::new(1.0, pal.outline))
                    .corner_radius(3)
                    .show(ui, |ui| {
                        let inner = pane.size() - vec2(2.0, 2.0);
                        ui.set_min_size(inner);
                        ui.set_max_width(inner.x);
                        match (editor, station) {
                            (Some(editor), _) => edited = editor.show(ui, form, snap, live, find),
                            (None, Some(station)) => {
                                station_find =
                                    station_pane(ui, form, station, station_finding.as_ref());
                            }
                            (None, None) => {}
                        }
                    });
                // The focus is only known once the editor's widgets are drawn.
                if form.focus_since(mark) {
                    ui.painter().rect_stroke(
                        frame.response.rect,
                        3,
                        egui::Stroke::new(1.0, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            });
        });
        if let Some(station) = station.filter(|_| station_find) {
            self.finder = match station_finding {
                Some(_) => None,
                None => {
                    log::info!("finding hotspot device {} by sound", station.mac);
                    let source = Source::Station {
                        device: station.device.clone(),
                        mac: station.mac.clone(),
                        ip: station.ip.clone(),
                    };
                    let key = format!("st:{}:{}", station.device, station.mac);
                    Some(Finder::start(cx.ctx.clone(), key, source))
                }
            };
        }
        let line = lines.get(selected).filter(|l| l.header.is_none());
        self.selected = line.map(|l| l.key.clone());
        if self
            .finder
            .as_ref()
            .is_some_and(|finder| !cx.wifi || self.selected.as_ref() != Some(&finder.key))
        {
            self.finder = None;
        }
        if self.followed != self.selected {
            self.followed = self.selected.clone();
            self.pending = None;
            let profile = line.and_then(|l| l.profile.as_ref());
            let join = line.and_then(|l| l.join.as_ref());
            if let Some(profile) = profile {
                log::debug!("editor follows '{}'", profile.name);
                cx.send.push(Request::Load(profile.uuid.clone()));
                self.pending = Some(Box::new(Editor::load(profile.uuid.clone())));
            } else if let Some(join) = join {
                log::debug!("editor follows new network '{}'", join.ssid);
                self.editor = Some(Box::new(Editor::join(&join.ssid, join.secured)));
            } else {
                self.editor = None;
            }
        }
        match edited {
            Some(Action::Pop) => {
                self.reset_editor();
                return None;
            }
            Some(action) => return Some(action),
            None => {}
        }
        if top == Some(0) {
            log::debug!("channel radar");
            return Some(Action::Overlay(Overlay::radar()));
        }
        if top == Some(1) {
            log::info!("rescan asked for");
            self.refreshed = Some(Instant::now());
            self.scanning = true;
            cx.send.push(Request::Refresh { rescan: true });
            return None;
        }
        if top == Some(2) {
            log::debug!("new connection");
            self.pending = None;
            self.editor = Some(Box::new(Editor::create(cx.snap)));
            return None;
        }
        if top == Some(3) {
            let uuids: Vec<String> = cx
                .snap
                .connections
                .iter()
                .map(|connection| connection.uuid.clone())
                .collect();
            log::info!("writing the NixOS module for {} profiles", uuids.len());
            cx.send.push(Request::Export(uuids));
            return None;
        }
        let line = line?;
        let press = match press? {
            Press::Row if line.available => Press::Toggle,
            press => press,
        };
        log::debug!("{press:?} on '{}'", line.text);
        match press {
            Press::Row => {
                let newest = self.pending.as_mut().or(self.editor.as_mut())?;
                newest.focus_on("name");
                None
            }
            Press::Toggle => {
                if let Some(uuid) = &line.active {
                    return Some(Action::Send(Request::Down(uuid.clone())));
                }
                if let Some(profile) = &line.profile {
                    return Some(Action::Send(Request::Up(profile.uuid.clone())));
                }
                let join = line.join.as_ref()?;
                if join.secured {
                    return Some(Action::Overlay(Overlay::password(
                        join.ssid.clone(),
                        join.device.clone(),
                    )));
                }
                Some(Action::Send(Request::Connect {
                    ssid: join.ssid.clone(),
                    password: None,
                    device: join.device.clone(),
                }))
            }
        }
    }
}

/// A device on our hotspot: its name and Find button, then what is known of
/// it. True when the button was pressed.
fn station_pane(
    ui: &mut Ui,
    form: &mut Form<'_>,
    station: &Station,
    finding: Option<&(String, Option<String>)>,
) -> bool {
    let pal = form.pal().clone();
    let header = Margin {
        bottom: 10,
        ..Margin::same(PADDING)
    };
    let mut pressed = false;
    Frame::new().inner_margin(header).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(station.name())
                    .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            ui.add_space((ui.available_width() - form::FIELD_HEIGHT).max(0.0));
            let (icon, tip) = match finding {
                Some(_) => (Icon::Stop, "Stop the sound"),
                None => (Icon::Find, "Find by sound"),
            };
            pressed = form.icon_button(ui, "station-find", icon, tip);
        });
    });
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));
    Frame::new().inner_margin(PADDING).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        if let Some(err) = finding.and_then(|(_, error)| error.as_ref()) {
            ui.label(RichText::new(err).color(pal.danger));
        }
        let signal = match finding {
            Some((text, _)) => text.clone(),
            None => format!("{:.0} dBm", station.signal),
        };
        let connected = station.connected_secs;
        let mut rows = vec![
            ("Signal", signal),
            ("MAC address", station.mac.clone()),
            ("IPv4 address", station.ip.clone()),
            ("Hostname", station.hostname.clone()),
            (
                "Connected for",
                format!("{}:{:02}", connected / 60, connected % 60),
            ),
            ("Hotspot", station.device.clone()),
        ];
        rows.retain(|(_, value)| !value.is_empty());
        for (label, value) in rows {
            ui.horizontal(|ui| {
                form::label(ui, &pal, label, LABEL);
                ui.label(RichText::new(value).color(pal.text));
            });
        }
    });
    pressed
}

// Reading two small sysfs files is cheap enough for the UI thread, and it keeps
// the rates live between the 5 s snapshots.
fn sample_traffic(traffic: &mut HashMap<String, Traffic>, device: &str) {
    let now = Instant::now();
    // The repaint asked for a second ago can come a little early.
    if traffic
        .get(device)
        .is_some_and(|t| now - t.at < TRAFFIC * 9 / 10)
    {
        return;
    }
    let read = |counter: &str| {
        std::fs::read_to_string(format!("/sys/class/net/{device}/statistics/{counter}"))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    };
    let (Some(rx), Some(tx)) = (read("rx_bytes"), read("tx_bytes")) else {
        log::debug!("no traffic counters for '{device}'");
        traffic.remove(device);
        return;
    };
    let (down, up) = match traffic.get(device) {
        Some(last) => {
            let seconds = (now - last.at).as_secs_f64();
            (
                rx.saturating_sub(last.rx) as f64 / seconds,
                tx.saturating_sub(last.tx) as f64 / seconds,
            )
        }
        None => (0.0, 0.0),
    };
    traffic.insert(
        device.to_owned(),
        Traffic {
            at: now,
            rx,
            tx,
            down,
            up,
        },
    );
}

fn rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1e6 {
        format!("{:.1} MB/s", bytes_per_second / 1e6)
    } else {
        format!("{:.0} kB/s", bytes_per_second / 1e3)
    }
}

fn lines(snap: &Snapshot) -> Vec<Line> {
    let header = |text: &str| Line {
        header: Some(text.to_owned()),
        key: String::new(),
        text: String::new(),
        signal: None,
        available: false,
        active: None,
        device: None,
        profile: None,
        join: None,
        station: None,
    };
    let saved = |c: &Connection| Line {
        header: None,
        key: format!("c:{}", c.uuid),
        text: c.name.clone(),
        signal: None,
        available: c.active,
        active: c.active.then(|| c.uuid.clone()),
        device: c.active.then(|| c.device.clone()),
        profile: Some(c.clone()),
        join: None,
        station: None,
    };
    let mut lines = Vec::new();
    let mut shown: Vec<&str> = Vec::new();

    for device in snap.devices.iter().filter(|d| d.kind == "wifi") {
        let mut networks: Vec<&AccessPoint> = Vec::new();
        for ap in snap
            .access_points
            .iter()
            .filter(|ap| ap.device == device.name && !ap.ssid.is_empty())
        {
            match networks.iter_mut().find(|n| n.ssid == ap.ssid) {
                Some(seen) if ap.in_use || (!seen.in_use && ap.signal > seen.signal) => *seen = ap,
                Some(_) => {}
                None => networks.push(ap),
            }
        }
        if networks.is_empty() {
            continue;
        }
        networks.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
        lines.push(header(&format!("Wi-Fi ({})", device.name)));
        for ap in networks {
            let profiles: Vec<&Connection> = snap
                .connections
                .iter()
                .filter(|c| c.kind == Kind::Wifi && c.ssid == ap.ssid)
                .collect();
            let active = profiles
                .iter()
                .find(|c| c.active && c.device == device.name)
                .copied();
            shown.extend(profiles.iter().map(|c| c.uuid.as_str()));
            lines.push(Line {
                header: None,
                key: format!("ap:{}:{}", device.name, ap.ssid),
                text: ap.ssid.clone(),
                signal: Some(ap.signal),
                available: true,
                active: active.map(|c| c.uuid.clone()),
                device: active.map(|_| device.name.clone()),
                profile: active.or(profiles.first().copied()).cloned(),
                join: Some(Join {
                    ssid: ap.ssid.clone(),
                    device: device.name.clone(),
                    secured: ap.secured(),
                }),
                station: None,
            });
        }
    }

    let mut stations: Vec<&Station> = snap.stations.iter().collect();
    stations.sort_by(|a, b| a.device.cmp(&b.device).then(a.name().cmp(b.name())));
    for (index, station) in stations.iter().enumerate() {
        if index == 0 || stations[index - 1].device != station.device {
            lines.push(header(&format!("Hotspot devices ({})", station.device)));
        }
        lines.push(Line {
            header: None,
            key: format!("st:{}:{}", station.device, station.mac),
            text: station.name().to_owned(),
            signal: Some(station.percent()),
            available: false,
            active: None,
            device: None,
            profile: None,
            join: None,
            station: Some((*station).clone()),
        });
    }

    let mut wifi: Vec<&Connection> = snap
        .connections
        .iter()
        .filter(|c| c.kind == Kind::Wifi && !shown.contains(&c.uuid.as_str()))
        .collect();
    let mut wired: Vec<&Connection> = snap
        .connections
        .iter()
        .filter(|c| c.kind == Kind::Ethernet)
        .collect();
    let mut others: Vec<&Connection> = snap
        .connections
        .iter()
        .filter(|c| matches!(&c.kind, Kind::Other(kind) if !matches!(kind.as_str(), "loopback" | "wifi-p2p")))
        .collect();
    // A cable counts as plugged in unless NetworkManager says the port is unavailable.
    let cable = snap
        .devices
        .iter()
        .any(|d| d.kind == "ethernet" && !matches!(d.state.as_str(), "unavailable" | "unmanaged"));
    wifi.sort_by_key(|c| c.name.to_lowercase());
    wired.sort_by_key(|c| c.name.to_lowercase());
    others.sort_by_key(|c| (c.kind.label().to_owned(), c.name.to_lowercase()));
    for (title, list, available) in [("Saved networks", wifi, false), ("Ethernet", wired, cable)] {
        if list.is_empty() {
            continue;
        }
        lines.push(header(title));
        for connection in list {
            let mut line = saved(connection);
            line.available |= available;
            lines.push(line);
        }
    }
    for (index, connection) in others.iter().enumerate() {
        let label = connection.kind.label();
        if index == 0 || others[index - 1].kind.label() != label {
            lines.push(header(label));
        }
        let mut line = saved(connection);
        line.available = true;
        lines.push(line);
    }
    lines
}
