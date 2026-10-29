use std::time::{Duration, Instant};

use egui::{Frame, Margin, Rect, RichText, Stroke, Ui, UiBuilder, pos2, vec2};

use super::overlay::Overlay;
use super::{Action, Cx};
use crate::form::{self, Form, Icon, Row};
use crate::pw::{self, Node, Request, Side, Snapshot};
use crate::theme::Palette;

const REFRESH: Duration = Duration::from_secs(2);
const PANE_WIDTH: f32 = 600.0;
const GAP: f32 = 20.0;
const LABEL: f32 = 170.0;
const PADDING: i8 = 16;
/// How long a volume just set wins over the snapshot, which lags behind it.
const SETTLE: Duration = Duration::from_secs(1);

pub struct Sound {
    snap: Option<Result<Snapshot, String>>,
    /// The last change that failed, until the next one works.
    error: Option<String>,
    /// None for streams, which are neither an output nor an input. The node
    /// name finds a Bluetooth device again after a codec switch renumbers it.
    selected: Option<(Option<Side>, u32, String)>,
    refreshed: Option<Instant>,
    /// The node, the volumes set and when.
    volume: Option<(u32, Vec<f32>, Instant)>,
}

struct Line {
    header: Option<&'static str>,
    side: Option<Side>,
    id: u32,
    text: String,
    detail: String,
}

impl Sound {
    pub fn new() -> Self {
        Self {
            snap: None,
            error: None,
            selected: None,
            refreshed: None,
            volume: None,
        }
    }

    pub fn opened(&mut self) {
        self.refreshed = None;
    }

    pub fn snapshot(&mut self, snap: Result<Snapshot, String>) {
        self.snap = Some(snap);
    }

    pub fn done(&mut self, result: Result<(), String>) {
        self.error = result.err();
    }

    pub fn show(&mut self, cx: &mut Cx<'_>, form: &mut Form<'_>, rect: Rect) -> Option<Action> {
        if self.refreshed.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.refreshed = Some(Instant::now());
            cx.sound.push(Request::Refresh);
        }
        cx.ctx.request_repaint_after(REFRESH);

        let snap = match &self.snap {
            Some(Ok(snap)) => Some(snap),
            _ => None,
        };
        let lines = snap.map(lines).unwrap_or_default();
        let rows: Vec<Row<'_>> = lines
            .iter()
            .map(|line| match line.header {
                Some(header) => Row::Header(header),
                None => Row::Item {
                    text: &line.text,
                    detail: Some(line.detail.as_str()).filter(|d| !d.is_empty()),
                    signal: None,
                    switch: None,
                },
            })
            .collect();
        let mut selected = self
            .selected
            .as_ref()
            .and_then(|(side, id, name)| {
                let item = |l: &Line| l.header.is_none() && l.side == *side;
                let named = |l: &Line| {
                    snap.and_then(|s| s.node(l.id))
                        .is_some_and(|n| &n.name == name)
                };
                (lines.iter().position(|l| item(l) && l.id == *id))
                    .or_else(|| lines.iter().position(|l| item(l) && named(l)))
            })
            .unwrap_or(1);

        let top = form.top_bar(
            cx.ctx,
            false,
            &[
                ("graph", Icon::Graph, "Node graph"),
                ("sound-nix", Icon::Save, "Save to NixOS"),
            ],
            None,
        );
        if top == Some(1) {
            let defaults =
                snap.map(|snap| (snap.default_sink.clone(), snap.default_source.clone()));
            let (sink, source) = defaults.unwrap_or_default();
            self.error = crate::nix::write(
                "sound.nix",
                &crate::nix::sound(sink.as_deref(), source.as_deref()),
            )
            .err()
            .or_else(super::not_imported);
        }

        let missing = |default: &Option<String>, what: &str| {
            let name = default.as_ref()?;
            let snap = snap?;
            (!snap.nodes.iter().any(|n| &n.name == name))
                .then(|| format!("The default {what} '{name}' is not connected."))
        };
        let status = match &self.snap {
            None => Some(("Reading PipeWire…".to_owned(), cx.pal.dim)),
            Some(Err(err)) => Some((err.clone(), cx.pal.danger)),
            Some(Ok(snap)) => self
                .error
                .clone()
                .map(|err| (err, cx.pal.danger))
                .or_else(|| {
                    missing(&snap.default_sink, "output")
                        .or_else(|| missing(&snap.default_source, "input"))
                        .map(|text| (text, cx.pal.dim))
                }),
        };
        let line = lines.get(selected).filter(|l| l.header.is_none());
        let node = line.and_then(|l| snap?.node(l.id));
        let volumes = node.map_or(Vec::new(), |node| {
            let set = self
                .volume
                .as_ref()
                .filter(|(id, _, at)| *id == node.id && at.elapsed() < SETTLE)
                .map(|(_, volumes, _)| volumes.clone());
            set.unwrap_or_else(|| node.volumes.clone())
        });
        let pal = cx.pal;
        let mut press = None;
        let mut pane_request = None;
        form::screen(cx.ctx, pal, "sound", "Sound", rect, |ui| {
            if let Some((text, color)) = status {
                ui.label(RichText::new(text).color(color));
                ui.add_space(8.0);
            }
            let area = ui.available_rect_before_wrap();
            let (Some(node), Some(line), Some(snap)) = (node, line, snap) else {
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
                        pane_request = details(ui, form, snap, node, line.side, &volumes);
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
        self.selected = line.map(|l| {
            (
                l.side,
                l.id,
                node.map(|n| n.name.clone()).unwrap_or_default(),
            )
        });
        if top == Some(0) {
            return Some(Action::Overlay(Overlay::graph()));
        }
        if let Some(request) = pane_request {
            self.error = None;
            if let Request::Volume { id, volumes, .. } = &request {
                self.volume = Some((*id, volumes.clone(), Instant::now()));
                cx.ctx.request_repaint_after(SETTLE);
            }
            cx.sound.push(request);
            return None;
        }
        if press.is_some()
            && let (Some(node), Some(side), Some(snap)) = (node, line.and_then(|l| l.side), snap)
            && !snap.is_default(node, side)
        {
            self.error = None;
            cx.sound.push(make_default(node));
        }
        None
    }
}

fn make_default(node: &Node) -> Request {
    Request::MakeDefault {
        id: node.id,
        name: node.name.clone(),
    }
}

/// "42 %" or "muted", for nodes with a volume.
fn loudness(node: &Node) -> Option<String> {
    let volume = node.volume()?;
    Some(if node.muted {
        "muted".to_owned()
    } else {
        format!("{:.0} %", volume * 100.0)
    })
}

fn lines(snap: &Snapshot) -> Vec<Line> {
    let mut lines = Vec::new();
    let sections = [
        ("Outputs", Some(Side::Output)),
        ("Inputs", Some(Side::Input)),
        ("Streams", None),
    ];
    for (header, side) in sections {
        let mut nodes: Vec<&Node> = snap
            .nodes
            .iter()
            .filter(|n| match side {
                Some(Side::Output) => n.is_output(),
                Some(Side::Input) => n.is_input(),
                None => n.volume().is_some() && !n.is_output() && !n.is_input(),
            })
            .collect();
        if nodes.is_empty() {
            continue;
        }
        nodes.sort_by_key(|n| n.label().to_lowercase());
        lines.push(Line {
            header: Some(header),
            side,
            id: 0,
            text: String::new(),
            detail: String::new(),
        });
        lines.extend(nodes.into_iter().map(|node| {
            let current = side.is_some_and(|side| snap.is_current(node, side));
            let default = side.is_some_and(|side| snap.is_default(node, side));
            let marks = [
                current.then(|| "current".to_owned()),
                default.then(|| "default".to_owned()),
                loudness(node),
            ];
            Line {
                header: None,
                side,
                id: node.id,
                text: label(node),
                detail: marks.into_iter().flatten().collect::<Vec<_>>().join(", "),
            }
        }));
    }
    lines
}

fn details(
    ui: &mut Ui,
    form: &mut Form<'_>,
    snap: &Snapshot,
    node: &Node,
    side: Option<Side>,
    volumes: &[f32],
) -> Option<Request> {
    let pal = form.pal().clone();
    // A stream can't be the default, so it never gets those buttons.
    let current = side.is_none_or(|side| snap.is_current(node, side));
    let default = side.is_none_or(|side| snap.is_default(node, side));
    let buttons = f32::from(u8::from(!current) + u8::from(!default));
    let (mut use_now, mut make) = (false, false);
    let header = Margin {
        bottom: 10,
        ..Margin::same(PADDING)
    };
    Frame::new().inner_margin(header).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(label(node))
                    .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            let width = buttons * form::FIELD_HEIGHT
                + (buttons - 1.0).max(0.0) * ui.spacing().item_spacing.x;
            ui.add_space((ui.available_width() - width).max(0.0));
            if !current {
                use_now = form.icon_button(ui, "sound-now", Icon::Speaker, "Use now");
            }
            if !default {
                make = form.icon_button(ui, "sound-default", Icon::Star, "Make default");
            }
        });
    });
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));

    let mut request = None;
    egui::ScrollArea::vertical()
        .id_salt("sound-node")
        .max_height(ui.available_height().max(60.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new().inner_margin(PADDING).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let set = |volumes: Vec<f32>| Request::Volume {
                    id: node.id,
                    route: node.route,
                    volumes,
                };
                if let Some(mut volume) = volumes.iter().copied().reduce(f32::max) {
                    heading(ui, &pal, "VOLUME");
                    ui.horizontal(|ui| {
                        form::label(ui, &pal, "Volume", LABEL);
                        if form.slider(ui, "sound-volume", &mut volume, 300.0) {
                            request = Some(set(pw::balanced(volumes, volume)));
                        }
                        ui.label(RichText::new(format!("{:.0} %", volume * 100.0)).color(pal.text));
                    });
                    let channels = node.volume_channels.iter().zip(volumes);
                    for (index, (channel, volume)) in channels.enumerate() {
                        if volumes.len() < 2 {
                            break;
                        }
                        let mut volume = *volume;
                        ui.horizontal(|ui| {
                            form::label(ui, &pal, pw::channel_name(channel), LABEL);
                            let id = format!("sound-channel-{index}");
                            if form.slider(ui, &id, &mut volume, 300.0) {
                                let mut volumes = volumes.to_vec();
                                volumes[index] = volume;
                                request = Some(set(volumes));
                            }
                            ui.label(
                                RichText::new(format!("{:.0} %", volume * 100.0)).color(pal.text),
                            );
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL + ui.spacing().item_spacing.x);
                        let mut muted = node.muted;
                        if form.check(ui, "sound-mute", "Muted", &mut muted) {
                            request = Some(Request::Mute { id: node.id, muted });
                        }
                    });
                    ui.add_space(4.0);
                }
                heading(ui, &pal, if side.is_some() { "DEVICE" } else { "STREAM" });
                let status = match (side, current, default) {
                    (None, ..) => "",
                    (_, true, true) => "Current and default",
                    (_, true, false) => "Current, not the default",
                    (_, false, true) => "Default, but not in use",
                    (_, false, false) => "Not in use",
                };
                let kind = match (node.is_output(), node.is_input(), node.class.as_str()) {
                    (true, true, _) => "Output and input",
                    (true, false, _) => "Output",
                    (false, true, _) => "Input",
                    (.., "Stream/Output/Audio") => "Playback",
                    (.., "Stream/Input/Audio") => "Recording",
                    (.., class) => class,
                };
                let driver = match node.api.as_str() {
                    "alsa" => "ALSA",
                    "bluez5" => "Bluetooth",
                    "" | "dsp" => "PipeWire (virtual)",
                    other => other,
                };
                let device = snap.devices.iter().find(|d| Some(d.id) == node.device);
                if let Some(device) = device.filter(|d| !d.codecs.is_empty()) {
                    let labels: Vec<&str> =
                        device.codecs.iter().map(|c| c.label.as_str()).collect();
                    let current = device
                        .codecs
                        .iter()
                        .position(|c| Some(c.profile) == device.profile);
                    let mut chosen = current.unwrap_or(0);
                    ui.horizontal(|ui| {
                        form::label(ui, &pal, "Codec", LABEL);
                        if form.select(ui, "sound-codec", &mut chosen, &labels, 300.0)
                            && Some(chosen) != current
                        {
                            request = Some(Request::Codec {
                                device: device.id,
                                profile: device.codecs[chosen].profile,
                            });
                        }
                    });
                }
                let rate = node
                    .rate
                    .map(|rate| format!("{} kHz", rate as f32 / 1000.0));
                let rows = [
                    ("Status", status.to_owned()),
                    ("State", node.state.clone()),
                    ("Kind", kind.to_owned()),
                    ("Driver", driver.to_owned()),
                    ("Channels", node.channels.clone()),
                    ("Sample rate", rate.unwrap_or_default()),
                    ("Allowed rates", node.rates.clone()),
                    ("Node name", node.name.clone()),
                    ("Node ID", node.id.to_string()),
                ];
                for (label, value) in rows.iter().filter(|(_, v)| !v.is_empty()) {
                    row(ui, &pal, label, value);
                }

                let mut peers: Vec<String> = Vec::new();
                for link in &snap.links {
                    let peer = if link.input_node == node.id {
                        snap.node(link.output_node)
                            .map(|n| format!("from {}", label(n)))
                    } else if link.output_node == node.id {
                        snap.node(link.input_node)
                            .map(|n| format!("to {}", label(n)))
                    } else {
                        None
                    };
                    if let Some(peer) = peer.filter(|p| !peers.contains(p)) {
                        peers.push(peer);
                    }
                }
                if !peers.is_empty() {
                    ui.add_space(4.0);
                    heading(ui, &pal, "CONNECTED");
                    for peer in peers {
                        row(ui, &pal, "", &peer);
                    }
                }
            });
        });

    if let (true, Some(side)) = (use_now, side) {
        return Some(Request::UseNow {
            name: node.name.clone(),
            side,
        });
    }
    if make {
        return Some(make_default(node));
    }
    request
}

/// An app's stream also names what it plays, like "Firefox: Video".
fn label(node: &Node) -> String {
    if node.is_stream() && !node.media_name.is_empty() && node.media_name != node.label() {
        format!("{}: {}", node.label(), node.media_name)
    } else {
        node.label().to_owned()
    }
}

fn row(ui: &mut Ui, pal: &Palette, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL, 0.0),
            egui::Layout::right_to_left(egui::Align::Min),
            |ui| ui.label(RichText::new(label).color(pal.secondary)),
        );
        ui.label(RichText::new(value).color(pal.text));
    });
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

    fn node(id: u32, name: &str, class: &str) -> Node {
        Node {
            id,
            name: name.to_owned(),
            description: name.to_owned(),
            class: class.to_owned(),
            ..Node::default()
        }
    }

    #[test]
    fn outputs_inputs_and_streams_marked_current_default_and_volume() {
        let snap = Snapshot {
            nodes: vec![
                node(1, "Speakers", "Audio/Sink"),
                node(2, "Headset", "Audio/Duplex"),
                node(3, "Mic", "Audio/Source"),
                Node {
                    app: "Firefox".to_owned(),
                    media_name: "Video".to_owned(),
                    volumes: vec![0.42],
                    ..node(4, "Firefox", "Stream/Output/Audio")
                },
                Node {
                    volumes: vec![0.0],
                    muted: true,
                    ..node(5, "Bot", "Stream/Input/Audio")
                },
                node(6, "Camera", "Video/Source"),
            ],
            current_sink: Some("Speakers".to_owned()),
            default_sink: Some("Headset".to_owned()),
            current_source: Some("Mic".to_owned()),
            default_source: Some("Mic".to_owned()),
            ..Snapshot::default()
        };
        let shown: Vec<(String, String)> = lines(&snap)
            .into_iter()
            .map(|l| (l.header.map_or(l.text, str::to_owned), l.detail))
            .collect();
        let shown: Vec<(&str, &str)> = shown
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Outputs", ""),
                ("Headset", "default"),
                ("Speakers", "current"),
                ("Inputs", ""),
                ("Headset", ""),
                ("Mic", "current, default"),
                ("Streams", ""),
                ("Bot", "muted"),
                ("Firefox: Video", "42 %"),
            ]
        );
    }
}
