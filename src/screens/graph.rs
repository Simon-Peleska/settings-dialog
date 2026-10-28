use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::epaint::CubicBezierShape;
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::form::{Form, Icon, Key};
use crate::pw::{self, Direction, Link, Media, Meter, Node, Request, Snapshot};
use crate::theme::Palette;

const NODE_WIDTH: f32 = 230.0;
const TITLE: f32 = 26.0;
const ROW: f32 = 18.0;
const NODE_GAP: f32 = 28.0;
/// Room around the nodes for the dots, which grow with the level, and the
/// links leaving them.
const MARGIN: f32 = 32.0;
const MIN_COLUMN_GAP: f32 = 80.0;
/// The volume strip at the bottom of nodes that have a volume.
const VOLUME: f32 = 22.0;
/// How long a volume just set wins over the snapshot, which lags behind it.
const SETTLE: Duration = Duration::from_secs(1);

/// The node graph: what was picked to link and where the keyboard is.
#[derive(Default)]
pub struct Graph {
    /// Where the keyboard is.
    cursor: Option<End>,
    /// The first end of a link being made, by click or Enter.
    picked: Option<End>,
    /// A link clicked on, for Disconnect.
    link: Option<u32>,
    dragging: Option<End>,
    /// The node, the volumes set and when.
    volume: Option<(u32, Vec<f32>, Instant)>,
    meter: Option<Meter>,
    /// The peak level per output port, falling back slowly.
    levels: HashMap<u32, f32>,
    meter_error: Option<String>,
}

/// What a link starts or ends at: one port, or all channels of one side of a
/// node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum End {
    Port(u32),
    Node(u32, Direction),
}

impl End {
    fn side(self, snap: &Snapshot) -> Option<(u32, Direction)> {
        match self {
            End::Port(id) => snap.port(id).map(|p| (p.node, p.direction)),
            End::Node(node, direction) => Some((node, direction)),
        }
    }

    fn touches(self, link: &Link) -> bool {
        match self {
            End::Port(id) => link.output_port == id || link.input_port == id,
            End::Node(node, Direction::Out) => link.output_node == node,
            End::Node(node, Direction::In) => link.input_node == node,
        }
    }
}

/// The pairs linking two ends: port to port, else all channels node to node.
fn pairs(snap: &Snapshot, a: End, b: End) -> Vec<(u32, u32)> {
    if let (End::Port(a), End::Port(b)) = (a, b) {
        return snap.linkable(a, b).into_iter().collect();
    }
    let (Some((a, side)), Some((b, _))) = (a.side(snap), b.side(snap)) else {
        return Vec::new();
    };
    if a == b {
        return Vec::new();
    }
    match side {
        Direction::Out => snap.link_nodes(a, b),
        Direction::In => snap.link_nodes(b, a),
    }
}

struct Spot {
    end: End,
    at: Pos2,
    /// Where it's hit: a port's row inside its node, holding the label.
    row: Rect,
}

struct Layout {
    nodes: Vec<(u32, Rect)>,
    /// In keyboard order: band by band, column by column, node by node, inputs
    /// before outputs.
    spots: Vec<Spot>,
    size: egui::Vec2,
}

/// Sources on the left, sinks on the right, everything with both in between.
/// A sink's monitor ports don't make it a source. Each media gets its own band,
/// audio on top, so MIDI devices don't sit in the way of the audio links.
fn layout(snap: &Snapshot, width: f32) -> Layout {
    let mut columns: [Vec<u32>; 3] = Default::default();
    for node in &snap.nodes {
        let ports: Vec<_> = snap.ports.iter().filter(|p| p.node == node.id).collect();
        if ports.is_empty() {
            continue;
        }
        let inputs = ports.iter().any(|p| p.direction == Direction::In);
        let outputs = ports
            .iter()
            .any(|p| p.direction == Direction::Out && !p.monitor);
        let column = match (inputs, outputs) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => 2,
        };
        columns[column].push(node.id);
    }
    let media = |node: u32| {
        snap.ports
            .iter()
            .filter(|p| p.node == node)
            .map(|p| p.media)
            .min()
            .unwrap_or(Media::Other)
    };
    for column in &mut columns {
        column.sort_by_cached_key(|id| {
            let label = snap.node(*id).map(|n| n.label().to_lowercase());
            (media(*id), label)
        });
    }
    let used: Vec<&Vec<u32>> = columns.iter().filter(|c| !c.is_empty()).collect();
    let gap = match used.len() {
        0 | 1 => 0.0,
        n => ((width - 2.0 * MARGIN - n as f32 * NODE_WIDTH) / (n - 1) as f32).max(MIN_COLUMN_GAP),
    };

    let mut nodes = Vec::new();
    let mut spots = Vec::new();
    let mut bands: Vec<Media> = used
        .iter()
        .flat_map(|c| c.iter().map(|id| media(*id)))
        .collect();
    bands.sort_unstable();
    bands.dedup();
    let mut band_top = MARGIN;
    for band in bands {
        let mut band_bottom = band_top;
        for (index, column) in used.iter().enumerate() {
            let left = MARGIN + index as f32 * (NODE_WIDTH + gap);
            let mut top = band_top;
            for id in column.iter().filter(|id| media(**id) == band) {
                let ports = |direction: Direction| {
                    snap.ports
                        .iter()
                        .filter(move |p| p.node == *id && p.direction == direction)
                };
                let rows = ports(Direction::In)
                    .count()
                    .max(ports(Direction::Out).count());
                let strip = snap.node(*id).map_or(0, strips) as f32 * VOLUME;
                let rect = Rect::from_min_size(
                    pos2(left, top),
                    vec2(NODE_WIDTH, TITLE + rows as f32 * ROW + 6.0 + strip),
                );
                for direction in [Direction::In, Direction::Out] {
                    let count = ports(direction).count();
                    if count > 1 {
                        let x = match direction {
                            Direction::In => rect.left(),
                            Direction::Out => rect.right(),
                        };
                        let at = pos2(x, rect.top() + TITLE / 2.0);
                        spots.push(Spot {
                            end: End::Node(*id, direction),
                            at,
                            row: Rect::from_center_size(at, vec2(14.0, 14.0)),
                        });
                    }
                    for (row, port) in ports(direction).enumerate() {
                        let y = rect.top() + TITLE + (row as f32 + 0.5) * ROW;
                        let (x, row_rect) = match direction {
                            Direction::In => (
                                rect.left(),
                                Rect::from_min_max(
                                    pos2(rect.left() - 6.0, y - ROW / 2.0),
                                    pos2(rect.center().x, y + ROW / 2.0),
                                ),
                            ),
                            Direction::Out => (
                                rect.right(),
                                Rect::from_min_max(
                                    pos2(rect.center().x, y - ROW / 2.0),
                                    pos2(rect.right() + 6.0, y + ROW / 2.0),
                                ),
                            ),
                        };
                        spots.push(Spot {
                            end: End::Port(port.id),
                            at: pos2(x, y),
                            row: row_rect,
                        });
                    }
                }
                nodes.push((*id, rect));
                top = rect.bottom() + NODE_GAP;
            }
            band_bottom = band_bottom.max(top);
        }
        band_top = band_bottom + NODE_GAP;
    }
    let height = (band_top - 2.0 * NODE_GAP + MARGIN).max(0.0);
    let right = nodes.iter().map(|(_, r)| r.right()).fold(0.0, f32::max);
    Layout {
        nodes,
        spots,
        size: vec2(right + MARGIN, height),
    }
}

/// Volume strips at the bottom of a node: the whole node, then each channel
/// when there's more than one.
fn strips(node: &Node) -> usize {
    match node.volumes.len() {
        0 => 0,
        1 => 1,
        channels => 1 + channels,
    }
}

fn media_color(pal: &Palette, media: Media) -> Color32 {
    match media {
        Media::Audio => pal.info,
        Media::Midi => pal.warning,
        Media::Video => pal.success,
        Media::Other => pal.dim,
    }
}

fn bezier(from: Pos2, to: Pos2) -> [Pos2; 4] {
    let pull = ((to.x - from.x).abs() / 2.0).max(40.0);
    [from, from + vec2(pull, 0.0), to - vec2(pull, 0.0), to]
}

fn near_curve(points: [Pos2; 4], pointer: Pos2) -> bool {
    let shape =
        CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, Stroke::NONE);
    (0..=24).any(|step| shape.sample(step as f32 / 24.0).distance(pointer) < 6.0)
}

impl Graph {
    /// The links Disconnect removes: the one clicked, else all of what the
    /// keyboard is on.
    pub fn doomed(&self, snap: &Snapshot) -> Vec<u32> {
        if let Some(link) = self
            .link
            .filter(|id| snap.links.iter().any(|l| l.id == *id))
        {
            return vec![link];
        }
        let Some(end) = self.cursor else {
            return Vec::new();
        };
        snap.links
            .iter()
            .filter(|l| end.touches(l))
            .map(|l| l.id)
            .collect()
    }

    /// Picks the first end of a link, or links it to the second.
    fn pick(&mut self, snap: &Snapshot, end: End) -> Option<Request> {
        self.link = None;
        let Some(first) = self.picked.take() else {
            self.picked = Some(end);
            return None;
        };
        if first == end {
            return None;
        }
        let pairs = pairs(snap, first, end);
        if pairs.is_empty() {
            log::debug!("{first:?} and {end:?} have nothing to link");
            self.picked = Some(end);
            return None;
        }
        Some(Request::Link(pairs))
    }

    /// Keeps a meter on every linked audio output and takes its readings.
    fn measure(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let mut ports: Vec<u32> = snap
            .links
            .iter()
            .map(|l| l.output_port)
            .filter(|p| snap.port(*p).is_some_and(|p| p.media == Media::Audio))
            .collect();
        ports.sort_unstable();
        ports.dedup();
        if self.meter.as_ref().map(|m| &m.ports) != Some(&ports) {
            log::debug!("metering ports {ports:?}");
            self.meter = None;
            self.meter_error = None;
            if !ports.is_empty() {
                self.meter = Some(Meter::start(ctx.clone(), ports));
            }
        }
        let Some(meter) = &self.meter else {
            return;
        };
        match meter.poll() {
            Some(Ok(peaks)) => {
                for (port, peak) in meter.ports.iter().zip(peaks) {
                    let level = self.levels.entry(*port).or_default();
                    *level = peak.max(*level * 0.7);
                }
            }
            Some(Err(err)) => self.meter_error = Some(err),
            None => {}
        }
    }

    /// Why the levels are missing, if the meter failed.
    pub fn meter_error(&self) -> Option<&str> {
        self.meter_error.as_deref()
    }

    /// 0 to 1 on a -40…0 dB scale. An input carries what its links bring.
    fn level(&self, snap: &Snapshot, port: u32, direction: Direction) -> f32 {
        let peak = match direction {
            Direction::Out => self.levels.get(&port).copied().unwrap_or(0.0),
            Direction::In => snap
                .links
                .iter()
                .filter(|l| l.input_port == port)
                .filter_map(|l| self.levels.get(&l.output_port))
                .fold(0.0, |sum, level| sum + level),
        };
        if peak <= 0.0 {
            return 0.0;
        }
        ((20.0 * peak.log10() + 40.0) / 40.0).clamp(0.0, 1.0)
    }

    fn walk(&mut self, spots: &[Spot], keys: &[Key]) -> bool {
        if spots.is_empty() {
            return false;
        }
        let mut index = self
            .cursor
            .and_then(|end| spots.iter().position(|s| s.end == end))
            .unwrap_or(0);
        let before = self.cursor;
        for key in keys {
            let at = spots[index].at;
            // Sideways goes to the nearest port in that direction: across the
            // node first, then into the next column.
            let sideways = |left: bool| {
                spots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        if left {
                            s.at.x < at.x - 1.0
                        } else {
                            s.at.x > at.x + 1.0
                        }
                    })
                    .min_by_key(|(_, s)| {
                        ((s.at.x - at.x).abs() as i32, (s.at.y - at.y).abs() as i32)
                    })
                    .map(|(i, _)| i)
            };
            index = match key {
                Key::Up => index.saturating_sub(1),
                Key::Down => (index + 1).min(spots.len() - 1),
                Key::Home | Key::PageUp => 0,
                Key::End | Key::PageDown => spots.len() - 1,
                Key::Left => sideways(true).unwrap_or(index),
                Key::Right => sideways(false).unwrap_or(index),
                _ => index,
            };
        }
        self.cursor = Some(spots[index].end);
        before != self.cursor
    }

    pub fn show(&mut self, ui: &mut Ui, form: &mut Form<'_>, snap: &Snapshot) -> Option<Request> {
        let pal = form.pal().clone();
        let keys = form.custom("graph");
        let focused = keys.is_some();
        let width = ui.available_width() - 20.0;
        let layout = layout(snap, width);
        let mut request = None;
        self.measure(ui.ctx(), snap);

        let keys = keys.unwrap_or_default();
        let moved = self.walk(&layout.spots, &keys);
        if moved {
            self.link = None;
        }
        if keys.iter().any(|k| matches!(k, Key::Enter | Key::Space))
            && let Some(end) = self.cursor
        {
            request = self.pick(snap, end);
        }

        egui::ScrollArea::both()
            .id_salt("graph")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (canvas, response) =
                    ui.allocate_exact_size(layout.size.max(vec2(width, 0.0)), Sense::click());
                let origin = canvas.min.to_vec2();
                let painter = ui.painter_at(canvas);
                let pointer = ui.input(|i| i.pointer.interact_pos());
                let spot_at = |end: End| {
                    layout
                        .spots
                        .iter()
                        .find(|s| s.end == end)
                        .map(|s| s.at + origin)
                };

                let on_node = |pointer: Pos2| {
                    layout
                        .nodes
                        .iter()
                        .any(|(_, r)| r.translate(origin).contains(pointer))
                };
                let mut hovered_link = None;
                for link in &snap.links {
                    let (Some(from), Some(to)) = (
                        spot_at(End::Port(link.output_port)),
                        spot_at(End::Port(link.input_port)),
                    ) else {
                        continue;
                    };
                    let points = bezier(from, to);
                    let hovered = pointer.is_some_and(|p| {
                        response.hovered() && !on_node(p) && near_curve(points, p)
                    });
                    if hovered {
                        hovered_link = Some(link.id);
                    }
                    let media = snap
                        .port(link.output_port)
                        .map_or(Media::Other, |p| p.media);
                    let doomed = self.link == Some(link.id)
                        || (focused
                            && self.link.is_none()
                            && self.cursor.is_some_and(|c| c.touches(link)));
                    let width = 1.0 + 13.0 * self.level(snap, link.output_port, Direction::Out);
                    let stroke = if doomed {
                        Stroke::new(width.max(3.0), pal.accent)
                    } else if link.state == "error" {
                        Stroke::new(width, pal.danger)
                    } else if hovered {
                        Stroke::new(width + 1.0, media_color(&pal, media))
                    } else {
                        Stroke::new(width, media_color(&pal, media))
                    };
                    painter.add(CubicBezierShape::from_points_stroke(
                        points,
                        false,
                        Color32::TRANSPARENT,
                        stroke,
                    ));
                }

                for (id, rect) in &layout.nodes {
                    let rect = rect.translate(origin);
                    let Some(node) = snap.node(*id) else {
                        continue;
                    };
                    painter.rect(
                        rect,
                        CornerRadius::same(4),
                        pal.surface,
                        Stroke::new(1.0, pal.outline),
                        StrokeKind::Inside,
                    );
                    let title = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE));
                    painter.with_clip_rect(title.shrink(4.0)).text(
                        pos2(title.left() + 10.0, title.center().y),
                        Align2::LEFT_CENTER,
                        node.label(),
                        fastframe_fonts::Weight::SemiBold.font_id(13.0),
                        pal.text,
                    );
                    ui.interact(title, ui.id().with(("node", id)), Sense::hover())
                        .on_hover_text(format!("{}\n{}  ·  id {}", node.name, node.class, node.id));

                    if node.volumes.is_empty() {
                        continue;
                    }
                    let volumes = self
                        .volume
                        .as_ref()
                        .filter(|(node, _, at)| *node == *id && at.elapsed() < SETTLE)
                        .map_or_else(|| node.volumes.clone(), |(_, volumes, _)| volumes.clone());
                    let strips = strips(node);
                    for strip_index in 0..strips {
                        let top = rect.bottom() - (strips - strip_index) as f32 * VOLUME;
                        let strip = Rect::from_min_max(
                            pos2(rect.left() + 6.0, top),
                            pos2(rect.right() - 6.0, top + VOLUME - 4.0),
                        );
                        let lead = Rect::from_min_size(strip.min, vec2(26.0, strip.height()));
                        let volume = match strip_index {
                            0 => volumes.iter().copied().fold(0.0, f32::max),
                            channel => volumes[channel - 1],
                        };
                        if strip_index == 0 {
                            let hit = ui
                                .interact(lead, ui.id().with(("mute", id)), Sense::click())
                                .on_hover_text(if node.muted { "Unmute" } else { "Mute" });
                            if hit.clicked() {
                                request = Some(Request::Mute {
                                    id: *id,
                                    muted: !node.muted,
                                });
                            }
                            let icon = if node.muted {
                                Icon::Muted
                            } else {
                                Icon::Speaker
                            };
                            let tint = if hit.hovered() {
                                pal.accent
                            } else {
                                pal.secondary
                            };
                            icon.image(tint, 14.0).paint_at(
                                ui,
                                Rect::from_center_size(lead.center(), vec2(14.0, 14.0)),
                            );
                        } else {
                            let channel = node
                                .volume_channels
                                .get(strip_index - 1)
                                .map_or("", String::as_str);
                            painter.text(
                                lead.center(),
                                Align2::CENTER_CENTER,
                                channel,
                                egui::FontId::proportional(11.0),
                                pal.dim,
                            );
                        }
                        let track = Rect::from_min_max(
                            pos2(lead.right() + 6.0, strip.center().y - 2.0),
                            pos2(strip.right() - 44.0, strip.center().y + 2.0),
                        );
                        let bar = ui.interact(
                            track.expand2(vec2(4.0, 6.0)),
                            ui.id().with(("volume", id, strip_index)),
                            Sense::click_and_drag(),
                        );
                        let mut shown = volume;
                        if let Some(pointer) = bar.interact_pointer_pos()
                            && (bar.is_pointer_button_down_on() || bar.clicked())
                        {
                            shown = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
                            if shown != volume {
                                let volumes = match strip_index {
                                    0 => pw::balanced(&volumes, shown),
                                    channel => {
                                        let mut volumes = volumes.clone();
                                        volumes[channel - 1] = shown;
                                        volumes
                                    }
                                };
                                self.volume = Some((*id, volumes.clone(), Instant::now()));
                                request = Some(Request::Volume {
                                    id: *id,
                                    route: node.route,
                                    volumes,
                                });
                                ui.ctx().request_repaint_after(SETTLE);
                            }
                        }
                        let active = bar.hovered() || bar.dragged();
                        let fill = match (node.muted, active) {
                            (_, true) => pal.accent,
                            (true, false) => pal.dim,
                            (false, false) => pal.secondary,
                        };
                        let x = track.left() + track.width() * shown.clamp(0.0, 1.0);
                        painter.rect_filled(track, CornerRadius::same(2), pal.window);
                        painter.rect_filled(
                            Rect::from_min_max(track.min, pos2(x, track.bottom())),
                            CornerRadius::same(2),
                            fill,
                        );
                        painter.circle_filled(pos2(x, track.center().y), 5.0, fill);
                        painter.text(
                            pos2(strip.right(), strip.center().y),
                            Align2::RIGHT_CENTER,
                            format!("{:.0} %", shown * 100.0),
                            egui::FontId::proportional(12.0),
                            if node.muted { pal.dim } else { pal.secondary },
                        );
                    }
                }

                let mut dropped_on = None;
                let mut drag_ended = false;
                for spot in &layout.spots {
                    let Some((node, direction)) = spot.end.side(snap) else {
                        continue;
                    };
                    let ports: Vec<_> = snap
                        .ports
                        .iter()
                        .filter(|p| match spot.end {
                            End::Port(id) => p.id == id,
                            End::Node(..) => p.node == node && p.direction == direction,
                        })
                        .collect();
                    let at = spot.at + origin;
                    let row = spot.row.translate(origin);
                    let hit = ui.interact(row, ui.id().with(spot.end), Sense::click_and_drag());
                    if hit.clicked() {
                        form.focus("graph");
                        self.cursor = Some(spot.end);
                        request = self.pick(snap, spot.end);
                    }
                    if hit.drag_started() {
                        form.focus("graph");
                        self.cursor = Some(spot.end);
                        self.picked = None;
                        self.dragging = Some(spot.end);
                    }
                    drag_ended |= hit.drag_stopped();
                    if pointer.is_some_and(|p| row.contains(p)) {
                        dropped_on = Some(spot.end);
                    }

                    let cursor = focused && self.cursor == Some(spot.end);
                    let picked = self.picked == Some(spot.end) || self.dragging == Some(spot.end);
                    if let End::Port(_) = spot.end
                        && let Some(port) = ports.first()
                    {
                        let label_color = if cursor || picked {
                            pal.accent
                        } else if port.monitor {
                            pal.dim
                        } else {
                            pal.secondary
                        };
                        let (anchor, x) = match direction {
                            Direction::In => (Align2::LEFT_CENTER, at.x + 10.0),
                            Direction::Out => (Align2::RIGHT_CENTER, at.x - 10.0),
                        };
                        painter.with_clip_rect(row).text(
                            pos2(x, at.y),
                            anchor,
                            &port.name,
                            egui::FontId::proportional(12.0),
                            label_color,
                        );
                    }
                    let level = ports
                        .iter()
                        .map(|p| self.level(snap, p.id, direction))
                        .fold(0.0, f32::max);
                    let media = ports.iter().map(|p| p.media).min().unwrap_or(Media::Other);
                    let (color, radius) = if picked {
                        (pal.accent, 3.5 + 7.5 * level)
                    } else {
                        (media_color(&pal, media), 2.5 + 7.5 * level)
                    };
                    painter.circle_filled(at, radius, color);
                    let mut ring = radius + 3.0;
                    if let End::Node(..) = spot.end {
                        painter.circle_stroke(at, ring, Stroke::new(1.0, color));
                        ring += 3.0;
                    }
                    if cursor || hit.hovered() {
                        painter.circle_stroke(at, ring, Stroke::new(1.5, pal.accent));
                    }
                    if cursor && moved {
                        ui.scroll_to_rect(row.expand(20.0), None);
                    }
                }

                let pending = self.dragging.or(self.picked);
                if let Some(end) = pending
                    && let (Some(at), Some(pointer)) = (spot_at(end), pointer)
                    && canvas.contains(pointer)
                {
                    let points = match end.side(snap) {
                        Some((_, Direction::In)) => bezier(pointer, at),
                        _ => bezier(at, pointer),
                    };
                    painter.add(CubicBezierShape::from_points_stroke(
                        points,
                        false,
                        Color32::TRANSPARENT,
                        Stroke::new(1.5, pal.accent),
                    ));
                    ui.ctx().request_repaint();
                }
                if drag_ended && let Some(from) = self.dragging.take() {
                    request = dropped_on
                        .map(|to| pairs(snap, from, to))
                        .filter(|pairs| !pairs.is_empty())
                        .map(Request::Link);
                }
                if response.clicked() {
                    form.focus("graph");
                    self.picked = None;
                    self.link = hovered_link;
                }
            });
        if focused {
            ui.painter().rect_stroke(
                ui.min_rect(),
                CornerRadius::same(3),
                Stroke::new(1.0, pal.accent),
                StrokeKind::Outside,
            );
        }
        request
    }

    /// Esc first drops a half-made link.
    pub fn cancel(&mut self) -> bool {
        self.dragging = None;
        self.picked.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::Port;

    fn node(id: u32, name: &str, class: &str) -> Node {
        Node {
            id,
            name: name.to_owned(),
            description: name.to_owned(),
            class: class.to_owned(),
            ..Node::default()
        }
    }

    fn port(id: u32, node: u32, direction: Direction, monitor: bool) -> Port {
        Port {
            id,
            node,
            name: format!("port{id}"),
            direction,
            media: Media::Audio,
            monitor,
            channel: String::new(),
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            nodes: vec![
                Node {
                    volumes: vec![0.5],
                    ..node(1, "Speakers", "Audio/Sink")
                },
                node(2, "Mic", "Audio/Source"),
                node(3, "Filter", "Audio/Sink"),
                node(4, "Driver", ""),
            ],
            ports: vec![
                port(10, 1, Direction::In, false),
                port(11, 1, Direction::Out, true),
                port(20, 2, Direction::Out, false),
                port(30, 3, Direction::In, false),
                port(31, 3, Direction::Out, false),
            ],
            links: vec![Link {
                id: 50,
                output_node: 3,
                output_port: 31,
                input_node: 1,
                input_port: 10,
                state: "active".to_owned(),
            }],
            ..Snapshot::default()
        }
    }

    #[test]
    fn sources_go_left_sinks_right_and_portless_nodes_away() {
        let layout = layout(&snap(), 1000.0);
        let x = |id: u32| {
            layout
                .nodes
                .iter()
                .find(|(n, _)| *n == id)
                .map(|(_, r)| r.left())
        };
        assert_eq!(x(2), Some(MARGIN));
        assert!(
            x(3).unwrap() > 0.0 && x(3) < x(1),
            "a monitor makes no source"
        );
        assert_eq!(x(4), None);
        let height = |id: u32| {
            layout
                .nodes
                .iter()
                .find(|(n, _)| *n == id)
                .map(|(_, r)| r.height())
        };
        assert_eq!(
            height(1),
            Some(TITLE + ROW + 6.0 + VOLUME),
            "room for the volume"
        );
        assert_eq!(height(3), Some(TITLE + ROW + 6.0));
        let stereo = Node {
            volumes: vec![0.5, 0.25],
            ..node(5, "Headphones", "Audio/Sink")
        };
        assert_eq!(strips(&stereo), 3, "the whole node, then each channel");
        let order: Vec<End> = layout.spots.iter().map(|s| s.end).collect();
        assert_eq!(order, [20, 30, 31, 10, 11].map(End::Port));
    }

    #[test]
    fn picking_two_ports_links_them_and_disconnect_takes_the_cursor_links() {
        let snap = snap();
        let mut graph = Graph::default();
        assert!(graph.pick(&snap, End::Port(10)).is_none());
        assert!(matches!(
            graph.pick(&snap, End::Port(20)),
            Some(Request::Link(pairs)) if pairs == [(20, 10)]
        ));
        assert_eq!(graph.picked, None);

        let spots = layout(&snap, 1000.0).spots;
        graph.walk(&spots, &[Key::Down, Key::Down, Key::Down]);
        assert_eq!(graph.cursor, Some(End::Port(10)));
        assert_eq!(graph.doomed(&snap), [50]);
        graph.walk(&spots, &[Key::Left]);
        assert_eq!(
            graph.cursor,
            Some(End::Port(31)),
            "left from an input crosses to the column before"
        );
    }

    #[test]
    fn the_all_channels_dots_link_node_to_node_and_disconnect_it() {
        let stereo = |id: u32, node: u32, direction: Direction, channel: &str| Port {
            channel: channel.to_owned(),
            ..port(id, node, direction, false)
        };
        let mut snap = Snapshot {
            nodes: vec![
                node(1, "Player", "Stream/Output/Audio"),
                node(2, "Speakers", "Audio/Sink"),
            ],
            ports: vec![
                stereo(10, 1, Direction::Out, "FL"),
                stereo(11, 1, Direction::Out, "FR"),
                stereo(20, 2, Direction::In, "FL"),
                stereo(21, 2, Direction::In, "FR"),
            ],
            ..Snapshot::default()
        };
        let order: Vec<End> = layout(&snap, 1000.0).spots.iter().map(|s| s.end).collect();
        assert_eq!(
            order,
            [
                End::Node(1, Direction::Out),
                End::Port(10),
                End::Port(11),
                End::Node(2, Direction::In),
                End::Port(20),
                End::Port(21),
            ]
        );

        let mut graph = Graph::default();
        assert!(graph.pick(&snap, End::Node(2, Direction::In)).is_none());
        assert!(matches!(
            graph.pick(&snap, End::Node(1, Direction::Out)),
            Some(Request::Link(pairs)) if pairs == [(10, 20), (11, 21)]
        ));

        let link = |id: u32, output_port: u32, input_port: u32| Link {
            id,
            output_node: 1,
            output_port,
            input_node: 2,
            input_port,
            state: "active".to_owned(),
        };
        snap.links = vec![link(70, 10, 20), link(71, 11, 21)];
        graph.cursor = Some(End::Node(2, Direction::In));
        assert_eq!(graph.doomed(&snap), [70, 71]);
        graph.cursor = Some(End::Port(11));
        assert_eq!(graph.doomed(&snap), [71]);
    }

    #[test]
    fn midi_devices_sit_below_the_audio_nodes() {
        let midi = |id: u32, node: u32, direction: Direction| Port {
            media: Media::Midi,
            ..port(id, node, direction, false)
        };
        let mut snap = snap();
        snap.nodes.push(node(5, "BLE MIDI", "Midi/Bridge"));
        snap.ports
            .extend([midi(60, 5, Direction::In), midi(61, 5, Direction::Out)]);
        let layout = layout(&snap, 1000.0);
        let rect = |id: u32| layout.nodes.iter().find(|(n, _)| *n == id).unwrap().1;
        let audio_bottom = [1, 2, 3]
            .map(|id| rect(id).bottom())
            .into_iter()
            .fold(0.0, f32::max);
        assert!(rect(5).top() > audio_bottom);
        assert_eq!(rect(5).left(), rect(3).left(), "still the middle column");
    }
}
