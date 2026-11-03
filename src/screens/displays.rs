use std::time::{Duration, Instant};

use egui::{Frame, Margin, Rect, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2};

use super::{Action, Cx};
use crate::form::{self, Form, Icon, Press, Row, Sort, Table};
use crate::hypr::{Mode, Monitor, Request, Snapshot};
use crate::light::{self, Auto, Backlight};
use crate::tablet::{self, Tablet};
use crate::theme::Palette;

const REFRESH: Duration = Duration::from_secs(2);
/// How long an applied layout waits to be kept before it goes back, so a
/// screen that went dark can't lock the desktop out.
const CONFIRM: Duration = Duration::from_secs(10);
/// How long a brightness just set wins over the snapshot, which lags behind
/// it; without that the slider springs back to the old level mid-drag.
const SETTLE: Duration = Duration::from_secs(1);
const PANE_WIDTH: f32 = 600.0;
const TABLE: Table = Table {
    name: "Name",
    columns: &[
        ("Resolution", 110.0),
        ("Refresh", 70.0),
        ("Scale", 60.0),
        ("Rotation", 90.0),
    ],
};
const GAP: f32 = 20.0;
const LABEL: f32 = 170.0;
const FIELD: f32 = 300.0;
const PADDING: i8 = 16;
/// The arrange map's height, and how close two edges have to be dragged
/// before they line up, in desktop pixels.
const MAP_HEIGHT: f32 = 220.0;
const SNAP: f32 = 60.0;

const ROTATIONS: [(&str, u8); 4] = [
    ("Normal", 0),
    ("90° left", 1),
    ("180°", 2),
    ("90° right", 3),
];
const SCALES: [(&str, f32); 7] = [
    ("100 %", 1.0),
    ("125 %", 1.25),
    ("150 %", 1.5),
    ("175 %", 1.75),
    ("200 %", 2.0),
    ("250 %", 2.5),
    ("300 %", 3.0),
];

pub struct Displays {
    snap: Option<Result<Snapshot, String>>,
    /// The layout being edited, and what it started from, so an unsaved edit
    /// isn't overwritten by the next refresh.
    draft: Vec<Monitor>,
    base: Vec<Monitor>,
    selected: usize,
    /// The last save that failed, until the next one works.
    error: Option<String>,
    refreshed: Option<Instant>,
    /// A layout applied live and waiting to be kept.
    confirm: Option<Confirm>,
    /// The brightness just sent, which wins over the snapshot for SETTLE.
    brightness: Option<(String, f32, Instant)>,
    /// A brightness that would not be set, shown by the slider.
    light_error: Option<String>,
    /// Follows the ambient light sensor while it is here; dropping it stops.
    auto: Option<Auto>,
    /// Streamed by Sunshine while it is here; dropping it removes the output.
    tablet: Option<Tablet>,
    /// Selects the tablet's output once a snapshot has it.
    find_tablet: bool,
    /// `None` keeps connected screens first, in the order Hyprland lists them.
    sort: Option<Sort>,
}

struct Confirm {
    until: Instant,
    /// The layout to go back to when it isn't kept.
    previous: Vec<Monitor>,
}

/// The brightness of the screen being shown, and what the pane did with it.
struct Light<'a> {
    /// The backlight of that screen, if it has one; without one no brightness
    /// is shown at all.
    backlight: Option<&'a Backlight>,
    /// Its level, 0 to 1, the one just sent while that is the fresher of the two.
    level: f32,
    error: Option<&'a str>,
    sensor: bool,
    auto: bool,
    /// The level the slider was dragged to.
    set: Option<f32>,
}

struct Line {
    header: Option<&'static str>,
    /// Which monitor in the draft the row stands for.
    monitor: usize,
    text: String,
    /// Resolution, refresh, scale and rotation as shown.
    cells: [String; 4],
    switch: Option<bool>,
}

impl Displays {
    pub fn new() -> Self {
        Self {
            snap: None,
            draft: Vec::new(),
            base: Vec::new(),
            selected: 0,
            error: None,
            refreshed: None,
            confirm: None,
            brightness: None,
            light_error: None,
            auto: None,
            tablet: None,
            find_tablet: false,
            sort: None,
        }
    }

    pub fn opened(&mut self) {
        self.refreshed = None;
    }

    pub fn snapshot(&mut self, snap: Result<Snapshot, String>) {
        if let Ok(snap) = &snap {
            if self.draft == self.base {
                self.draft = snap.monitors.clone();
                self.base = snap.monitors.clone();
            } else {
                log::debug!("keeping the edited layout over the new snapshot");
            }
            let tablet = snap.monitors.iter().find(|m| m.name == tablet::OUTPUT);
            if let (Some(running), Some(tablet)) = (&mut self.tablet, tablet) {
                running.capture(match tablet.mirror.is_empty() {
                    true => tablet::OUTPUT,
                    false => &tablet.mirror,
                });
            }
            if self.find_tablet
                && let Some(index) = self.draft.iter().position(|m| m.name == tablet::OUTPUT)
            {
                self.selected = index;
                self.find_tablet = false;
            }
        }
        self.snap = Some(snap);
    }

    pub fn done(&mut self, result: Result<(), String>) {
        self.error = result.err();
        if self.error.is_some() && self.confirm.take().is_some() {
            log::warn!("the layout did not apply, so there is nothing to keep");
        }
    }

    /// A brightness that would not be set; the layout is none the worse, so
    /// a working one stays applied and only the line by the slider appears.
    pub fn light(&mut self, err: String) {
        self.light_error = Some(err);
    }

    pub fn show(&mut self, cx: &mut Cx<'_>, form: &mut Form<'_>, rect: Rect) -> Option<Action> {
        if self.refreshed.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.refreshed = Some(Instant::now());
            cx.screens.push(Request::Refresh);
        }
        cx.ctx.request_repaint_after(REFRESH);

        if let Some(tablet) = &mut self.tablet {
            tablet.poll();
        }
        let (icon, tip) = match self.tablet {
            Some(_) => (Icon::Stop, "Stop the tablet screen"),
            None => (Icon::Tablet, "Use a tablet as a screen"),
        };
        let top = form.top_bar(cx.ctx, false, &[("displays-tablet", icon, tip)], None);
        if top == Some(0) {
            if self.tablet.take().is_none() {
                match Tablet::start(cx.ctx.clone()) {
                    Ok(tablet) => {
                        self.tablet = Some(tablet);
                        self.find_tablet = true;
                        self.error = None;
                    }
                    Err(err) => self.error = Some(err),
                }
            }
            cx.screens.push(Request::Refresh);
        }

        let read_error = match &self.snap {
            Some(Err(err)) => Some(err.clone()),
            _ => None,
        };
        let saved = match &self.snap {
            Some(Ok(snap)) => (snap.imported, snap.path.display().to_string()),
            _ => (true, String::new()),
        };
        if let Some(err) = self.auto.as_ref().and_then(Auto::error) {
            self.light_error = Some(err);
            self.auto = None;
        }
        let name = self
            .draft
            .get(self.selected)
            .map_or("", |monitor| monitor.name.as_str());
        let (backlight, sensor) = match &self.snap {
            Some(Ok(snap)) => (light::of(&snap.lights, name), snap.sensor),
            _ => (None, false),
        };
        let just_set = self
            .brightness
            .as_ref()
            .filter(|(device, _, at)| {
                at.elapsed() < SETTLE && backlight.is_some_and(|light| *device == light.device)
            })
            .map(|(_, level, _)| *level);
        let mut light = Light {
            backlight,
            level: just_set.unwrap_or(backlight.map_or(0.0, |light| light.level)),
            error: self.light_error.as_deref(),
            sensor,
            auto: self.auto.is_some(),
            set: None,
        };
        let mut lines = lines(&self.draft);
        if let Some(sort) = self.sort {
            let draft = &self.draft;
            let mode = |line: &Line| {
                draft[line.monitor]
                    .mode
                    .filter(|_| draft[line.monitor].enabled)
            };
            form::sort_groups(
                &mut lines,
                |l| l.header.is_some(),
                sort,
                |a, b| {
                    let (one, two) = (&draft[a.monitor], &draft[b.monitor]);
                    match sort.column {
                        0 => form::by_text(&a.text, &b.text),
                        1 => {
                            let pixels = |line| mode(line).map(|m| m.width * m.height);
                            pixels(a).cmp(&pixels(b))
                        }
                        2 => {
                            let refresh = |line| mode(line).map_or(0.0, |m| m.refresh);
                            refresh(a).total_cmp(&refresh(b))
                        }
                        3 => one.scale.total_cmp(&two.scale),
                        _ => (one.transform % 4).cmp(&(two.transform % 4)),
                    }
                },
            );
        }
        let rows: Vec<Row<'_>> = lines
            .iter()
            .map(|line| match line.header {
                Some(header) => Row::Header(header),
                None => Row::Item {
                    text: &line.text,
                    detail: None,
                    signal: None,
                    switch: line.switch,
                    tree: &[],
                    columns: &line.cells,
                    links: &[],
                },
            })
            .collect();
        // Headers take a row of their own, so the list index isn't the monitor's.
        let mut index = lines
            .iter()
            .position(|line| line.header.is_none() && line.monitor == self.selected)
            .unwrap_or(1);
        let changed = self.draft != self.base;
        let (mut press, mut reset, mut save) = (None, false, false);
        let mut picked = None;
        let (draft, selected, error) = (&mut self.draft, &mut self.selected, &self.error);
        let tablet = self.tablet.as_mut();
        let pal = cx.pal;
        form::screen(cx.ctx, pal, "displays", "Screens", rect, |ui| {
            let area = ui.available_rect_before_wrap();
            let band = Rect::from_min_max(area.min, pos2(area.right(), area.top() + MAP_HEIGHT));
            let below = band.bottom() + GAP;
            let split = area.right() - PANE_WIDTH;
            let list =
                Rect::from_min_max(pos2(area.left(), below), pos2(split - GAP, area.bottom()));
            let pane = Rect::from_min_max(pos2(split, below), area.max);
            ui.scope_builder(UiBuilder::new().max_rect(band), |ui| {
                if let Some(clicked) = map(ui, pal, draft, *selected) {
                    *selected = clicked;
                    picked = Some(clicked);
                }
            });
            ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
                press = form.table(ui, "list", &TABLE, &mut self.sort, &rows, &mut index);
            });
            if draft.is_empty() {
                return;
            }
            ui.scope_builder(UiBuilder::new().max_rect(pane), |ui| {
                Frame::new()
                    .stroke(Stroke::new(1.0, pal.outline))
                    .corner_radius(3)
                    .show(ui, |ui| {
                        let inner = (pane.size() - vec2(2.0, 2.0)).max(Vec2::ZERO);
                        ui.set_min_size(inner);
                        ui.set_max_width(inner.x);
                        let shown = read_error.as_deref().or(error.as_deref());
                        (reset, save) = pane_contents(
                            ui, form, draft, selected, changed, &saved, shown, tablet, &mut light,
                        );
                    });
            });
        });
        if let Some(index) = picked {
            self.selected = index;
        } else if let Some(line) = lines.get(index).filter(|line| line.header.is_none()) {
            self.selected = line.monitor;
        }
        let (dragged, shown, auto) = (light.set, light.level, light.auto);
        // brightnessctl only takes whole percent, and the slider reports a
        // change on every frame the button is down, however far it moved.
        if let (Some(level), Some(backlight)) = (dragged, backlight)
            && (level * 100.0).round() != (shown * 100.0).round()
        {
            cx.screens.push(Request::Brightness {
                device: backlight.device.clone(),
                level,
            });
            self.brightness = Some((backlight.device.clone(), level, Instant::now()));
            self.light_error = None;
            cx.ctx.request_repaint_after(SETTLE);
        }
        if auto != self.auto.is_some() {
            self.auto = match (auto, backlight) {
                (true, Some(backlight)) => Some(Auto::start(
                    cx.ctx.clone(),
                    backlight.device.clone(),
                    backlight.level,
                )),
                _ => None,
            };
        }
        if press == Some(Press::Toggle)
            && let Some(monitor) = self.draft.get_mut(self.selected)
            && monitor.connected
        {
            monitor.enabled = !monitor.enabled;
            log::debug!(
                "'{}' is now {}",
                monitor.label(),
                if monitor.enabled { "on" } else { "off" }
            );
            settle(&mut self.draft, self.selected);
        }
        if reset {
            log::debug!("discarding the layout changes");
            self.draft = self.base.clone();
            self.error = None;
        }
        if save {
            if !self.draft.iter().any(|monitor| monitor.enabled) {
                self.error = Some("[HY-7] at least one screen has to stay on".to_owned());
            } else {
                log::info!("applying the layout, waiting {CONFIRM:?} for it to be kept");
                self.error = None;
                cx.screens.push(Request::Apply(self.draft.clone()));
                cx.screens.push(Request::Attention);
                self.confirm = Some(Confirm {
                    until: Instant::now() + CONFIRM,
                    previous: self.base.clone(),
                });
            }
        }
        if let Some(confirm) = &self.confirm {
            let left = confirm.until.saturating_duration_since(Instant::now());
            cx.ctx.request_repaint_after(Duration::from_millis(200));
            let mut keep = false;
            let mut back = left.is_zero();
            form::popup(cx.ctx, pal, "displays-confirm", "Keep this layout?", |ui| {
                ui.set_max_width(360.0);
                ui.label(
                    RichText::new(format!(
                        "Going back in {} s",
                        left.as_secs_f32().ceil() as u32
                    ))
                    .color(pal.text),
                );
                ui.add_space(12.0);
                match form::button_row(
                    form,
                    ui,
                    &[("displays-back", "Go back"), ("displays-keep", "Keep")],
                ) {
                    Some(0) => back = true,
                    Some(_) => keep = true,
                    None => {}
                }
            });
            if keep {
                log::info!("the layout is kept, saving it");
                cx.screens.push(Request::Save(self.draft.clone()));
                self.base = self.draft.clone();
                self.confirm = None;
            } else if back {
                log::info!("the layout was not kept, going back to the last one");
                let previous = self.confirm.take().expect("a pending layout").previous;
                cx.screens.push(Request::Apply(previous.clone()));
                self.draft = previous.clone();
                self.base = previous;
            }
        }
        None
    }
}

/// The list: a header per group, then a row per screen in it.
fn lines(draft: &[Monitor]) -> Vec<Line> {
    let mut lines = Vec::new();
    for connected in [true, false] {
        let group: Vec<usize> = (0..draft.len())
            .filter(|index| draft[*index].connected == connected)
            .collect();
        if group.is_empty() {
            continue;
        }
        lines.push(Line {
            header: Some(if connected {
                "Connected"
            } else {
                "Not connected"
            }),
            monitor: usize::MAX,
            text: String::new(),
            cells: Default::default(),
            switch: None,
        });
        for index in group {
            let monitor = &draft[index];
            lines.push(Line {
                header: None,
                monitor: index,
                text: monitor.label().to_owned(),
                cells: cells(monitor),
                switch: monitor.connected.then_some(monitor.enabled),
            });
        }
    }
    lines
}

fn cells(monitor: &Monitor) -> [String; 4] {
    let alone = |text: &str| [text.to_owned(), String::new(), String::new(), String::new()];
    if !monitor.connected {
        return alone("not connected");
    }
    if !monitor.enabled {
        return alone("off");
    }
    let Some(mode) = monitor.mode else {
        return alone("on");
    };
    let turn = ROTATIONS
        .iter()
        .find(|(_, transform)| *transform == monitor.transform % 4)
        .map_or("", |(label, _)| *label);
    [
        mode.size(),
        format!("{:.0} Hz", mode.refresh),
        format!("{:.0} %", monitor.scale * 100.0),
        turn.to_owned(),
    ]
}

/// The selected screen's settings. `saved` is whether hyprland.lua loads the
/// layout file and where that file is. Returns reset and save.
#[allow(clippy::too_many_arguments)]
fn pane_contents(
    ui: &mut Ui,
    form: &mut Form<'_>,
    draft: &mut [Monitor],
    selected: &mut usize,
    changed: bool,
    saved: &(bool, String),
    error: Option<&str>,
    tablet: Option<&mut Tablet>,
    light: &mut Light<'_>,
) -> (bool, bool) {
    let pal = form.pal().clone();
    let label = draft
        .get(*selected)
        .map_or(String::new(), |m| m.label().to_owned());
    let (mut reset, mut save) = (false, false);
    let header = Margin {
        bottom: 10,
        ..Margin::same(PADDING)
    };
    Frame::new().inner_margin(header).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&label)
                    .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            let buttons = 2.0;
            let width =
                buttons * form::FIELD_HEIGHT + (buttons - 1.0) * ui.spacing().item_spacing.x;
            ui.add_space((ui.available_width() - width).max(0.0));
            reset = form.icon_button(ui, "displays-reset", Icon::Reset, "Discard changes");
            save = form.icon_button(ui, "displays-save", Icon::Save, "Apply and save");
        });
    });
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));
    egui::ScrollArea::vertical()
        .id_salt("displays")
        .max_height(ui.available_height().max(60.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new().inner_margin(PADDING).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(error) = error {
                    ui.label(RichText::new(error).color(pal.danger));
                }
                if !saved.0 {
                    ui.label(
                        RichText::new("hyprland.lua does not load monitors.lua: a saved layout is lost on restart")
                            .color(pal.warning),
                    );
                }
                if changed {
                    ui.label(RichText::new("Not applied yet").color(pal.info));
                }
                let on_tablet = draft.get(*selected).is_some_and(|m| m.name == tablet::OUTPUT);
                if let (true, Some(tablet)) = (on_tablet, tablet) {
                    if let Some(err) = &tablet.error {
                        ui.label(RichText::new(err).color(pal.danger));
                    }
                    let addresses = match tablet.addresses.is_empty() {
                        true => "no network address".to_owned(),
                        false => tablet.addresses.join("\n"),
                    };
                    row(ui, &pal, "Address", &addresses);
                    let streams = match tablet.streams {
                        0 => "none yet".to_owned(),
                        1 => "1 Moonlight".to_owned(),
                        n => format!("{n} Moonlight"),
                    };
                    row(ui, &pal, "Streaming to", &streams);
                    if tablet.pairing {
                        field(ui, &pal, "Moonlight PIN", |ui| {
                            form.entry(ui, "displays-pin", &mut tablet.pin, 80.0, false)
                        });
                        tablet.pin.retain(|c| c.is_ascii_digit());
                        if tablet.pin.len() == tablet::PIN_LENGTH {
                            tablet.pair();
                        }
                    }
                    ui.add_space(4.0);
                }
                let before = draft.get(*selected).map(Monitor::logical);
                fields(ui, form, draft, *selected, light);
                if draft.get(*selected).map(Monitor::logical) != before {
                    settle(draft, *selected);
                }
                if !saved.1.is_empty() {
                    row(ui, &pal, "Saved in", &saved.1);
                }
            });
        });
    (reset, save)
}

/// Every screen to scale where it sits on the desktop; the selected one is
/// outlined and any of them can be dragged. Some(index) when one was clicked.
fn map(ui: &mut Ui, pal: &Palette, draft: &mut [Monitor], selected: usize) -> Option<usize> {
    let shown: Vec<usize> = (0..draft.len()).filter(|i| placed(&draft[*i])).collect();
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), MAP_HEIGHT), Sense::hover());
    ui.painter().rect_filled(area, 3.0, pal.window);
    if shown.is_empty() {
        return None;
    }
    let boxes: Vec<(f32, f32, f32, f32)> = shown
        .iter()
        .map(|i| {
            let (w, h) = draft[*i].logical();
            (draft[*i].x as f32, draft[*i].y as f32, w, h)
        })
        .collect();
    let left = boxes.iter().map(|b| b.0).fold(f32::MAX, f32::min);
    let top = boxes.iter().map(|b| b.1).fold(f32::MAX, f32::min);
    let right = boxes.iter().map(|b| b.0 + b.2).fold(f32::MIN, f32::max);
    let bottom = boxes.iter().map(|b| b.1 + b.3).fold(f32::MIN, f32::max);
    let margin = 20.0;
    let zoom = ((area.width() - 2.0 * margin) / (right - left).max(1.0))
        .min((area.height() - 2.0 * margin) / (bottom - top).max(1.0));
    let origin = area.center() - vec2((right - left) * zoom, (bottom - top) * zoom) / 2.0;
    let on_screen = |x: f32, y: f32| origin + vec2((x - left) * zoom, (y - top) * zoom);

    let mut clicked = None;
    let mut moved = None;
    let mut dropped = None;
    for (slot, (index, bounds)) in shown.iter().zip(&boxes).enumerate() {
        let (index, &(x, y, w, h)) = (*index, bounds);
        let rect = Rect::from_min_size(on_screen(x, y), vec2(w * zoom, h * zoom));
        let hit = ui.interact(
            rect,
            ui.id().with(("monitor", slot)),
            Sense::click_and_drag(),
        );
        let fill = if index == selected {
            pal.surface_active
        } else if hit.hovered() {
            pal.surface_hover
        } else {
            pal.surface
        };
        let stroke = if index == selected {
            Stroke::new(2.0, pal.accent)
        } else {
            Stroke::new(1.0, pal.outline)
        };
        ui.painter().rect_filled(rect, 3.0, fill);
        ui.painter()
            .rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
        let name = &draft[index].name;
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            name,
            fastframe_fonts::Weight::Medium.font_id(13.0),
            pal.text,
        );
        if hit.clicked() || hit.drag_started() {
            clicked = Some(index);
        }
        if hit.dragged() {
            let delta = hit.drag_delta() / zoom;
            moved = Some((index, delta));
        }
        if hit.drag_stopped() {
            dropped = Some(index);
        }
    }
    if let Some((index, delta)) = moved {
        drag(draft, index, delta);
    }
    if let Some(index) = dropped {
        settle(draft, index);
    }
    clicked
}

/// Moves a screen by `delta` desktop pixels and lines its edges up with the
/// others when they come close, so there is no gap or overlap by accident.
fn drag(draft: &mut [Monitor], index: usize, delta: Vec2) {
    let (width, height) = draft[index].logical();
    let mut x = draft[index].x as f32 + delta.x;
    let mut y = draft[index].y as f32 + delta.y;
    let others: Vec<(f32, f32, f32, f32)> = (0..draft.len())
        .filter(|i| *i != index && placed(&draft[*i]))
        .map(|i| {
            let (w, h) = draft[i].logical();
            (draft[i].x as f32, draft[i].y as f32, w, h)
        })
        .collect();
    let mut best = (SNAP, 0.0);
    for (ox, _, ow, _) in &others {
        for (ours, theirs) in [
            (x, *ox),
            (x, ox + ow),
            (x + width, *ox),
            (x + width, ox + ow),
        ] {
            let gap = theirs - ours;
            if gap.abs() < best.0 {
                best = (gap.abs(), gap);
            }
        }
    }
    x += best.1;
    let mut best = (SNAP, 0.0);
    for (_, oy, _, oh) in &others {
        for (ours, theirs) in [
            (y, *oy),
            (y, oy + oh),
            (y + height, *oy),
            (y + height, oy + oh),
        ] {
            let gap = theirs - ours;
            if gap.abs() < best.0 {
                best = (gap.abs(), gap);
            }
        }
    }
    y += best.1;
    draft[index].x = x.round() as i32;
    draft[index].y = y.round() as i32;
}

/// Puts a screen flush against one of the others, overlapping none, as close
/// to where it was let go as it can. Hyprland places the desktop by these
/// coordinates, so an overlap would show the same area twice and a gap would
/// leave the pointer stuck at the hole.
fn settle(draft: &mut [Monitor], index: usize) {
    let others: Vec<(f32, f32, f32, f32)> = (0..draft.len())
        .filter(|i| *i != index && placed(&draft[*i]))
        .map(|i| {
            let (w, h) = draft[i].logical();
            (draft[i].x as f32, draft[i].y as f32, w, h)
        })
        .collect();
    if others.is_empty() || !placed(&draft[index]) {
        return;
    }
    let (width, height) = draft[index].logical();
    let (free_x, free_y) = (draft[index].x as f32, draft[index].y as f32);
    let mut spots = Vec::new();
    for (x, y, w, h) in &others {
        for side in [x - width, x + w] {
            for line in [*y, y + h - height, y + (h - height) / 2.0] {
                spots.push((side, line));
            }
        }
        for side in [y - height, y + h] {
            for line in [*x, x + w - width, x + (w - width) / 2.0] {
                spots.push((line, side));
            }
        }
    }
    let mut best: Option<(f32, (f32, f32))> = None;
    for (x, y) in spots {
        if others
            .iter()
            .any(|other| overlap((x, y, width, height), *other))
        {
            continue;
        }
        let away = (x - free_x).hypot(y - free_y);
        if best.is_none_or(|(nearest, _)| away < nearest) {
            best = Some((away, (x, y)));
        }
    }
    let Some((away, (x, y))) = best else {
        log::debug!("'{}' has nowhere to sit", draft[index].label());
        return;
    };
    log::debug!(
        "'{}' settles at {}, {} ({away:.0} px from where it was let go)",
        draft[index].label(),
        x.round(),
        y.round()
    );
    draft[index].x = x.round() as i32;
    draft[index].y = y.round() as i32;
}

/// A copy takes no room on the desktop of its own.
fn placed(monitor: &Monitor) -> bool {
    monitor.connected && monitor.enabled && monitor.mode.is_some() && monitor.mirror.is_empty()
}

fn overlap(one: (f32, f32, f32, f32), two: (f32, f32, f32, f32)) -> bool {
    one.0 < two.0 + two.2 && two.0 < one.0 + one.2 && one.1 < two.1 + two.3 && two.1 < one.1 + one.3
}

fn fields(
    ui: &mut Ui,
    form: &mut Form<'_>,
    draft: &mut [Monitor],
    selected: usize,
    light: &mut Light<'_>,
) {
    let pal = form.pal().clone();
    let names: Vec<String> = draft
        .iter()
        .enumerate()
        .filter(|(i, m)| *i != selected && m.connected)
        .map(|(_, m)| m.name.clone())
        .collect();
    let Some(monitor) = draft.get_mut(selected) else {
        return;
    };
    if !monitor.connected {
        row(ui, &pal, "Status", "saved, not connected now");
        if let Some(mode) = monitor.mode {
            row(ui, &pal, "Resolution", &mode.size());
        }
        row(
            ui,
            &pal,
            "Position",
            &format!("{}, {}", monitor.x, monitor.y),
        );
        return;
    }
    let mut on = monitor.enabled;
    field(ui, &pal, "On", |ui| {
        form.switch(ui, "displays-on", "", &mut on)
    });
    monitor.enabled = on;
    if !monitor.enabled {
        return;
    }

    let mut sizes: Vec<String> = Vec::new();
    for mode in &monitor.modes {
        if !sizes.contains(&mode.size()) {
            sizes.push(mode.size());
        }
    }
    let current = monitor.mode.unwrap_or(Mode {
        width: 0,
        height: 0,
        refresh: 0.0,
    });
    if sizes.is_empty() {
        row(ui, &pal, "Resolution", &current.size());
    } else {
        let labels: Vec<&str> = sizes.iter().map(String::as_str).collect();
        let mut index = sizes.iter().position(|s| *s == current.size()).unwrap_or(0);
        field(ui, &pal, "Resolution", |ui| {
            form.select(ui, "displays-mode", &mut index, &labels, FIELD)
        });
        let size = &sizes[index];
        let refreshes: Vec<&Mode> = monitor
            .modes
            .iter()
            .filter(|mode| mode.size() == *size)
            .collect();
        let texts: Vec<String> = refreshes
            .iter()
            .map(|mode| format!("{:.2} Hz", mode.refresh))
            .collect();
        let labels: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut rate = refreshes
            .iter()
            .position(|mode| (mode.refresh - current.refresh).abs() < 0.1)
            .unwrap_or(0);
        field(ui, &pal, "Refresh rate", |ui| {
            form.select(ui, "displays-refresh", &mut rate, &labels, FIELD)
        });
        monitor.mode = refreshes.get(rate).copied().copied();
    }

    let mut scales: Vec<(String, f32)> = SCALES
        .iter()
        .map(|(label, scale)| ((*label).to_owned(), *scale))
        .collect();
    if !scales
        .iter()
        .any(|(_, s)| (s - monitor.scale).abs() < 0.001)
    {
        scales.push((format!("{:.0} %", monitor.scale * 100.0), monitor.scale));
        scales.sort_by(|a, b| a.1.total_cmp(&b.1));
    }
    let labels: Vec<&str> = scales.iter().map(|(label, _)| label.as_str()).collect();
    let mut index = scales
        .iter()
        .position(|(_, s)| (s - monitor.scale).abs() < 0.001)
        .unwrap_or(0);
    field(ui, &pal, "Scale", |ui| {
        form.select(ui, "displays-scale", &mut index, &labels, FIELD)
    });
    monitor.scale = scales[index].1;

    let labels = ROTATIONS.map(|(label, _)| label);
    let mut index = ROTATIONS
        .iter()
        .position(|(_, transform)| *transform == monitor.transform % 4)
        .unwrap_or(0);
    field(ui, &pal, "Rotation", |ui| {
        form.select(ui, "displays-rotation", &mut index, &labels, FIELD)
    });
    let mut flipped = monitor.transform >= 4;
    field(ui, &pal, "Flipped", |ui| {
        form.check(ui, "displays-flip", "", &mut flipped)
    });
    monitor.transform = ROTATIONS[index].1 + 4 * u8::from(flipped);

    let mut vrr = monitor.vrr;
    field(ui, &pal, "Variable refresh", |ui| {
        form.check(ui, "displays-vrr", "", &mut vrr)
    });
    monitor.vrr = vrr;

    if light.backlight.is_some() {
        if let Some(error) = light.error {
            ui.label(RichText::new(error).color(pal.danger));
        }
        if light.auto {
            row(
                ui,
                &pal,
                "Brightness",
                &format!("{:.0} %", light.level * 100.0),
            );
        } else {
            let mut level = light.level;
            field(ui, &pal, "Brightness", |ui| {
                if form.slider(ui, "displays-brightness", &mut level, FIELD) {
                    light.set = Some(level);
                }
                ui.label(RichText::new(format!("{:.0} %", level * 100.0)).color(pal.text));
            });
        }
        if light.sensor {
            let mut auto = light.auto;
            field(ui, &pal, "Automatic brightness", |ui| {
                form.check(ui, "displays-auto", "", &mut auto)
            });
            light.auto = auto;
        }
    }

    let mut options = vec!["Nothing".to_owned()];
    options.extend(names);
    let labels: Vec<&str> = options.iter().map(String::as_str).collect();
    let mut index = options
        .iter()
        .position(|name| *name == monitor.mirror)
        .unwrap_or(0);
    field(ui, &pal, "Shows a copy of", |ui| {
        form.select(ui, "displays-mirror", &mut index, &labels, FIELD)
    });
    monitor.mirror = if index == 0 {
        String::new()
    } else {
        options[index].clone()
    };

    row(
        ui,
        &pal,
        "Position",
        &format!("{}, {}", monitor.x, monitor.y),
    );
    if !monitor.name.is_empty() && !monitor.description.is_empty() {
        row(ui, &pal, "Output", &monitor.name);
    }
}

fn field<R>(ui: &mut Ui, pal: &Palette, text: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        form::label(ui, pal, text, LABEL);
        add(ui)
    })
    .inner
}

fn row(ui: &mut Ui, pal: &Palette, text: &str, value: &str) {
    ui.horizontal(|ui| {
        form::label(ui, pal, text, LABEL);
        ui.add(egui::Label::new(RichText::new(value).color(pal.text)).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32, y: i32, width: u32, height: u32) -> Monitor {
        Monitor {
            name: name.to_owned(),
            description: format!("a {name}"),
            connected: true,
            enabled: true,
            mode: Some(Mode {
                width,
                height,
                refresh: 60.0,
            }),
            x,
            y,
            scale: 1.0,
            transform: 0,
            vrr: false,
            mirror: String::new(),
            modes: Vec::new(),
        }
    }

    #[test]
    fn every_screen_gets_a_row_of_its_own_under_its_header() {
        let mut saved = monitor("DP-9", -4000, 0, 1920, 1080);
        saved.connected = false;
        let draft = vec![
            monitor("eDP-1", 0, 0, 2880, 1920),
            monitor("DP-4", -2560, -480, 2560, 1440),
            saved,
        ];
        let lines = lines(&draft);
        let headers: Vec<&str> = lines.iter().filter_map(|line| line.header).collect();
        assert_eq!(headers, ["Connected", "Not connected"]);
        let shown: Vec<usize> = lines
            .iter()
            .filter(|line| line.header.is_none())
            .map(|line| line.monitor)
            .collect();
        assert_eq!(shown, [0, 1, 2]);
    }

    #[test]
    fn a_screen_dropped_on_another_one_ends_up_beside_it() {
        let mut draft = vec![
            monitor("eDP-1", 0, 0, 2880, 1920),
            monitor("DP-4", 200, 300, 2560, 1440),
        ];
        settle(&mut draft, 1);
        assert!(!overlap(
            (0.0, 0.0, 2880.0, 1920.0),
            (
                draft[1].x as f32,
                draft[1].y as f32,
                draft[1].logical().0,
                draft[1].logical().1
            )
        ));
        let touches =
            draft[1].x == 2880 || draft[1].x == -2560 || draft[1].y == 1920 || draft[1].y == -1440;
        assert!(touches, "sits at {}, {}", draft[1].x, draft[1].y);
    }

    #[test]
    fn a_screen_dropped_far_away_is_pulled_back_against_its_neighbour() {
        let mut draft = vec![
            monitor("eDP-1", 0, 0, 2880, 1920),
            monitor("DP-4", 9000, 40, 2560, 1440),
        ];
        settle(&mut draft, 1);
        assert_eq!((draft[1].x, draft[1].y), (2880, 0));
    }
}
