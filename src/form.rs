use egui::{
    Align, Align2, Color32, CornerRadius, Event, FontId, Frame, Id, Key as EguiKey, Margin, Order,
    Pos2, RawInput, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2, vec2,
};
use fastframe_fonts::Weight;

use crate::theme::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Next,
    Prev,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Space,
    Escape,
}

// egui must never see these keys, or its own focus handling fights ours.
pub fn take_keys(raw: &mut RawInput, in_entry: bool) -> Vec<Key> {
    let mut keys = Vec::new();
    raw.events.retain(|event| {
        let (key, modifiers) = match event {
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => (*key, *modifiers),
            Event::Text(text) if text == " " => return in_entry,
            _ => return true,
        };
        if modifiers.ctrl || modifiers.alt || modifiers.command {
            return true;
        }
        let nav = match key {
            EguiKey::Tab if modifiers.shift => Key::Prev,
            EguiKey::Tab => Key::Next,
            EguiKey::ArrowUp => Key::Up,
            EguiKey::ArrowDown => Key::Down,
            EguiKey::Enter => Key::Enter,
            EguiKey::Escape => Key::Escape,
            EguiKey::PageUp => Key::PageUp,
            EguiKey::PageDown => Key::PageDown,
            _ if in_entry => return true,
            EguiKey::ArrowLeft => Key::Left,
            EguiKey::ArrowRight => Key::Right,
            EguiKey::Home => Key::Home,
            EguiKey::End => Key::End,
            EguiKey::Space => Key::Space,
            _ => return true,
        };
        keys.push(nav);
        false
    });
    keys
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Entry,
    Other,
}

#[derive(Default)]
pub struct FormState {
    pub focus: Option<String>,
    reveal: bool,
    popup: Option<Popup>,
    chosen: Option<(String, usize)>,
}

impl FormState {
    pub fn focused(focus: &str) -> Self {
        Self {
            focus: Some(focus.to_owned()),
            ..Self::default()
        }
    }
}

struct Popup {
    id: String,
    options: Vec<String>,
    highlighted: usize,
    anchor: Rect,
    // The click that opened it is still in this frame's input, and it landed on
    // the field, not on the popup, so dismissing starts one frame later.
    opened_this_frame: bool,
}

fastframe_icons::icons! {
    pub enum Icon {
        prefix: "settings-dialog-icon-",
        directory: "../icons/",
        Trash => lucide "trash-2",
        Reset => "rotate-ccw",
        Save => "save",
        Plus => lucide "plus",
        Reload => lucide "refresh-cw",
        Radar => "radar",
        Plug => "plug",
        Unplug => "unplug",
        Pair => "bluetooth-connected",
        Star => "star",
        Speaker => "volume-2",
        Muted => "volume-x",
        Find => "audio-lines",
        Stop => lucide "square",
        Kill => lucide "x",
        Graph => "workflow",
        Tree => "list-tree",
        List => "list",
        Network => "network",
        Tablet => "tablet",
        ChevronDown => lucide "chevron-down",
        ChevronUp => lucide "chevron-up",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Row,
    Toggle,
}

pub enum Row<'a> {
    Header(&'a str),
    Item {
        text: &'a str,
        /// Dim text right-aligned before the signal bars.
        detail: Option<&'a str>,
        signal: Option<u8>,
        switch: Option<bool>,
        /// The lines in front of a row in a tree, one per level; empty outside one.
        tree: &'a [Guide],
        /// Right-aligned values at the right end, under a `table`'s columns.
        columns: &'a [String],
        /// Links for the name and then each column, by position; empty for none.
        links: &'a [Option<String>],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guide {
    /// An ancestor here has no more siblings below.
    Blank,
    /// An ancestor here has more siblings below.
    Line,
    /// The row's own branch, with more siblings below.
    Tee,
    /// The row's own branch, as the last sibling.
    Corner,
}

const TREE_INDENT: f32 = 20.0;
/// A value column's width when a plain `list` row has columns.
const COLUMN: f32 = 100.0;
const ROW_INSET: f32 = 6.0;
/// The room between a value and the one left of it.
const COLUMN_GAP: f32 = 20.0;
/// Between two column heads, as wide as their margin to the border.
const HEAD_GAP: f32 = 4.0;
pub const HEADS_HEIGHT: f32 = 30.0;

/// The column heads of a `table`: the name heads the row text, then the
/// right-aligned value columns with the most room each may take. A column
/// shrinks to its widest value; longer values end in "…".
pub struct Table<'a> {
    pub name: &'a str,
    pub columns: &'a [(&'a str, f32)],
}

/// Column 0 is the name, then the value columns in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

/// Sorts the rows between section headers, so each group stays under its own.
pub fn sort_groups<T>(
    lines: &mut [T],
    header: impl Fn(&T) -> bool,
    sort: Sort,
    order: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    for group in lines.split_mut(header) {
        group.sort_by(|a, b| match sort.descending {
            true => order(b, a),
            false => order(a, b),
        });
    }
}

/// The same column again flips the direction; a new one starts A–Z for the
/// name and largest first for the rest.
fn pick(sort: &mut Option<Sort>, column: usize) {
    *sort = Some(match *sort {
        Some(now) if now.column == column => Sort {
            column,
            descending: !now.descending,
        },
        _ => Sort {
            column,
            descending: column != 0,
        },
    });
}

fn log_sort(id: &str, labels: &[&str], before: Option<Sort>, sort: Option<Sort>) {
    let Some(sort) = sort.filter(|sort| Some(*sort) != before) else {
        return;
    };
    let direction = if sort.descending {
        "descending"
    } else {
        "ascending"
    };
    log::info!("{id} sorted by {}, {direction}", labels[sort.column]);
}

/// Text order ignoring case.
pub fn by_text(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.chars(), b.chars());
    a.flat_map(char::to_lowercase)
        .cmp(b.flat_map(char::to_lowercase))
}

/// Where a row's value columns go. In a table every row keeps room for signal
/// bars and a switch if any row has them, so the columns line up.
struct Layout<'a> {
    signal: bool,
    switch: bool,
    widths: &'a [f32],
}

impl Layout<'_> {
    /// Where the signal bars start, and where the last value column ends.
    fn trailing(&self, row: Rect) -> (f32, f32) {
        let signal_x = match self.switch {
            true => row.right() - 80.0,
            false => row.right() - 32.0,
        };
        let right = match (self.signal, self.switch) {
            (true, _) => signal_x - 8.0,
            (false, true) => switch_rect(row).left() - 8.0,
            (false, false) => row.right() - ROW_INSET,
        };
        (signal_x, right)
    }

    fn width(&self, column: usize) -> f32 {
        self.widths.get(column).copied().unwrap_or(COLUMN)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Plain,
    Hovered,
    Selected,
    /// Selected in the focused list.
    Lit,
}

impl Row<'_> {
    fn selectable(&self) -> bool {
        matches!(self, Self::Item { .. })
    }

    fn switch(&self) -> Option<bool> {
        match self {
            Self::Item { switch, .. } => *switch,
            Self::Header(_) => None,
        }
    }
}

pub const ROW_HEIGHT: f32 = 28.0;
pub const FIELD_HEIGHT: f32 = 26.0;
const MARGIN: Vec2 = vec2(28.0, 20.0);

pub struct Form<'a> {
    state: &'a mut FormState,
    keys: &'a [Key],
    pal: &'a Palette,
    order: Vec<(String, Kind)>,
    step: i32,
    escape: bool,
}

impl<'a> Form<'a> {
    pub fn new(state: &'a mut FormState, keys: &'a [Key], pal: &'a Palette) -> Self {
        let mut escape = false;
        let mut popup_keys = false;
        if let Some(popup) = &mut state.popup {
            popup_keys = true;
            let count = popup.options.len();
            for key in keys {
                match key {
                    Key::Up | Key::Prev | Key::Left => {
                        popup.highlighted = popup.highlighted.saturating_sub(1)
                    }
                    Key::Down | Key::Next | Key::Right => {
                        popup.highlighted = (popup.highlighted + 1).min(count - 1)
                    }
                    Key::Home | Key::PageUp => popup.highlighted = 0,
                    Key::End | Key::PageDown => popup.highlighted = count - 1,
                    Key::Enter | Key::Space => {
                        state.chosen = Some((popup.id.clone(), popup.highlighted));
                        state.popup = None;
                        break;
                    }
                    Key::Escape => {
                        state.popup = None;
                        break;
                    }
                }
            }
        } else {
            escape = keys.contains(&Key::Escape);
        }
        Self {
            state,
            keys: if popup_keys { &[] } else { keys },
            pal,
            order: Vec::new(),
            step: 0,
            escape,
        }
    }

    pub fn pal(&self) -> &Palette {
        self.pal
    }

    pub fn escape(&self) -> bool {
        self.escape
    }

    fn register(&mut self, id: &str, kind: Kind) -> bool {
        if self.state.focus.is_none() {
            self.state.focus = Some(id.to_owned());
        }
        self.order.push((id.to_owned(), kind));
        self.state.focus.as_deref() == Some(id)
    }

    fn pressed(&self, key: Key) -> bool {
        self.keys.contains(&key)
    }

    fn count(&self, key: Key) -> usize {
        self.keys.iter().filter(|k| **k == key).count()
    }

    fn click(&mut self, id: &str) {
        self.state.focus = Some(id.to_owned());
    }

    /// How many widgets have been drawn so far; see `focus_since`.
    pub fn mark(&self) -> usize {
        self.order.len()
    }

    /// Whether the focus is on a widget drawn after `mark`.
    pub fn focus_since(&self, mark: usize) -> bool {
        let focus = self.state.focus.as_deref();
        self.order[mark..]
            .iter()
            .any(|(id, _)| Some(id.as_str()) == focus)
    }

    pub fn focus(&mut self, id: &str) {
        self.state.focus = Some(id.to_owned());
        self.state.reveal = true;
    }

    fn reveal(&mut self, ui: &Ui, rect: Rect, focused: bool) {
        if focused && self.state.reveal {
            ui.scroll_to_rect(rect.expand(8.0), None);
            self.state.reveal = false;
        }
    }

    fn arrows(&mut self) {
        self.step -= (self.count(Key::Up) + self.count(Key::Left)) as i32;
        self.step += (self.count(Key::Down) + self.count(Key::Right)) as i32;
    }

    pub fn button(&mut self, ui: &mut Ui, id: &str, label: &str) -> bool {
        let focused = self.register(id, Kind::Other);
        let font = Weight::Medium.font_id(14.0);
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), font, self.pal.text);
        let size = vec2((galley.size().x + 28.0).max(72.0), FIELD_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        self.reveal(ui, rect, focused);
        let (fill, text) = if focused {
            (self.pal.accent, self.pal.on_accent)
        } else if response.hovered() {
            (self.pal.surface_hover, self.pal.text)
        } else {
            (self.pal.surface, self.pal.text)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(3), fill);
        painter.galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, text);
        if response.clicked() {
            self.click(id);
            return true;
        }
        if focused {
            if self.pressed(Key::Enter) || self.pressed(Key::Space) {
                return true;
            }
            self.arrows();
        }
        false
    }

    pub fn tabs(&mut self, ui: &mut Ui, id: &str, labels: &[&str], index: &mut usize) -> bool {
        let focused = self.register(id, Kind::Other);
        let before = *index;
        let mut clicked = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (tab, label) in labels.iter().enumerate() {
                let galley = ui.painter().layout_no_wrap(
                    label.to_string(),
                    Weight::Medium.font_id(14.0),
                    self.pal.text,
                );
                let size = vec2(galley.size().x + 28.0, FIELD_HEIGHT);
                let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                let current = tab == *index;
                if current {
                    self.reveal(ui, rect, focused);
                }
                let (fill, text) = if current && focused {
                    (self.pal.accent, self.pal.on_accent)
                } else if current {
                    (self.pal.surface_active, self.pal.text)
                } else if response.hovered() {
                    (self.pal.surface_hover, self.pal.text)
                } else {
                    (self.pal.surface, self.pal.secondary)
                };
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(3), fill);
                painter.galley_with_override_text_color(
                    rect.center() - galley.size() / 2.0,
                    galley,
                    text,
                );
                if response.clicked() {
                    *index = tab;
                    clicked = true;
                }
            }
        });
        if clicked {
            self.click(id);
        }
        if focused {
            for key in self.keys {
                match key {
                    Key::Left => *index = index.saturating_sub(1),
                    Key::Right => *index = (*index + 1).min(labels.len() - 1),
                    _ => {}
                }
            }
            self.step += self.count(Key::Down) as i32 - self.count(Key::Up) as i32;
        }
        *index != before
    }

    pub fn check(&mut self, ui: &mut Ui, id: &str, label: &str, value: &mut bool) -> bool {
        let focused = self.register(id, Kind::Other);
        let font = FontId::proportional(14.0);
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), font, self.pal.text);
        let size = vec2(18.0 + 8.0 + galley.size().x, FIELD_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        self.reveal(ui, rect, focused);
        let boxed =
            Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(16.0, 16.0));
        let painter = ui.painter();
        let (fill, mark) = if focused {
            (self.pal.accent, self.pal.on_accent)
        } else {
            (self.pal.window, self.pal.accent)
        };
        painter.rect_filled(boxed, CornerRadius::same(2), fill);
        let edge = if focused || response.hovered() {
            self.pal.accent
        } else {
            self.pal.dim
        };
        painter.rect_stroke(
            boxed,
            CornerRadius::same(2),
            Stroke::new(1.0, edge),
            StrokeKind::Inside,
        );
        if *value {
            let stroke = Stroke::new(2.0, mark);
            let (l, c, r) = (
                pos2(boxed.left() + 3.5, boxed.center().y),
                pos2(boxed.left() + 6.5, boxed.bottom() - 4.0),
                pos2(boxed.right() - 3.0, boxed.top() + 4.0),
            );
            painter.line_segment([l, c], stroke);
            painter.line_segment([c, r], stroke);
        }
        let color = if focused {
            self.pal.accent
        } else {
            self.pal.text
        };
        painter.galley_with_override_text_color(
            pos2(boxed.right() + 8.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        let mut toggle = response.clicked();
        if response.clicked() {
            self.click(id);
        }
        if focused {
            toggle |= self.pressed(Key::Enter) || self.pressed(Key::Space);
            self.arrows();
        }
        if toggle {
            *value = !*value;
        }
        toggle
    }

    /// Icon buttons and an optional switch in the window's top-right corner;
    /// returns the index of the pressed button.
    pub fn top_bar(
        &mut self,
        ctx: &egui::Context,
        busy: bool,
        buttons: &[(&str, Icon, &str)],
        switch: Option<(&str, &str, &mut bool)>,
    ) -> Option<usize> {
        let corner = ctx.content_rect().right_top() + vec2(-MARGIN.x, MARGIN.y);
        egui::Area::new(Id::new("top-bar"))
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(corner)
            .movable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if busy {
                        ui.add(egui::Spinner::new().color(self.pal.accent));
                    }
                    let mut pressed = None;
                    for (index, (id, icon, tip)) in buttons.iter().enumerate() {
                        if self.icon_button(ui, id, *icon, tip) {
                            pressed = Some(index);
                        }
                    }
                    if let Some((id, label, on)) = switch {
                        ui.add_space(12.0);
                        self.switch(ui, id, label, on);
                    }
                    pressed
                })
                .inner
            })
            .inner
    }

    pub fn icon_button(&mut self, ui: &mut Ui, id: &str, icon: Icon, tip: &str) -> bool {
        let focused = self.register(id, Kind::Other);
        let (rect, response) =
            ui.allocate_exact_size(vec2(FIELD_HEIGHT, FIELD_HEIGHT), Sense::click());
        let response = response.on_hover_text(tip);
        let (fill, tint) = if focused {
            (self.pal.accent, self.pal.on_accent)
        } else if response.hovered() {
            (self.pal.surface_hover, self.pal.text)
        } else {
            (self.pal.surface, self.pal.text)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
        icon.image(tint, 16.0)
            .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(16.0)));
        if response.clicked() {
            self.click(id);
            return true;
        }
        if focused {
            if self.pressed(Key::Enter) || self.pressed(Key::Space) {
                return true;
            }
            self.arrows();
        }
        false
    }

    /// Registers a widget that moves around inside itself; while it has the
    /// focus it gets the keys, and Tab still leaves it.
    pub fn custom(&mut self, id: &str) -> Option<Vec<Key>> {
        let focused = self.register(id, Kind::Other);
        focused.then(|| self.keys.to_vec())
    }

    /// A 0–1 slider: click or drag it, or step it by 5 % with ← and →;
    /// Home and End go to the ends.
    pub fn slider(&mut self, ui: &mut Ui, id: &str, value: &mut f32, width: f32) -> bool {
        let focused = self.register(id, Kind::Other);
        let before = *value;
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, FIELD_HEIGHT), Sense::click_and_drag());
        self.reveal(ui, rect, focused);
        let track = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.center().y - 3.0),
            pos2(rect.right() - 8.0, rect.center().y + 3.0),
        );
        if let Some(pointer) = response.interact_pointer_pos()
            && (response.is_pointer_button_down_on() || response.clicked())
        {
            self.click(id);
            *value = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
        }
        if focused {
            for key in self.keys {
                match key {
                    Key::Left => *value = (((*value * 20.0 - 0.01).ceil() - 1.0) / 20.0).max(0.0),
                    Key::Right => *value = (((*value * 20.0 + 0.01).floor() + 1.0) / 20.0).min(1.0),
                    Key::Home => *value = 0.0,
                    Key::End => *value = 1.0,
                    _ => {}
                }
            }
            self.step += self.count(Key::Down) as i32 - self.count(Key::Up) as i32;
        }

        let painter = ui.painter();
        painter.rect_filled(track, CornerRadius::same(3), self.pal.surface);
        let x = track.left() + track.width() * value.clamp(0.0, 1.0);
        let filled = Rect::from_min_max(track.min, pos2(x, track.bottom()));
        let active = focused || response.hovered() || response.dragged();
        let fill = if active {
            self.pal.accent
        } else {
            self.pal.secondary
        };
        painter.rect_filled(filled, CornerRadius::same(3), fill);
        painter.circle_filled(pos2(x, track.center().y), 7.0, fill);
        if focused {
            painter.circle_stroke(
                pos2(x, track.center().y),
                9.0,
                Stroke::new(1.5, self.pal.accent),
            );
        }
        *value != before
    }

    pub fn switch(&mut self, ui: &mut Ui, id: &str, label: &str, on: &mut bool) -> bool {
        let focused = self.register(id, Kind::Other);
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            Weight::SemiBold.font_id(14.0),
            self.pal.text,
        );
        let size = vec2(36.0 + 8.0 + galley.size().x + 8.0, FIELD_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let track =
            Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(36.0, 20.0));
        let painter = ui.painter();
        paint_switch(painter, self.pal, track, *on, false);
        if focused || response.hovered() {
            painter.rect_stroke(
                track.expand(2.0),
                CornerRadius::same(12),
                Stroke::new(1.5, self.pal.accent),
                StrokeKind::Outside,
            );
        }
        let color = if focused {
            self.pal.accent
        } else {
            self.pal.text
        };
        painter.galley_with_override_text_color(
            pos2(track.right() + 8.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        let mut toggle = response.clicked();
        if response.clicked() {
            self.click(id);
        }
        if focused {
            toggle |= self.pressed(Key::Enter) || self.pressed(Key::Space);
            self.arrows();
        }
        if toggle {
            *on = !*on;
        }
        toggle
    }

    pub fn entry(
        &mut self,
        ui: &mut Ui,
        id: &str,
        text: &mut String,
        width: f32,
        password: bool,
    ) -> bool {
        let focused = self.register(id, Kind::Entry);
        let stroke = if focused {
            self.pal.accent
        } else {
            self.pal.outline
        };
        let inner = Frame::new()
            .fill(self.pal.window)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(CornerRadius::same(3))
            .inner_margin(Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.add(
                    TextEdit::singleline(text)
                        .id(Id::new(("entry", id)))
                        .frame(Frame::NONE)
                        .password(password)
                        .desired_width(width - 14.0)
                        .text_color(self.pal.text),
                )
            });
        let response = inner.inner;
        self.reveal(ui, inner.response.rect, focused);
        if response.clicked() || response.gained_focus() {
            self.click(id);
        } else if focused {
            if !response.has_focus() {
                response.request_focus();
            }
            if self.pressed(Key::Enter) || self.pressed(Key::Down) {
                self.step += 1;
            }
            if self.pressed(Key::Up) {
                self.step -= 1;
            }
        } else if response.has_focus() {
            response.surrender_focus();
        }
        response.changed()
    }

    pub fn select(
        &mut self,
        ui: &mut Ui,
        id: &str,
        selected: &mut usize,
        options: &[&str],
        width: f32,
    ) -> bool {
        let focused = self.register(id, Kind::Other);
        let mut changed = false;
        if let Some((chosen_id, index)) = &self.state.chosen
            && chosen_id == id
        {
            changed = *selected != *index;
            *selected = *index;
            self.state.chosen = None;
        }
        let label = options.get(*selected).copied().unwrap_or("");
        let (rect, response) = ui.allocate_exact_size(vec2(width, FIELD_HEIGHT), Sense::click());
        self.reveal(ui, rect, focused);
        let open = self.state.popup.as_ref().is_some_and(|p| p.id == id);
        let (fill, text) = if focused && !open {
            (self.pal.accent, self.pal.on_accent)
        } else if response.hovered() || open {
            (self.pal.surface_hover, self.pal.text)
        } else {
            (self.pal.surface, self.pal.text)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(3), fill);
        painter.text(
            pos2(rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            Weight::Medium.font_id(14.0),
            text,
        );
        let tip = pos2(rect.right() - 14.0, rect.center().y + 3.0);
        painter.add(egui::Shape::convex_polygon(
            vec![tip, tip + vec2(-4.5, -6.0), tip + vec2(4.5, -6.0)],
            text,
            Stroke::NONE,
        ));
        let mut open_it = response.clicked();
        if response.clicked() {
            self.click(id);
        }
        if focused {
            open_it |= self.pressed(Key::Enter) || self.pressed(Key::Space);
            self.arrows();
        }
        if open_it && !open {
            log::debug!("select '{id}' opens with {} options", options.len());
            self.state.popup = Some(Popup {
                id: id.to_owned(),
                options: options.iter().map(|o| (*o).to_owned()).collect(),
                highlighted: *selected,
                anchor: rect,
                opened_this_frame: true,
            });
        }
        changed
    }

    pub fn list(
        &mut self,
        ui: &mut Ui,
        id: &str,
        rows: &[Row<'_>],
        selected: &mut usize,
        size: Vec2,
    ) -> Option<Press> {
        self.rows(ui, id, rows, selected, size, None)
    }

    /// A list under column heads; clicking a head sorts by that column, again
    /// flips the direction. With the focus on the heads, ← and → pick a column
    /// and Space or Enter flips it. The caller sorts the rows by `sort`, and
    /// `None` keeps its own order. Fills the space left in `ui`.
    pub fn table(
        &mut self,
        ui: &mut Ui,
        id: &str,
        table: &Table<'_>,
        sort: &mut Option<Sort>,
        rows: &[Row<'_>],
        selected: &mut usize,
    ) -> Option<Press> {
        let measure = |text: &str, font: FontId| {
            let galley = ui
                .painter()
                .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
            galley.size().x
        };
        let widths: Vec<f32> = (table.columns.iter().enumerate())
            .map(|(column, (label, most))| {
                // Room for the chevron at the head's left edge, even in a column capped narrower.
                let head = measure(label, Weight::SemiBold.font_id(13.0))
                    + 25.0
                    + 2.0 * ROW_INSET
                    + HEAD_GAP;
                let widest = rows
                    .iter()
                    .filter_map(|row| match row {
                        Row::Item { columns, .. } => columns.get(column),
                        Row::Header(_) => None,
                    })
                    .map(|value| measure(value, FontId::proportional(13.0)))
                    .fold(head, f32::max);
                (widest + COLUMN_GAP).min(*most).max(head)
            })
            .collect();
        let layout = Layout {
            signal: rows.iter().any(|row| {
                matches!(
                    row,
                    Row::Item {
                        signal: Some(_),
                        ..
                    }
                )
            }),
            switch: rows.iter().any(|row| row.switch().is_some()),
            widths: &widths,
        };
        let area = ui.available_rect_before_wrap();
        let (heads, list) = area.split_top_bottom_at_y(area.top() + HEADS_HEIGHT);
        ui.painter()
            .rect_filled(heads, CornerRadius::same(3), self.pal.window);
        self.heads(ui, id, table, &layout, heads, sort);
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            self.rows(ui, id, rows, selected, list.size(), Some(&layout))
        })
        .inner
    }

    fn heads(
        &mut self,
        ui: &mut Ui,
        id: &str,
        table: &Table<'_>,
        layout: &Layout<'_>,
        rect: Rect,
        sort: &mut Option<Sort>,
    ) {
        let before = *sort;
        let count = table.columns.len() + 1;
        let focused = self.sort_keys(id, count, sort);

        // Lined up with the rows inside the list's frame: 1 stroke and 4 margin.
        let row = rect.shrink(5.0);
        let (_, right) = layout.trailing(row);
        let values = table.columns.len();
        let end_of =
            |value: usize| right - (value + 1..values).map(|c| layout.width(c)).sum::<f32>();
        let names_end = end_of(0) - layout.width(0) + ROW_INSET;
        for column in 0..count {
            let (label, cell) = match column {
                0 => (
                    table.name,
                    Rect::from_min_max(row.min, pos2(names_end - HEAD_GAP, row.bottom())),
                ),
                _ => {
                    let end = end_of(column - 1);
                    let left = end - layout.width(column - 1) + ROW_INSET + HEAD_GAP;
                    let cell = Rect::from_min_max(
                        pos2(left, row.top()),
                        pos2(end + ROW_INSET, row.bottom()),
                    );
                    (table.columns[column - 1].0, cell)
                }
            };
            self.head(
                ui,
                id,
                cell,
                column,
                label,
                column > 0,
                before,
                focused,
                sort,
            );
        }
        let labels: Vec<&str> = std::iter::once(table.name)
            .chain(table.columns.iter().map(|(label, _)| *label))
            .collect();
        log_sort(id, &labels, before, *sort);
    }

    /// Takes the keys while the heads have the focus; returns whether they have it.
    fn sort_keys(&mut self, id: &str, count: usize, sort: &mut Option<Sort>) -> bool {
        let Some(keys) = self.custom(&format!("{id}-sort")) else {
            return false;
        };
        for key in keys {
            let column = sort.map_or(0, |sort| sort.column);
            match key {
                Key::Left => pick(sort, column.saturating_sub(1)),
                Key::Right => pick(sort, (column + 1).min(count - 1)),
                Key::Enter | Key::Space => pick(sort, column),
                _ => {}
            }
        }
        true
    }

    /// One column head: sorts on a click and shows the direction with a chevron.
    #[allow(clippy::too_many_arguments)]
    fn head(
        &mut self,
        ui: &mut Ui,
        id: &str,
        cell: Rect,
        column: usize,
        label: &str,
        right_aligned: bool,
        before: Option<Sort>,
        focused: bool,
        sort: &mut Option<Sort>,
    ) {
        let heads_id = format!("{id}-sort");
        let response = ui.interact(cell, ui.id().with((&heads_id, column)), Sense::click());
        if response.clicked() {
            self.click(&heads_id);
            pick(sort, column);
        }
        let pal = self.pal;
        let current = before.filter(|sort| sort.column == column);
        let (fill, color) = match (current.is_some(), focused, response.hovered()) {
            (true, true, _) => (Some(pal.accent), pal.on_accent),
            (false, true, _) if before.is_none() && column == 0 => {
                (Some(pal.accent), pal.on_accent)
            }
            (_, _, true) => (Some(pal.surface_hover), pal.text),
            (true, _, _) => (None, pal.text),
            _ => (None, pal.secondary),
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(cell, CornerRadius::same(3), fill);
        }
        let galley =
            ui.painter()
                .layout_no_wrap(label.to_owned(), Weight::SemiBold.font_id(13.0), color);
        let width = galley.size().x;
        let chevron = if current.is_some() { 18.0 } else { 0.0 };
        let text_x = match right_aligned {
            false => cell.left() + 7.0 + chevron,
            true => cell.right() - ROW_INSET - width,
        };
        let chevron_x = cell.left() + 7.0;
        let y = cell.center().y - galley.size().y / 2.0;
        ui.painter().galley(pos2(text_x, y), galley, color);
        if let Some(current) = current {
            let icon = match current.descending {
                true => Icon::ChevronDown,
                false => Icon::ChevronUp,
            };
            let at = Rect::from_min_size(pos2(chevron_x, cell.center().y - 7.0), Vec2::splat(14.0));
            icon.image(color, 14.0).paint_at(ui, at);
        }
    }

    fn rows(
        &mut self,
        ui: &mut Ui,
        id: &str,
        rows: &[Row<'_>],
        selected: &mut usize,
        size: Vec2,
        table: Option<&Layout<'_>>,
    ) -> Option<Press> {
        let focused = self.register(id, Kind::Other);
        let selectable: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].selectable()).collect();
        if !selectable.contains(selected) {
            *selected = selectable.first().copied().unwrap_or(0);
        }
        let mut moved = false;
        let mut entered = false;
        if focused && !selectable.is_empty() {
            let mut pos = selectable.iter().position(|i| i == selected).unwrap_or(0);
            let last = selectable.len() - 1;
            for key in self.keys {
                let before = pos;
                match key {
                    Key::Up => pos = pos.saturating_sub(1),
                    Key::Down => pos = (pos + 1).min(last),
                    Key::PageUp => pos = pos.saturating_sub(8),
                    Key::PageDown => pos = (pos + 8).min(last),
                    Key::Home => pos = 0,
                    Key::End => pos = last,
                    Key::Enter | Key::Space => entered = true,
                    Key::Left => self.step -= 1,
                    Key::Right => self.step += 1,
                    _ => {}
                }
                moved |= before != pos;
            }
            *selected = selectable[pos];
        }

        let pal = self.pal;
        let mut clicked: Option<(usize, Option<Press>)> = None;
        let border = Stroke::new(1.0, if focused { pal.accent } else { pal.outline });
        let frame = Frame::new()
            .fill(pal.window)
            .stroke(if table.is_some() {
                Stroke::NONE
            } else {
                border
            })
            .corner_radius(CornerRadius::same(3))
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                let inner = (size - vec2(10.0, 10.0)).max(Vec2::ZERO);
                ui.set_min_size(inner);
                ui.set_max_width(inner.x);
                egui::ScrollArea::vertical()
                    .id_salt(("list", id))
                    .max_height(inner.y)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (index, row) in rows.iter().enumerate() {
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), ROW_HEIGHT),
                                    if row.selectable() {
                                        Sense::click()
                                    } else {
                                        Sense::hover()
                                    },
                                );
                                if response.clicked() {
                                    let double = response.double_clicked();
                                    clicked = Some((index, double.then_some(Press::Row)));
                                }
                                let mut switch_hovered = false;
                                if row.switch().is_some() {
                                    let hit = ui.interact(
                                        switch_rect(rect),
                                        ui.id().with((id, index)),
                                        Sense::click(),
                                    );
                                    switch_hovered = hit.hovered();
                                    if hit.clicked() {
                                        clicked = Some((index, Some(Press::Toggle)));
                                    }
                                }
                                let current = index == *selected;
                                let hovered = response.hovered() || switch_hovered;
                                let look = match (current, focused, hovered) {
                                    (true, true, _) => Look::Lit,
                                    (true, false, _) => Look::Selected,
                                    (false, _, true) => Look::Hovered,
                                    _ => Look::Plain,
                                };
                                let own = Layout {
                                    signal: matches!(
                                        row,
                                        Row::Item {
                                            signal: Some(_),
                                            ..
                                        }
                                    ),
                                    switch: row.switch().is_some(),
                                    widths: &[],
                                };
                                let row_id = ui.id().with((id, "row", index));
                                draw_row(ui, pal, row_id, rect, row, table.unwrap_or(&own), look);
                                if current && (moved || self.state.reveal && focused) {
                                    ui.scroll_to_rect(rect, None);
                                }
                            }
                        })
                    });
            });
        // A table's border goes around its heads too, with a line below them.
        if table.is_some() {
            let list = frame.response.rect;
            let outer = Rect::from_min_max(pos2(list.left(), list.top() - HEADS_HEIGHT), list.max);
            let painter = ui.painter();
            painter.hline(outer.x_range(), list.top(), Stroke::new(1.0, pal.outline));
            painter.rect_stroke(
                outer,
                CornerRadius::same(3),
                border,
                egui::StrokeKind::Inside,
            );
        }
        if frame.response.clicked() {
            self.click(id);
        }
        if focused {
            self.state.reveal = false;
        }
        match clicked {
            Some((index, press)) => {
                *selected = index;
                self.click(id);
                press
            }
            None => entered.then_some(Press::Row),
        }
    }

    pub fn finish(mut self, ctx: &egui::Context) -> bool {
        if self.state.popup.is_none() {
            self.step += self.count(Key::Next) as i32 - self.count(Key::Prev) as i32;
        }
        if !self.order.is_empty() {
            let current = self
                .state
                .focus
                .as_deref()
                .and_then(|focus| self.order.iter().position(|(id, _)| id == focus));
            let len = self.order.len() as i32;
            let next = match current {
                Some(index) => (index as i32 + self.step).rem_euclid(len) as usize,
                None => 0,
            };
            if current != Some(next) {
                self.state.focus = Some(self.order[next].0.clone());
                self.state.reveal = true;
                ctx.request_repaint();
            }
        }
        self.popup(ctx);
        let focus = self.state.focus.as_deref();
        self.order
            .iter()
            .any(|(id, kind)| Some(id.as_str()) == focus && *kind == Kind::Entry)
    }

    fn popup(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let Some(popup) = &mut self.state.popup else {
            return;
        };
        let mut chosen = None;
        let opened_this_frame = std::mem::take(&mut popup.opened_this_frame);
        // It opens downwards, but upwards when the options don't fit below the
        // field, and it scrolls when they fit neither way, so a field near the
        // bottom of the window still offers all of them.
        let screen = ctx.input(|input| input.viewport_rect());
        let wanted = popup.options.len() as f32 * ROW_HEIGHT + 8.0;
        let below = screen.bottom() - popup.anchor.bottom() - 4.0;
        let above = popup.anchor.top() - screen.top() - 4.0;
        let upwards = wanted > below && above > below;
        let room = if upwards { above } else { below };
        let position = if upwards {
            pos2(
                popup.anchor.left(),
                popup.anchor.top() - 2.0 - wanted.min(room),
            )
        } else {
            popup.anchor.left_bottom() + vec2(0.0, 2.0)
        };
        let area = egui::Area::new(Id::new(("popup", &popup.id)))
            .order(Order::Foreground)
            .constrain_to(screen)
            .fixed_pos(position)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(pal.panel)
                    .stroke(Stroke::new(1.0, pal.accent))
                    .corner_radius(CornerRadius::same(3))
                    .inner_margin(Margin::same(3))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        egui::ScrollArea::vertical()
                            .id_salt(("popup", &popup.id))
                            .max_height((room - 8.0).max(ROW_HEIGHT))
                            .show(ui, |ui| {
                                for (index, option) in popup.options.iter().enumerate() {
                                    let width = popup.anchor.width().max(160.0) - 6.0;
                                    let (rect, response) = ui.allocate_exact_size(
                                        vec2(width, ROW_HEIGHT),
                                        Sense::click(),
                                    );
                                    if response.hovered()
                                        && ui.input(|i| i.pointer.delta() != Vec2::ZERO)
                                    {
                                        popup.highlighted = index;
                                    }
                                    let highlighted = index == popup.highlighted;
                                    if highlighted {
                                        ui.painter().rect_filled(
                                            rect,
                                            CornerRadius::same(2),
                                            pal.accent,
                                        );
                                    }
                                    ui.painter().text(
                                        pos2(rect.left() + 8.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        option,
                                        FontId::proportional(14.0),
                                        if highlighted { pal.on_accent } else { pal.text },
                                    );
                                    if response.clicked() {
                                        chosen = Some(index);
                                    }
                                }
                            });
                    });
            });
        if let Some(index) = chosen {
            self.state.chosen = Some((popup.id.clone(), index));
            self.state.popup = None;
            ctx.request_repaint();
        } else if !opened_this_frame && area.response.clicked_elsewhere() {
            log::debug!("select '{}' closes, clicked elsewhere", popup.id);
            self.state.popup = None;
        }
    }
}

/// Paints a row. Its links answer clicks of their own, so `id` must be the row's.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &Ui,
    pal: &Palette,
    id: Id,
    rect: Rect,
    row: &Row<'_>,
    layout: &Layout<'_>,
    look: Look,
) {
    let painter = ui.painter();
    let (text, detail, signal, switch, tree, columns, links) = match row {
        Row::Header(text) => {
            painter.text(
                pos2(rect.left() + 6.0, rect.center().y),
                Align2::LEFT_CENTER,
                text,
                Weight::SemiBold.font_id(13.0),
                pal.info,
            );
            return;
        }
        Row::Item {
            text,
            detail,
            signal,
            switch,
            tree,
            columns,
            links,
        } => (*text, *detail, *signal, *switch, *tree, *columns, *links),
    };
    let lit = look == Look::Lit;
    let (fill, color) = match look {
        Look::Lit => (Some(pal.accent), pal.on_accent),
        Look::Selected => (Some(pal.surface_active), pal.text),
        Look::Hovered => (Some(pal.surface), pal.text),
        Look::Plain => (None, pal.text),
    };
    if let Some(fill) = fill {
        painter.rect_filled(rect, CornerRadius::same(2), fill);
    }
    // A link opens on a click, underlines on hover, and turns red with the
    // error as its tooltip when opening failed. Returns the text colour.
    let link = |position: usize, text_rect: Rect, plain: Color32| {
        let Some(Some(url)) = links.get(position) else {
            return plain;
        };
        let response = ui
            .interact(text_rect, id.with(("link", position)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            open_url(ui.ctx(), url);
        }
        let failed = url_error(ui.ctx(), url);
        let color = match (&failed, lit) {
            (Some(_), _) => pal.danger,
            (None, true) => pal.on_accent,
            (None, false) => pal.info,
        };
        if response.hovered() {
            painter.hline(
                text_rect.x_range(),
                text_rect.bottom(),
                Stroke::new(1.0, color),
            );
        }
        if let Some(err) = failed {
            response.on_hover_text(err);
        }
        color
    };

    let (signal_x, mut text_right) = layout.trailing(rect);
    for (column, value) in columns.iter().enumerate().rev() {
        let dim = if lit { pal.on_accent } else { pal.secondary };
        let mut job = egui::text::LayoutJob::simple_singleline(
            value.clone(),
            FontId::proportional(13.0),
            Color32::PLACEHOLDER,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(layout.width(column) - COLUMN_GAP);
        let galley = painter.layout_job(job);
        let at = pos2(
            text_right - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        );
        let color = link(column + 1, Rect::from_min_size(at, galley.size()), dim);
        painter.galley(at, galley, color);
        text_right -= layout.width(column);
    }

    if let Some(detail) = detail {
        let dim = if lit { pal.on_accent } else { pal.secondary };
        let galley = painter.layout_no_wrap(detail.to_owned(), FontId::proportional(13.0), dim);
        let left = text_right - galley.size().x;
        painter.galley(
            pos2(left, rect.center().y - galley.size().y / 2.0),
            galley,
            dim,
        );
        text_right = left - 12.0;
    }
    let guide_color = if lit { pal.on_accent } else { pal.outline };
    let guide_stroke = Stroke::new(1.0, guide_color);
    for (level, guide) in tree.iter().enumerate() {
        let x = (rect.left() + 18.0 + level as f32 * TREE_INDENT).round() + 0.5;
        let middle = rect.center().y.round() + 0.5;
        let bottom = match guide {
            Guide::Blank => continue,
            Guide::Line | Guide::Tee => rect.bottom(),
            Guide::Corner => middle,
        };
        painter.line_segment([pos2(x, rect.top()), pos2(x, bottom)], guide_stroke);
        if *guide != Guide::Line {
            let end = x + TREE_INDENT - 8.0;
            painter.line_segment([pos2(x, middle), pos2(end, middle)], guide_stroke);
        }
    }
    let text_rect = Rect::from_min_max(
        pos2(
            rect.left() + 12.0 + tree.len() as f32 * TREE_INDENT,
            rect.top(),
        ),
        pos2(text_right, rect.bottom()),
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        FontId::proportional(14.0),
        Color32::PLACEHOLDER,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(0.0));
    let galley = painter.layout_job(job);
    let at = pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0);
    let shown = Rect::from_min_size(at, galley.size()).intersect(text_rect);
    let color = link(0, shown, color);
    painter.with_clip_rect(text_rect).galley(at, galley, color);
    if let Some(signal) = signal {
        let (on, off) = if lit {
            (pal.on_accent, pal.accent_hover)
        } else {
            (pal.text, pal.surface_hover)
        };
        signal_bars(
            painter,
            pos2(signal_x, rect.center().y + 6.0),
            signal,
            on,
            off,
        );
    }

    if let Some(on) = switch {
        let track = Rect::from_center_size(switch_rect(rect).center(), vec2(36.0, 20.0));
        paint_switch(painter, pal, track, on, lit);
    }
}

fn switch_rect(row: Rect) -> Rect {
    Rect::from_min_max(
        pos2(row.right() - 52.0, row.top() + 2.0),
        pos2(row.right() - 4.0, row.bottom() - 2.0),
    )
}

// On an accent-filled row the normal accent track would vanish, so the colours flip.
fn paint_switch(painter: &egui::Painter, pal: &Palette, track: Rect, on: bool, on_accent: bool) {
    let (fill, knob) = match (on, on_accent) {
        (true, false) => (pal.accent, pal.on_accent),
        (false, false) => (pal.surface, pal.dim),
        (true, true) => (pal.on_accent, pal.accent),
        (false, true) => (pal.accent_hover, pal.on_accent),
    };
    painter.rect_filled(track, CornerRadius::same(10), fill);
    let x = if on {
        track.right() - 10.0
    } else {
        track.left() + 10.0
    };
    painter.circle_filled(pos2(x, track.center().y), 7.0, knob);
}

fn signal_bars(painter: &egui::Painter, base: Pos2, signal: u8, lit: Color32, unlit: Color32) {
    let bars = match signal {
        80.. => 4,
        55.. => 3,
        30.. => 2,
        5.. => 1,
        _ => 0,
    };
    for bar in 0..4 {
        let height = 3.0 + bar as f32 * 3.0;
        let left = base.x + bar as f32 * 6.0;
        let rect = Rect::from_min_max(pos2(left, base.y - height), pos2(left + 4.0, base.y));
        painter.rect_filled(
            rect,
            CornerRadius::same(1),
            if bar < bars { lit } else { unlit },
        );
    }
}

/// The window's content area inside its margins.
pub fn content(ctx: &egui::Context) -> Rect {
    let rect = ctx.content_rect();
    Rect::from_min_max(rect.min + MARGIN, rect.max - MARGIN)
}

pub fn screen<R>(
    ctx: &egui::Context,
    pal: &Palette,
    id: &str,
    title: &str,
    rect: Rect,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    // A plain Ui, not an Area: an Area lays out its first frames at a remembered
    // size, which made lists start narrow and jump to full width.
    let mut ui = Ui::new(
        ctx.clone(),
        Id::new(("screen", id)),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(rect),
    );
    ui.label(
        egui::RichText::new(title)
            .font(Weight::SemiBold.font_id(18.0))
            .color(pal.accent),
    );
    ui.add_space(14.0);
    add(&mut ui)
}

/// A small centred box over the dimmed screen; the screen below can't be clicked.
pub fn popup<R>(
    ctx: &egui::Context,
    pal: &Palette,
    id: &str,
    title: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let frame = Frame::new()
        .fill(pal.panel)
        .stroke(Stroke::new(1.0, pal.outline))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(20));
    egui::Modal::new(Id::new(("popup", id)))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(title)
                    .font(Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            ui.add_space(10.0);
            add(ui)
        })
        .inner
}

pub fn label(ui: &mut Ui, pal: &Palette, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        vec2(width, FIELD_HEIGHT),
        egui::Layout::right_to_left(Align::Center),
        |ui| ui.label(egui::RichText::new(text).color(pal.secondary)),
    );
}

pub fn button_row(form: &mut Form<'_>, ui: &mut Ui, buttons: &[(&str, &str)]) -> Option<usize> {
    let mut pressed = None;
    ui.horizontal(|ui| {
        let width: f32 = buttons
            .iter()
            .map(|(_, label)| button_width(ui, label))
            .sum::<f32>()
            + ui.spacing().item_spacing.x * (buttons.len().saturating_sub(1)) as f32;
        ui.add_space((ui.available_width() - width).max(0.0));
        for (index, (id, label)) in buttons.iter().enumerate() {
            if form.button(ui, id, label) {
                pressed = Some(index);
            }
        }
    });
    pressed
}

fn button_width(ui: &Ui, label: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        Weight::Medium.font_id(14.0),
        Color32::WHITE,
    );
    (galley.size().x + 28.0).max(72.0)
}

/// An address or name as a link to the web page it may serve; None for
/// anything else, like a MAC address, and for link-local IPv6, which needs a
/// zone that browsers don't take.
pub fn web_url(text: &str) -> Option<String> {
    let address = text.split('/').next().unwrap_or_default();
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => Some(format!("http://{ip}/")),
        Ok(std::net::IpAddr::V6(ip)) if ip.is_unicast_link_local() => None,
        Ok(std::net::IpAddr::V6(ip)) => Some(format!("http://[{ip}]/")),
        Err(_) => {
            let name = address.contains('.')
                && address
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            name.then(|| format!("http://{address}/"))
        }
    }
}

/// Opens with `xdg-open`, since eframe opens no links on the desktop. A
/// failure shows below the link.
pub fn link(ui: &mut Ui, pal: &Palette, text: egui::RichText, url: &str) {
    let response = ui
        .add(egui::Label::new(text.color(pal.info)).sense(Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().hline(
            response.rect.x_range(),
            response.rect.bottom(),
            Stroke::new(1.0, pal.info),
        );
    }
    if response.clicked() {
        open_url(ui.ctx(), url);
    }
    if let Some(err) = url_error(ui.ctx(), url) {
        ui.label(egui::RichText::new(err).color(pal.danger).small());
    }
}

/// Runs `xdg-open` on its own thread; a failure is kept for `url_error`.
fn open_url(ctx: &egui::Context, url: &str) {
    let failed = Id::new(("open-url", url));
    ctx.data_mut(|d| d.remove::<String>(failed));
    let (ctx, url) = (ctx.clone(), url.to_owned());
    std::thread::spawn(move || {
        log::info!("opening {url}");
        // Not run(): its LC_ALL=C would reach a browser started by this.
        let result = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        log::debug!("xdg-open {url} -> {result:?}");
        let err = match result {
            Ok(status) if status.success() => return,
            Ok(status) => format!("xdg-open failed: {status}"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                "xdg-open is not installed".to_owned()
            }
            Err(err) => format!("Could not run xdg-open: {err}"),
        };
        let err = crate::nm::failed("URL-1", &format!("open {url}"), err);
        ctx.data_mut(|d| d.insert_temp(failed, err));
        ctx.request_repaint();
    });
}

fn url_error(ctx: &egui::Context, url: &str) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(Id::new(("open-url", url))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Palette;
    use egui::{Context, PointerButton};

    fn frame(ctx: &Context, state: &mut FormState, events: Vec<Event>) -> Rect {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
            events,
            ..Default::default()
        };
        let pal = Palette::gruvbox_dark_hard();
        let mut anchor = Rect::NOTHING;
        let mut output = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut form = Form::new(state, &[], &pal);
                let mut selected = 0;
                form.select(ui, "pick", &mut selected, &["one", "two", "three"], 120.0);
                anchor = form
                    .state
                    .popup
                    .as_ref()
                    .map_or(Rect::NOTHING, |p| p.anchor);
                form.finish(&ui.ctx().clone());
            });
        });
        output.textures_delta.clear();
        anchor
    }

    #[test]
    fn a_click_opens_the_popup_and_the_next_frame_keeps_it_open() {
        let ctx = Context::default();
        ctx.set_fonts(fastframe_fonts::FontSetup::default().definitions());
        let mut state = FormState::default();

        frame(&ctx, &mut state, Vec::new());
        let field = pos2(40.0, 20.0);
        frame(
            &ctx,
            &mut state,
            vec![
                Event::PointerMoved(field),
                Event::PointerButton {
                    pos: field,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                Event::PointerButton {
                    pos: field,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
        );
        assert!(state.popup.is_some(), "the click did not open the popup");

        frame(&ctx, &mut state, Vec::new());
        assert!(
            state.popup.is_some(),
            "the popup closed the frame after it opened"
        );
    }

    fn click(at: Pos2) -> Vec<Event> {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![Event::PointerMoved(at), button(true), button(false)]
    }

    #[test]
    fn clicking_a_column_head_sorts_by_it_and_again_flips_the_direction() {
        let ctx = Context::default();
        ctx.set_fonts(fastframe_fonts::FontSetup::default().definitions());
        let mut state = FormState::default();
        let table = Table {
            name: "Name",
            columns: &[("Size", 100.0)],
        };
        let mut frame = |events, mut sort: Option<Sort>| {
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
                events,
                ..Default::default()
            };
            let pal = Palette::gruvbox_dark_hard();
            let cells = ["1 kB".to_owned()];
            let rows = [Row::Item {
                text: "one",
                detail: None,
                signal: None,
                switch: None,
                tree: &[],
                columns: &cells,
                links: &[],
            }];
            let mut output = ctx.run_ui(raw, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut form = Form::new(&mut state, &[], &pal);
                    form.table(ui, "list", &table, &mut sort, &rows, &mut 0);
                    form.finish(&ui.ctx().clone());
                });
            });
            output.textures_delta.clear();
            sort
        };
        let (name, size) = (pos2(40.0, 22.0), pos2(340.0, 22.0));
        let sort = |column, descending| Some(Sort { column, descending });

        let sorted = frame(Vec::new(), None);
        let sorted = frame(click(size), sorted);
        assert_eq!(sorted, sort(1, true));
        let sorted = frame(click(size), sorted);
        assert_eq!(sorted, sort(1, false));
        let sorted = frame(click(name), sorted);
        assert_eq!(sorted, sort(0, false));
    }

    #[test]
    fn sorting_keeps_every_row_under_its_header() {
        let mut lines = ["b", "c", "a", "#", "z", "y"];
        let header = |l: &&str| *l == "#";
        let by_name = |a: &&str, b: &&str| a.cmp(b);
        sort_groups(
            &mut lines,
            header,
            Sort {
                column: 0,
                descending: false,
            },
            by_name,
        );
        assert_eq!(lines, ["a", "b", "c", "#", "y", "z"]);
        sort_groups(
            &mut lines,
            header,
            Sort {
                column: 0,
                descending: true,
            },
            by_name,
        );
        assert_eq!(lines, ["c", "b", "a", "#", "z", "y"]);
    }

    #[test]
    fn addresses_and_names_link_to_their_web_page() {
        assert_eq!(
            web_url("192.168.188.1/24").as_deref(),
            Some("http://192.168.188.1/")
        );
        assert_eq!(
            web_url("2001:db8::1/64").as_deref(),
            Some("http://[2001:db8::1]/")
        );
        assert_eq!(web_url("fritz.box").as_deref(), Some("http://fritz.box/"));
        assert_eq!(web_url("fe80::1/64"), None);
        assert_eq!(web_url("14:AC:60:00:00:FD"), None);
        assert_eq!(web_url(""), None);
    }
}
