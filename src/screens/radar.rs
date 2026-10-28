use std::ops::RangeInclusive;

use egui::{Align2, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::nm::{Beacon, channel};
use crate::theme::Palette;

const WIDTH: f32 = 760.0;
/// A 20 MHz channel spans four channel numbers, two either side of its centre.
/// The bandwidth isn't read, so every network is drawn 20 MHz wide.
const HALF_WIDTH: f32 = 2.0;
/// The signal axis in dBm: barely audible at the bottom, next to the router at the top.
const WEAKEST: f32 = -100.0;
const STRONGEST: f32 = -30.0;

struct Band {
    name: &'static str,
    frequencies: RangeInclusive<u32>,
    /// The usable channels, spaced evenly on the axis so gaps between
    /// sub-bands don't waste space.
    channels: Vec<u32>,
    /// Channel numbers between two neighbouring usable channels.
    step: f32,
    /// MHz of channel 0; channels are 5 MHz apart.
    base: u32,
}

fn bands() -> [Band; 3] {
    let five_ghz = (36..=64)
        .step_by(4)
        .chain((100..=144).step_by(4))
        .chain((149..=165).step_by(4));
    [
        Band {
            name: "2.4 GHz",
            frequencies: 2400..=2500,
            channels: (1..=13).collect(),
            step: 1.0,
            base: 2407,
        },
        Band {
            name: "5 GHz",
            frequencies: 5150..=5900,
            channels: five_ghz.collect(),
            step: 4.0,
            base: 5000,
        },
        Band {
            name: "6 GHz",
            frequencies: 5925..=7125,
            channels: (1..=233).step_by(4).collect(),
            step: 4.0,
            base: 5950,
        },
    ]
}

/// Where a channel number sits on the axis, counted in usable channels:
/// linear between neighbours, extended past the ends with the end steps.
fn slot(channels: &[u32], channel: f32) -> f32 {
    let at = |i: usize| channels[i] as f32;
    let last = channels.len() - 1;
    let i = channels
        .windows(2)
        .position(|pair| channel <= pair[1] as f32)
        .unwrap_or(last - 1);
    i as f32 + (channel - at(i)) / (at(i + 1) - at(i))
}

/// One chart per band that has networks: a hill per access point, centred on its
/// channel, as high as its signal. Hovering a channel lists its networks.
pub fn chart(ui: &mut Ui, pal: &Palette, mut aps: Vec<&Beacon>) {
    // Strongest first, so they get the first colours and the label spots at their peaks.
    aps.sort_by(|a, b| b.signal.total_cmp(&a.signal));
    let mut ssids: Vec<&str> = Vec::new();
    for ap in &aps {
        if !ssids.contains(&ap.ssid.as_str()) {
            ssids.push(&ap.ssid);
        }
    }
    let colors = [
        pal.accent,
        pal.info,
        pal.success,
        pal.warning,
        pal.danger,
        pal.text,
    ];
    let name = |ap: &Beacon| {
        if ap.ssid.is_empty() {
            "(hidden)".to_owned()
        } else {
            ap.ssid.clone()
        }
    };

    let bands: Vec<Band> = bands()
        .into_iter()
        .filter(|band| {
            aps.iter()
                .any(|ap| band.frequencies.contains(&ap.frequency))
        })
        .collect();
    if bands.is_empty() {
        ui.label(RichText::new("No networks in range.").color(pal.dim));
        return;
    }
    let height =
        ((ui.ctx().content_rect().height() - 220.0) / bands.len() as f32).clamp(120.0, 260.0);

    for band in bands {
        let in_band: Vec<&Beacon> = aps
            .iter()
            .copied()
            .filter(|ap| band.frequencies.contains(&ap.frequency))
            .collect();
        ui.add_space(6.0);
        ui.label(RichText::new(band.name).color(pal.info));
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, height), Sense::hover());
        let painter = ui.painter_at(rect);
        let plot = Rect::from_min_max(rect.min + vec2(30.0, 8.0), rect.max - vec2(4.0, 32.0));
        let channels = &band.channels;
        // Room for a hill on the first and last channel.
        let half = HALF_WIDTH / band.step;
        let (first, last) = (-half, (channels.len() - 1) as f32 + half);
        // Hills keep their width in a gap between sub-bands, so only the centre
        // goes through `slot`.
        let x = |channel: f32, offset: f32| {
            let at = slot(channels, channel) + offset / band.step;
            plot.left() + (at - first) / (last - first) * plot.width()
        };
        let y = |dbm: f32| {
            let height = ((dbm - WEAKEST) / (STRONGEST - WEAKEST)).clamp(0.0, 1.0);
            plot.bottom() - height * plot.height()
        };
        let small = FontId::proportional(11.0);

        for signal in [-90.0, -80.0, -70.0, -60.0, -50.0, -40.0, -30.0] {
            painter.hline(plot.x_range(), y(signal), Stroke::new(1.0, pal.surface));
            painter.text(
                pos2(plot.left() - 6.0, y(signal)),
                Align2::RIGHT_CENTER,
                format!("{signal}"),
                small.clone(),
                pal.dim,
            );
        }
        painter.text(
            pos2(plot.left() - 6.0, plot.bottom() + 17.0),
            Align2::RIGHT_TOP,
            "MHz",
            small.clone(),
            pal.dim,
        );
        // The frequencies are wider than the channel numbers, so each row
        // drops the ticks it has no room for on its own.
        let mut channel_right = f32::NEG_INFINITY;
        let mut frequency_right = f32::NEG_INFINITY;
        for channel in channels {
            let centre = x(*channel as f32, 0.0);
            let rows = [
                (channel.to_string(), 3.0, &mut channel_right),
                (
                    (band.base + channel * 5).to_string(),
                    17.0,
                    &mut frequency_right,
                ),
            ];
            for (text, top, right) in rows {
                let galley = painter.layout_no_wrap(text, small.clone(), pal.dim);
                let left = centre - galley.size().x / 2.0;
                if left < *right + 6.0 {
                    continue;
                }
                *right = left + galley.size().x;
                painter.galley(pos2(left, plot.bottom() + top), galley, pal.dim);
            }
        }

        let mut labels: Vec<Rect> = Vec::new();
        for ap in &in_band {
            let index = ssids.iter().position(|s| *s == ap.ssid).unwrap_or(0);
            let color = colors[index % colors.len()];
            let centre = channel(ap.frequency) as f32;
            let rise = ap.signal - WEAKEST;
            let hill: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 8.0 - 1.0;
                    pos2(x(centre, t * HALF_WIDTH), y(WEAKEST + rise * (1.0 - t * t)))
                })
                .collect();
            painter.add(Shape::convex_polygon(
                hill.clone(),
                color.gamma_multiply(0.1),
                Stroke::NONE,
            ));
            let width = if ap.associated { 2.5 } else { 1.2 };
            painter.add(Shape::line(hill, Stroke::new(width, color)));

            // Networks crowding one channel stack their names downwards; the
            // ones that no longer fit are left to the hover list.
            let galley = painter.layout_no_wrap(name(ap), small.clone(), color);
            let mut label =
                Rect::from_center_size(pos2(x(centre, 0.0), y(ap.signal) - 8.0), galley.size());
            let inside =
                (plot.left() - label.left()).max(0.0) + (plot.right() - label.right()).min(0.0);
            label = label.translate(vec2(inside, 0.0));
            while labels.iter().any(|l| l.intersects(label)) {
                label = label.translate(vec2(0.0, galley.size().y));
            }
            if label.bottom() <= plot.bottom() {
                painter.galley(label.min, galley, color);
                labels.push(label);
            }
        }

        let Some(pointer) = response.hover_pos() else {
            continue;
        };
        let under: Vec<&&Beacon> = in_band
            .iter()
            .filter(|ap| {
                let centre = channel(ap.frequency) as f32;
                (x(centre, -HALF_WIDTH)..=x(centre, HALF_WIDTH)).contains(&pointer.x)
            })
            .collect();
        if under.is_empty() {
            continue;
        }
        response.on_hover_ui(|ui| {
            egui::Grid::new("radar_hover")
                .num_columns(4)
                .spacing(vec2(16.0, 4.0))
                .show(ui, |ui| {
                    for heading in ["Network", "Channel", "Signal", "BSSID"] {
                        ui.label(RichText::new(heading).color(pal.dim));
                    }
                    ui.end_row();
                    for ap in under {
                        ui.label(name(ap));
                        ui.label(channel(ap.frequency).to_string());
                        ui.label(format!("{:.0} dBm", ap.signal));
                        ui.label(RichText::new(&ap.bssid).monospace());
                        ui.end_row();
                    }
                });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_map_to_channels() {
        assert_eq!(channel(2412), 1);
        assert_eq!(channel(2472), 13);
        assert_eq!(channel(2484), 14);
        assert_eq!(channel(5180), 36);
        assert_eq!(channel(5300), 60);
        assert_eq!(channel(5955), 1);
    }

    #[test]
    fn tick_frequencies_match_their_channels() {
        for band in bands() {
            for &c in &band.channels {
                let frequency = band.base + c * 5;
                assert!(band.frequencies.contains(&frequency), "{frequency}");
                assert_eq!(channel(frequency), c);
            }
        }
    }

    #[test]
    fn channels_sit_evenly_across_gaps() {
        let five_ghz = &bands()[1].channels;
        assert_eq!(slot(five_ghz, 36.0), 0.0);
        assert_eq!(slot(five_ghz, 64.0), 7.0);
        assert_eq!(slot(five_ghz, 100.0), 8.0);
        assert_eq!(slot(five_ghz, 34.0), -0.5);
        assert_eq!(slot(five_ghz, 169.0), 25.0);
    }
}
