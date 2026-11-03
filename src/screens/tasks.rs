use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use egui::{Align2, Frame, Margin, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::{Action, Cx};
use crate::form::{self, Form, Guide, Icon, Row, Sort, Table};
#[cfg(test)]
use crate::tasks::Gpu;
use crate::tasks::{Backend, Process, Reply, Request, Snapshot};
use crate::theme::Palette;

const REFRESH: Duration = Duration::from_secs(2);
const PANE_WIDTH: f32 = 600.0;
const GAP: f32 = 20.0;
const LABEL: f32 = 170.0;
const PADDING: i8 = 16;
const METERS: f32 = 72.0;
/// From here a bar turns to the warning colour.
const FULL: f32 = 0.9;

pub struct Tasks {
    backend: Backend,
    snap: Option<Result<Snapshot, String>>,
    /// The last End or Kill that failed, until the next one works.
    error: Option<String>,
    selected: Option<u32>,
    refreshed: Option<Instant>,
    tree: bool,
    sort: Option<Sort>,
}

const TABLE: Table = Table {
    name: "Name",
    columns: &[("CPU", 100.0), ("Memory", 100.0)],
};

impl Tasks {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            backend: Backend::start(ctx.clone()),
            snap: None,
            error: None,
            selected: None,
            refreshed: None,
            tree: false,
            sort: Some(Sort {
                column: 1,
                descending: true,
            }),
        }
    }

    pub fn opened(&mut self) {
        self.refreshed = None;
    }

    pub fn show(&mut self, cx: &mut Cx<'_>, form: &mut Form<'_>, rect: Rect) -> Option<Action> {
        for reply in self.backend.poll() {
            match reply {
                Reply::Snapshot(snap) => self.snap = Some(snap),
                Reply::Done(result) => self.error = result.err(),
            }
        }
        if self.refreshed.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.refreshed = Some(Instant::now());
            self.backend.send(Request::Refresh);
        }
        cx.ctx.request_repaint_after(REFRESH);

        let snap = match &self.snap {
            Some(Ok(snap)) => Some(snap),
            _ => None,
        };
        let processes: &[Process] = snap.map_or(&[], |snap| &snap.processes);
        let (icon, tip) = match self.tree {
            true => (Icon::List, "Show as a list"),
            false => (Icon::Tree, "Show as a tree"),
        };
        if form.top_bar(cx.ctx, false, &[("task-tree", icon, tip)], None) == Some(0) {
            self.tree = !self.tree;
            log::info!("process tree {}", if self.tree { "on" } else { "off" });
        }
        let sorted = sorted(processes, self.sort);
        let shown: Vec<(&Process, Vec<Guide>)> = match self.tree {
            true => tree(&sorted),
            false => sorted.into_iter().map(|p| (p, Vec::new())).collect(),
        };
        let values: Vec<[String; 2]> = shown
            .iter()
            .map(|(p, _)| [format!("{:.1} %", p.cpu), bytes(p.memory)])
            .collect();
        let rows: Vec<Row<'_>> = shown
            .iter()
            .zip(&values)
            .map(|((p, guides), columns)| Row::Item {
                text: &p.name,
                detail: None,
                signal: None,
                switch: None,
                tree: guides,
                columns,
                links: &[],
            })
            .collect();
        let mut selected = self
            .selected
            .and_then(|pid| shown.iter().position(|(p, _)| p.pid == pid))
            .unwrap_or(0);

        let status = match &self.snap {
            None => ("Reading the processes…".to_owned(), cx.pal.dim),
            Some(Err(err)) => (err.clone(), cx.pal.danger),
            Some(Ok(snap)) => match &self.error {
                Some(err) => (err.clone(), cx.pal.danger),
                None => (
                    format!("{} processes", snap.processes.len()),
                    cx.pal.secondary,
                ),
            },
        };
        let pal = cx.pal;
        let mut request = None;
        form::screen(cx.ctx, pal, "tasks", "Tasks", rect, |ui| {
            ui.label(RichText::new(status.0).color(status.1));
            ui.add_space(8.0);
            if let Some(snap) = snap {
                meters(ui, pal, snap);
                ui.add_space(12.0);
            }
            let area = ui.available_rect_before_wrap();
            let process = shown.get(selected).map(|&(process, _)| process);
            let split = area.right() - PANE_WIDTH;
            let list = match process {
                Some(_) => Rect::from_min_max(area.min, pos2(split - GAP, area.bottom())),
                None => area,
            };
            ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
                form.table(ui, "list", &TABLE, &mut self.sort, &rows, &mut selected);
            });
            let Some(process) = process else {
                return;
            };
            let pane = Rect::from_min_max(pos2(split, area.top()), area.max);
            let parent = processes.iter().find(|p| p.pid == process.parent);
            ui.scope_builder(UiBuilder::new().max_rect(pane), |ui| {
                let mark = form.mark();
                let frame = Frame::new()
                    .stroke(Stroke::new(1.0, pal.outline))
                    .corner_radius(3)
                    .show(ui, |ui| {
                        let inner = pane.size() - vec2(2.0, 2.0);
                        ui.set_min_size(inner);
                        ui.set_max_width(inner.x);
                        request = pane_details(ui, form, process, parent);
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

        self.selected = shown.get(selected).map(|(p, _)| p.pid);
        if let Some(request) = request {
            self.error = None;
            self.backend.send(request);
            self.refreshed = None;
        }
        None
    }
}

fn sorted(processes: &[Process], sort: Option<Sort>) -> Vec<&Process> {
    let mut sorted: Vec<&Process> = processes.iter().collect();
    let Some(sort) = sort else {
        return sorted;
    };
    sorted.sort_by(|a, b| {
        let order = match sort.column {
            0 => form::by_text(&a.name, &b.name),
            1 => a.cpu.total_cmp(&b.cpu).then(a.memory.cmp(&b.memory)),
            _ => a.memory.cmp(&b.memory).then(a.cpu.total_cmp(&b.cpu)),
        };
        match sort.descending {
            true => order.reverse(),
            false => order,
        }
    });
    sorted
}

/// Depth-first from the roots, keeping the given order among siblings, with
/// the guide lines in front of each row. A process whose parent isn't listed
/// is a root.
fn tree<'a>(processes: &[&'a Process]) -> Vec<(&'a Process, Vec<Guide>)> {
    let listed: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    let mut children: HashMap<u32, Vec<&Process>> = HashMap::new();
    let mut roots = Vec::new();
    for &p in processes {
        match p.parent != p.pid && listed.contains(&p.parent) {
            true => children.entry(p.parent).or_default().push(p),
            false => roots.push(p),
        }
    }
    let mut order = Vec::with_capacity(processes.len());
    let mut stack: Vec<(&Process, Vec<Guide>)> =
        roots.into_iter().rev().map(|p| (p, Vec::new())).collect();
    while let Some((p, guides)) = stack.pop() {
        if let Some(kids) = children.get(&p.pid) {
            let mut above = guides.clone();
            if let Some(last) = above.last_mut() {
                *last = match last {
                    Guide::Corner => Guide::Blank,
                    _ => Guide::Line,
                };
            }
            for (index, &kid) in kids.iter().enumerate().rev() {
                let mut guides = above.clone();
                guides.push(match index + 1 == kids.len() {
                    true => Guide::Corner,
                    false => Guide::Tee,
                });
                stack.push((kid, guides));
            }
        }
        order.push((p, guides));
    }
    order
}

fn pane_details(
    ui: &mut Ui,
    form: &mut Form<'_>,
    process: &Process,
    parent: Option<&Process>,
) -> Option<Request> {
    let pal = form.pal().clone();
    let (mut end, mut kill) = (false, false);
    let header = Margin {
        bottom: 10,
        ..Margin::same(PADDING)
    };
    Frame::new().inner_margin(header).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&process.name)
                    .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
                    .color(pal.text),
            );
            let width = 2.0 * form::FIELD_HEIGHT + ui.spacing().item_spacing.x;
            ui.add_space((ui.available_width() - width).max(0.0));
            end = form.icon_button(ui, "task-end", Icon::Stop, "End (ask it to quit)");
            kill = form.icon_button(ui, "task-kill", Icon::Kill, "Kill (stop it at once)");
        });
    });
    let y = ui.cursor().top();
    ui.painter()
        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));

    egui::ScrollArea::vertical()
        .id_salt("task")
        .max_height(ui.available_height().max(60.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new().inner_margin(PADDING).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let parent = match parent {
                    Some(parent) => format!("{} ({})", parent.name, parent.pid),
                    None => process.parent.to_string(),
                };
                let rows = [
                    ("Process ID", process.pid.to_string()),
                    ("Started by", parent),
                    ("User", process.user.clone()),
                    ("State", process.state.clone()),
                    ("CPU", format!("{:.1} %", process.cpu)),
                    ("CPU time", duration(process.cpu_time)),
                    ("Memory", bytes(process.memory)),
                    ("Threads", process.threads.to_string()),
                    ("Running for", duration(process.age)),
                    ("Command", process.command.clone()),
                ];
                for (label, value) in rows.iter().filter(|(_, v)| !v.is_empty()) {
                    row(ui, &pal, label, value);
                }
            });
        });

    let (pid, name) = (process.pid, process.name.clone());
    if end {
        return Some(Request::End { pid, name });
    }
    kill.then_some(Request::Kill { pid, name })
}

/// A bar per core above the list, and bars for the memory and each graphics
/// card's above the pane.
fn meters(ui: &mut Ui, pal: &Palette, snap: &Snapshot) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), METERS), Sense::hover());
    let painter = ui.painter();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let level = |share: f32| {
        if share >= FULL {
            pal.warning
        } else {
            pal.accent
        }
    };
    let split = rect.right() - PANE_WIDTH;

    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("CPU {:.0} %", snap.cpu),
        font.clone(),
        pal.secondary,
    );
    let bars = Rect::from_min_max(rect.min + vec2(0.0, 20.0), pos2(split - GAP, rect.bottom()));
    let step = bars.width() / snap.cores.len().max(1) as f32;
    let gap = (step * 0.25).min(4.0);
    for (index, busy) in snap.cores.iter().enumerate() {
        let left = bars.left() + index as f32 * step;
        let slot = Rect::from_min_max(
            pos2(left, bars.top()),
            pos2(left + step - gap, bars.bottom()),
        );
        let share = (busy / 100.0).clamp(0.0, 1.0);
        let fill = Rect::from_min_max(
            pos2(slot.left(), slot.bottom() - slot.height() * share),
            slot.max,
        );
        painter.rect_filled(slot, 2, pal.surface);
        painter.rect_filled(fill, 2, level(share));
    }

    let rows = memory_rows(snap);
    let height = (rect.height() / rows.len() as f32).min(24.0);
    for (index, (label, used, total)) in rows.iter().enumerate() {
        let top = rect.top() + index as f32 * height;
        let middle = top + height / 2.0 - 2.0;
        painter.text(
            pos2(split, middle),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            pal.secondary,
        );
        let text = format!("{} of {}", bytes(*used), bytes(*total));
        painter.text(
            pos2(rect.right(), middle),
            Align2::RIGHT_CENTER,
            text,
            font.clone(),
            pal.text,
        );
        let bar = Rect::from_min_max(
            pos2(split + 100.0, middle - 5.0),
            pos2(rect.right() - 160.0, middle + 5.0),
        );
        let share = (*used as f32 / (*total).max(1) as f32).clamp(0.0, 1.0);
        let fill = Rect::from_min_max(
            bar.min,
            pos2(bar.left() + bar.width() * share, bar.bottom()),
        );
        painter.rect_filled(bar, 2, pal.surface);
        painter.rect_filled(fill, 2, level(share));
    }

    let hovered = response
        .hover_pos()
        .filter(|pointer| bars.contains(*pointer));
    let core = hovered.map_or(usize::MAX, |pointer| {
        ((pointer.x - bars.left()) / step) as usize
    });
    if let Some(busy) = snap.cores.get(core) {
        response.on_hover_text(format!("Core {core}: {busy:.0} %"));
    }
}

/// Label, used and total bytes: the memory, then each graphics card's own
/// memory and the system memory it shares, when it shares any.
fn memory_rows(snap: &Snapshot) -> Vec<(String, u64, u64)> {
    let mut rows = vec![("Memory".to_owned(), snap.memory_used, snap.memory_total)];
    let several = snap.gpus.len() > 1;
    for gpu in &snap.gpus {
        let card = if several {
            format!(" {}", gpu.card)
        } else {
            String::new()
        };
        rows.push((format!("VRAM{card}"), gpu.vram_used, gpu.vram_total));
        if gpu.shared_total > 0 {
            rows.push((
                format!("GPU shared{card}"),
                gpu.shared_used,
                gpu.shared_total,
            ));
        }
    }
    rows
}

/// "340 MB", "1.2 GB".
fn bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// "42 s", "3 min 5 s", "2 h 10 min", "4 days 3 h".
fn duration(seconds: f64) -> String {
    let s = seconds as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        3600..86400 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        _ => format!("{} days {} h", s / 86400, s % 86400 / 3600),
    }
}

fn row(ui: &mut Ui, pal: &Palette, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL, 0.0),
            egui::Layout::right_to_left(egui::Align::Min),
            |ui| ui.label(RichText::new(label).color(pal.secondary)),
        );
        ui.add(egui::Label::new(RichText::new(value).color(pal.text)).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations_read_plainly() {
        assert_eq!(bytes(340_000_000), "340 MB");
        assert_eq!(bytes(1_234_000_000), "1.2 GB");
        assert_eq!(duration(42.9), "42 s");
        assert_eq!(duration(185.0), "3 min 5 s");
        assert_eq!(duration(7800.0), "2 h 10 min");
        assert_eq!(duration(356_400.0), "4 days 3 h");
    }

    #[test]
    fn memory_rows_show_vram_and_shared_gpu_memory_only_where_there_is_some() {
        let gpu = |card: &str, shared_total| Gpu {
            card: card.to_owned(),
            vram_used: 1,
            vram_total: 2,
            shared_used: 3,
            shared_total,
        };
        let labels = |gpus: Vec<Gpu>| -> Vec<String> {
            let snap = Snapshot {
                gpus,
                ..Snapshot::default()
            };
            memory_rows(&snap).into_iter().map(|row| row.0).collect()
        };
        assert_eq!(labels(vec![]), ["Memory"]);
        assert_eq!(
            labels(vec![gpu("card1", 4)]),
            ["Memory", "VRAM", "GPU shared"]
        );
        assert_eq!(
            labels(vec![gpu("card0", 0), gpu("card1", 4)]),
            ["Memory", "VRAM card0", "VRAM card1", "GPU shared card1"]
        );
    }

    #[test]
    fn tree_puts_children_under_their_parent_in_the_given_order_with_guides() {
        let process = |pid, parent| Process {
            pid,
            parent,
            ..Process::default()
        };
        // 1 is the parent of 4 and 2, 2 of 3 and 5, and 9's parent 8 isn't listed.
        let processes = [
            process(3, 2),
            process(9, 8),
            process(4, 1),
            process(1, 0),
            process(2, 1),
            process(5, 2),
        ];
        let sorted: Vec<&Process> = processes.iter().collect();
        let rows: Vec<(u32, Vec<Guide>)> = tree(&sorted)
            .into_iter()
            .map(|(p, guides)| (p.pid, guides))
            .collect();
        use Guide::*;
        assert_eq!(
            rows,
            [
                (9, vec![]),
                (1, vec![]),
                (4, vec![Tee]),
                (2, vec![Corner]),
                (3, vec![Blank, Tee]),
                (5, vec![Blank, Corner]),
            ]
        );
    }

    #[test]
    fn processes_sort_by_each_column_both_ways() {
        let process = |name: &str, cpu, memory| Process {
            name: name.to_owned(),
            cpu,
            memory,
            ..Process::default()
        };
        let processes = [
            process("b", 1.0, 30),
            process("C", 3.0, 10),
            process("a", 2.0, 20),
        ];
        let names = |column, descending| -> String {
            sorted(&processes, Some(Sort { column, descending }))
                .iter()
                .map(|p| p.name.as_str())
                .collect()
        };
        assert_eq!(names(0, false), "abC");
        assert_eq!(names(0, true), "Cba");
        assert_eq!(names(1, true), "Cab");
        assert_eq!(names(1, false), "baC");
        assert_eq!(names(2, true), "baC");
        assert_eq!(names(2, false), "Cab");
    }
}
