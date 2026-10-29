//! The screen backlight and the ambient light sensor. Both are sysfs files
//! that have nothing to do with Hyprland, so the Screens page reads them here.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::nm::{failed, run};

const BACKLIGHTS: &str = "/sys/class/backlight";
const SENSORS: &str = "/sys/bus/iio/devices";
/// How often automatic brightness looks at the light in the room.
const STEP: Duration = Duration::from_secs(2);
/// A dark panel is still readable; an off one looks broken.
const DIMMEST: f32 = 0.05;
/// A smaller step than this isn't worth the flicker.
const WORTH_IT: f32 = 0.03;

#[derive(Clone, Debug, PartialEq)]
pub struct Backlight {
    /// The sysfs name, such as "amdgpu_bl1".
    pub device: String,
    /// The connector it lights, such as "eDP-1"; empty when sysfs doesn't say.
    pub connector: String,
    /// 0 to 1.
    pub level: f32,
}

/// Every panel whose brightness can be changed. An external monitor is not
/// one: that needs DDC/CI over i2c, which this doesn't do.
pub fn backlights() -> Vec<Backlight> {
    let entries = match std::fs::read_dir(BACKLIGHTS) {
        Ok(entries) => entries,
        Err(err) => {
            log::debug!("no backlight in {BACKLIGHTS}: {err}");
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let max = number(&path.join("max_brightness")).unwrap_or(0.0);
        // The level that was asked for; what the panel shows reads as 0 while
        // the driver is in the middle of a modeset.
        let now = number(&path.join("brightness")).unwrap_or(0.0);
        if max <= 0.0 {
            log::debug!("{} has no brightness to set", path.display());
            continue;
        }
        let device = entry.file_name().to_string_lossy().into_owned();
        let connector =
            connector_of(&std::fs::canonicalize(path.join("device")).unwrap_or_default());
        log::debug!(
            "backlight {device} lights '{connector}' at {:.0} %",
            now / max * 100.0
        );
        found.push(Backlight {
            device,
            connector,
            level: now / max,
        });
    }
    found
}

/// The backlight of one screen. sysfs points most panels at their connector;
/// when it points at the graphics card instead, the one backlight there is
/// belongs to the built-in screen.
pub fn of<'a>(found: &'a [Backlight], connector: &str) -> Option<&'a Backlight> {
    if let Some(exact) = found.iter().find(|light| light.connector == connector) {
        return Some(exact);
    }
    let built_in = ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|kind| connector.starts_with(kind));
    match found {
        [only] if built_in && only.connector.is_empty() => Some(only),
        _ => None,
    }
}

pub fn set(device: &str, level: f32) -> Result<(), String> {
    let percent = (level.clamp(DIMMEST, 1.0) * 100.0).round() as u32;
    log::info!("setting the brightness of {device} to {percent} %");
    run(
        "brightnessctl",
        [
            "--device",
            device,
            "--class",
            "backlight",
            "set",
            &format!("{percent}%"),
        ],
    )
    .map(|_| ())
    .map_err(|err| failed("LT-1", "set the screen brightness", err))
}

/// What the ambient light sensor reads, in lux; None when there is no sensor.
pub fn lux() -> Option<f32> {
    for entry in std::fs::read_dir(SENSORS).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        let Some(raw) = number(&path.join("in_illuminance_raw")) else {
            continue;
        };
        let scale = number(&path.join("in_illuminance_scale")).unwrap_or(1.0);
        return Some(raw * scale);
    }
    None
}

/// How bright the panel should be for that much light in the room. The eye
/// reads light logarithmically, so the level follows the logarithm of the lux,
/// not the lux: dim in the dark, full from about a thousand lux up.
pub fn level_for(lux: f32) -> f32 {
    (0.1 + 0.3 * (1.0 + lux.max(0.0)).log10()).clamp(DIMMEST, 1.0)
}

/// "…/drm/card1/card1-eDP-1" is the connector the panel sits on.
fn connector_of(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    match name.split_once('-') {
        Some((card, connector)) if card.starts_with("card") => connector.to_owned(),
        _ => String::new(),
    }
}

fn number(path: &Path) -> Option<f32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Sets the backlight to match the light in the room, over and over, until it
/// is dropped. One thread, like the finder, because nothing else has to wait
/// for it.
pub struct Auto {
    pub device: String,
    stop: Arc<AtomicBool>,
    errors: Receiver<String>,
}

impl Auto {
    pub fn start(ctx: egui::Context, device: String, level: f32) -> Self {
        let (sender, errors) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let name = device.clone();
        log::info!("following the light in the room for {device}");
        std::thread::Builder::new()
            .name("brightness".into())
            .spawn(move || {
                let mut level = level;
                while !stopped.load(Ordering::Relaxed) {
                    let Some(lux) = lux() else {
                        let err = failed(
                            "LT-2",
                            "read the ambient light sensor",
                            "it stopped answering".to_owned(),
                        );
                        let _ = sender.send(err);
                        ctx.request_repaint();
                        return;
                    };
                    let wanted = level_for(lux);
                    if (wanted - level).abs() >= WORTH_IT {
                        log::debug!("{lux:.0} lux asks for {:.0} %", wanted * 100.0);
                        if let Err(err) = set(&name, wanted) {
                            let _ = sender.send(err);
                            ctx.request_repaint();
                            return;
                        }
                        level = wanted;
                        ctx.request_repaint();
                    }
                    // Short naps, so the toggle stops it at once.
                    for _ in 0..20 {
                        if stopped.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(STEP / 20);
                    }
                }
            })
            .expect("a worker thread");
        Self {
            device,
            stop,
            errors,
        }
    }

    /// The error that stopped the thread, once.
    pub fn error(&self) -> Option<String> {
        self.errors.try_iter().last()
    }
}

impl Drop for Auto {
    fn drop(&mut self) {
        log::info!(
            "no longer following the light in the room for {}",
            self.device
        );
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(device: &str, connector: &str) -> Backlight {
        Backlight {
            device: device.to_owned(),
            connector: connector.to_owned(),
            level: 0.5,
        }
    }

    #[test]
    fn the_connector_comes_out_of_the_drm_path() {
        assert_eq!(
            connector_of(Path::new("/sys/devices/pci0000:00/drm/card1/card1-eDP-1")),
            "eDP-1"
        );
        assert_eq!(
            connector_of(Path::new("/sys/devices/pci0000:00/0000:c1:00.0")),
            ""
        );
    }

    #[test]
    fn a_screen_gets_the_backlight_of_its_own_connector() {
        let found = [light("amdgpu_bl1", "eDP-1"), light("other", "DP-2")];
        assert_eq!(of(&found, "eDP-1").unwrap().device, "amdgpu_bl1");
        assert_eq!(of(&found, "DP-2").unwrap().device, "other");
        assert!(of(&found, "DP-5").is_none());
    }

    #[test]
    fn the_only_backlight_belongs_to_the_built_in_screen() {
        let found = [light("intel_backlight", "")];
        assert_eq!(of(&found, "eDP-1").unwrap().device, "intel_backlight");
        assert!(of(&found, "DP-3").is_none());
    }

    #[test]
    fn brightness_follows_the_light_in_the_room() {
        assert_eq!(level_for(0.0), 0.1);
        assert!(level_for(15.0) > level_for(0.0));
        assert!(level_for(200.0) > level_for(15.0));
        assert_eq!(level_for(10_000.0), 1.0);
        assert_eq!(level_for(-5.0), 0.1);
    }
}
