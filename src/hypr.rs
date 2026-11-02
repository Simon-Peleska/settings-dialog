use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::Value;

use crate::light::{self, Backlight};
use crate::nm::{failed, run};
use crate::tablet;

/// Written beside hyprland.lua, which loads it with `dofile`.
const LAYOUT_FILE: &str = "monitors.lua";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Hz
    pub refresh: f32,
}

impl Mode {
    pub fn size(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }

    /// What Hyprland's `mode` takes.
    pub fn text(&self) -> String {
        format!("{}x{}@{:.2}", self.width, self.height, self.refresh)
    }

    /// The same mode, give or take the rounding of the refresh rate.
    fn close_to(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && (self.refresh - other.refresh).abs() < 0.1
    }

    /// "2560x1440@59.95Hz", with or without the unit.
    fn parse(text: &str) -> Option<Self> {
        let (size, refresh) = text.trim().trim_end_matches("Hz").split_once('@')?;
        let (width, height) = size.split_once('x')?;
        Some(Self {
            width: width.trim().parse().ok()?,
            height: height.trim().parse().ok()?,
            refresh: refresh.trim().parse().ok()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// The connector, such as "DP-4". It can change between boots, so the
    /// layout file goes by the description instead.
    pub name: String,
    pub description: String,
    /// False for a monitor that is only in the layout file.
    pub connected: bool,
    pub enabled: bool,
    /// None is the monitor's preferred mode.
    pub mode: Option<Mode>,
    pub x: i32,
    pub y: i32,
    pub scale: f32,
    /// 0 to 3 turn the picture by 0°, 90°, 180° and 270°; 4 to 7 are the same,
    /// flipped first.
    pub transform: u8,
    pub vrr: bool,
    /// The output this one shows a copy of; empty when it shows its own.
    pub mirror: String,
    /// What the monitor can do; empty for one that isn't connected.
    pub modes: Vec<Mode>,
}

impl Monitor {
    /// How the layout file names it.
    pub fn output(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            format!("desc:{}", self.description)
        }
    }

    pub fn label(&self) -> &str {
        if self.description.is_empty() {
            &self.name
        } else {
            &self.description
        }
    }

    /// Its width and height on the desktop, after the scale and the turn.
    pub fn logical(&self) -> (f32, f32) {
        let Some(mode) = self.mode else {
            return (0.0, 0.0);
        };
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (width, height) = (mode.width as f32 / scale, mode.height as f32 / scale);
        if self.transform % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub monitors: Vec<Monitor>,
    /// Where the layout is written; empty when there is no Hyprland config.
    pub path: PathBuf,
    /// hyprland.lua loads the layout file, so saving sticks across a restart.
    pub imported: bool,
    /// The panels whose brightness can be set, one per backlight in sysfs.
    pub lights: Vec<Backlight>,
    /// Whether there is an ambient light sensor to follow.
    pub sensor: bool,
}

#[derive(Clone, Debug)]
pub enum Request {
    Refresh,
    /// Applies the layout now, without writing it anywhere.
    Apply(Vec<Monitor>),
    /// Focuses this window and brings the pointer with it.
    Attention,
    /// Applies the layout now and writes it to the layout file.
    Save(Vec<Monitor>),
    /// Sets one panel's backlight, 0 to 1.
    Brightness {
        device: String,
        level: f32,
    },
}

pub enum Reply {
    Snapshot(Result<Snapshot, String>),
    Done(Result<(), String>),
    /// A brightness that could not be set; it leaves the layout alone. A
    /// brightness that worked says nothing, the next snapshot shows it.
    Light(String),
}

pub struct Backend {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
}

impl Backend {
    pub fn start(ctx: egui::Context) -> Self {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("screens".into())
            .spawn(move || {
                while let Ok(request) = inbox.recv() {
                    let changed = matches!(request, Request::Apply(_) | Request::Save(_));
                    if let Some(reply) = handle(request) {
                        let _ = outbox.send(reply);
                    }
                    if changed {
                        let snap = snapshot()
                            .map_err(|err| failed("HY-1", "read the monitors after a change", err));
                        let _ = outbox.send(Reply::Snapshot(snap));
                    }
                    ctx.request_repaint();
                }
            })
            .expect("a worker thread");
        Self { requests, replies }
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    pub fn poll(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

fn handle(request: Request) -> Option<Reply> {
    match request {
        Request::Refresh => Some(Reply::Snapshot(
            snapshot().map_err(|err| failed("HY-2", "read the monitors", err)),
        )),
        Request::Apply(monitors) => {
            log::info!("applying the layout of {} monitors", monitors.len());
            Some(Reply::Done(apply(&monitors)))
        }
        Request::Save(monitors) => {
            log::info!("saving the layout of {} monitors", monitors.len());
            Some(Reply::Done(
                apply(&monitors).and_then(|()| write(&monitors)),
            ))
        }
        Request::Brightness { device, level } => light::set(&device, level).err().map(Reply::Light),
        // A failure here only means the window stays where it is, which is no
        // reason to take the layout error line away from the page.
        Request::Attention => {
            if let Err(err) = attention() {
                log::warn!("{err}");
            }
            None
        }
    }
}

/// Focuses this window; Hyprland warps the pointer onto a window it focuses,
/// so this also brings the mouse to the screen the window is on. The warp has
/// to be turned on for the dispatch, because a config can turn it off.
fn attention() -> Result<(), String> {
    let pid = std::process::id();
    let chunk = format!(
        "for _, w in ipairs(hl.get_windows()) do \
           if w.pid == {pid} then \
             local warps = hl.get_config('cursor.no_warps') \
             hl.config({{ cursor = {{ no_warps = false }} }}) \
             hl.dispatch(hl.dsp.focus({{ window = w }})) \
             hl.config({{ cursor = {{ no_warps = warps }} }}) \
           end \
         end"
    );
    log::debug!("focusing the window of pid {pid}");
    let answer = run("hyprctl", ["eval", &chunk])
        .map_err(|err| failed("HY-8", "focus the settings window", err))?;
    if answer.trim() == "ok" {
        return Ok(());
    }
    Err(failed(
        "HY-9",
        "focus the settings window",
        answer.trim().trim_start_matches("error: ").to_owned(),
    ))
}

/// The connected monitors, then the ones only the layout file knows.
fn snapshot() -> Result<Snapshot, String> {
    let json = run("hyprctl", ["-j", "monitors", "all"])?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| format!("hyprctl sent no JSON: {err}"))?;
    let mut monitors = live_monitors(value.as_array().ok_or("hyprctl sent no list of monitors")?);
    log::debug!("{} monitors connected", monitors.len());

    let path = config_dir().map(|dir| dir.join(LAYOUT_FILE));
    let saved = match &path {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|err| {
            log::debug!("no layout in {}: {err}", path.display());
            String::new()
        }),
        None => String::new(),
    };
    for monitor in parse_layout(&saved) {
        if !monitors.iter().any(|live| live.label() == monitor.label()) {
            log::debug!("'{}' is saved but not connected", monitor.label());
            monitors.push(monitor);
        }
    }

    let imported = config_dir().is_some_and(|dir| {
        std::fs::read_to_string(dir.join("hyprland.lua"))
            .is_ok_and(|config| config.contains(LAYOUT_FILE))
    });
    let lights = light::backlights();
    let sensor = light::lux().is_some();
    Ok(Snapshot {
        monitors,
        path: path.unwrap_or_default(),
        imported,
        lights,
        sensor,
    })
}

/// Hyprland names the mirrored monitor by its ID, the layout file by name.
fn live_monitors(list: &[Value]) -> Vec<Monitor> {
    let mut monitors: Vec<Monitor> = list.iter().map(live_monitor).collect();
    for monitor in &mut monitors {
        if let Some(source) = list.iter().find(|value| {
            value["id"]
                .as_u64()
                .is_some_and(|id| id.to_string() == monitor.mirror)
        }) {
            monitor.mirror = source["name"].as_str().unwrap_or_default().to_owned();
        }
    }
    monitors
}

fn live_monitor(value: &Value) -> Monitor {
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| *text != "none")
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: &str| value[key].as_f64().unwrap_or_default() as f32;
    let mut modes: Vec<Mode> = value["availableModes"]
        .as_array()
        .map(|modes| {
            modes
                .iter()
                .filter_map(|mode| Mode::parse(mode.as_str()?))
                .collect()
        })
        .unwrap_or_default();
    let mut mode = Mode {
        width: number("width") as u32,
        height: number("height") as u32,
        refresh: number("refreshRate"),
    };
    if text("name") == tablet::OUTPUT {
        modes = tablet::SIZES
            .iter()
            .map(|(width, height)| Mode {
                width: *width,
                height: *height,
                refresh: 60.0,
            })
            .collect();
        if !modes.iter().any(|listed| listed.close_to(&mode)) {
            modes.push(mode);
        }
    }
    // The live refresh rate is more precise than the one in the mode list, so
    // it is rounded onto the list the Refresh rate field offers. Without this
    // every monitor would look edited the moment the page draws it.
    if let Some(listed) = modes.iter().find(|listed| listed.close_to(&mode)) {
        mode = *listed;
    }
    Monitor {
        name: text("name"),
        description: text("description"),
        connected: true,
        enabled: !value["disabled"].as_bool().unwrap_or(false),
        // A disabled monitor reports no size, so its saved mode is kept.
        mode: (mode.width > 0).then_some(mode),
        x: value["x"].as_i64().unwrap_or_default() as i32,
        y: value["y"].as_i64().unwrap_or_default() as i32,
        scale: number("scale").max(0.1),
        transform: value["transform"].as_u64().unwrap_or_default() as u8,
        vrr: value["vrr"].as_bool().unwrap_or(false),
        mirror: text("mirrorOf"),
        modes,
    }
}

/// The `hl.monitor({ … })` call the layout file has for this output.
pub fn saved(output: &str) -> Option<(String, Monitor)> {
    let text = std::fs::read_to_string(config_dir()?.join(LAYOUT_FILE)).unwrap_or_default();
    parse_layout(&text)
        .into_iter()
        .find(|monitor| monitor.output() == output)
        .map(|monitor| (lua_block(&monitor), monitor))
}

/// `~/.config/hypr`, followed to where the config really is, so the layout
/// lands beside a config that is symlinked out of the Nix store.
fn config_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let lua = config.join("hypr/hyprland.lua");
    let real = std::fs::canonicalize(&lua)
        .map_err(|err| log::debug!("no hyprland.lua at {}: {err}", lua.display()))
        .ok()?;
    real.parent().map(PathBuf::from)
}

/// Every monitor in one chunk, so a move and a turn land together.
fn apply(monitors: &[Monitor]) -> Result<(), String> {
    let chunk: String = monitors.iter().map(lua_block).collect();
    let answer =
        run("hyprctl", ["eval", &chunk]).map_err(|err| failed("HY-3", "apply the layout", err))?;
    // Hyprland reports a bad layout on stdout and still exits 0.
    if answer.trim() == "ok" {
        return Ok(());
    }
    Err(failed(
        "HY-4",
        "apply the layout",
        answer.trim().trim_start_matches("error: ").to_owned(),
    ))
}

fn write(monitors: &[Monitor]) -> Result<(), String> {
    let path = config_dir()
        .map(|dir| dir.join(LAYOUT_FILE))
        .ok_or_else(|| {
            failed(
                "HY-5",
                "find the Hyprland config",
                "no hyprland.lua".to_owned(),
            )
        })?;
    let mut text = String::from(
        "-- Written by settings-dialog's Screens page. Edits here are overwritten.\n\
         -- hyprland.lua loads this file with dofile().\n\n",
    );
    text.extend(monitors.iter().map(lua_block));
    log::info!("writing the layout to {}", path.display());
    std::fs::write(&path, text)
        .map_err(|err| failed("HY-6", "write the layout file", err.to_string()))
}

fn lua_block(monitor: &Monitor) -> String {
    let mut block = format!("hl.monitor({{\n\toutput = \"{}\",\n", monitor.output());
    if !monitor.enabled {
        return block + "\tdisabled = true,\n})\n\n";
    }
    let mode = monitor
        .mode
        .map_or_else(|| "preferred".to_owned(), |mode| mode.text());
    block += &format!("\tmode = \"{mode}\",\n");
    // Hyprland reports a copy at 0x0 and warns that it overlaps when it gets there.
    let position = match monitor.mirror.is_empty() {
        true => format!("{}x{}", monitor.x, monitor.y),
        false => "auto".to_owned(),
    };
    block += &format!("\tposition = \"{position}\",\n");
    block += &format!("\tscale = {:.6},\n", monitor.scale);
    block += &format!("\ttransform = {},\n", monitor.transform);
    block += &format!("\tvrr = {},\n", u8::from(monitor.vrr));
    // Left out, Hyprland would keep a monitor's earlier mirror.
    block += &format!("\tmirror = \"{}\",\n", monitor.mirror);
    block + "})\n\n"
}

/// The `hl.monitor({ … })` blocks this wrote earlier, so a monitor that isn't
/// plugged in right now keeps its settings.
fn parse_layout(text: &str) -> Vec<Monitor> {
    let mut monitors = Vec::new();
    let mut fields: Vec<(&str, &str)> = Vec::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("hl.monitor(") {
            inside = true;
            fields.clear();
        } else if inside && line.starts_with("})") {
            inside = false;
            if let Some(monitor) = saved_monitor(&fields) {
                monitors.push(monitor);
            }
        } else if inside && let Some((key, value)) = line.split_once('=') {
            fields.push((key.trim(), value.trim().trim_end_matches(',').trim()));
        }
    }
    monitors
}

fn saved_monitor(fields: &[(&str, &str)]) -> Option<Monitor> {
    let field = |wanted: &str| {
        fields
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.trim_matches('"'))
    };
    let output = field("output")?;
    let (x, y) = field("position")
        .and_then(|position| position.split_once('x'))
        .unwrap_or(("0", "0"));
    Some(Monitor {
        name: output
            .strip_prefix("desc:")
            .map_or(output, |_| "")
            .to_owned(),
        description: output.strip_prefix("desc:").unwrap_or("").to_owned(),
        connected: false,
        enabled: field("disabled") != Some("true"),
        mode: field("mode").and_then(Mode::parse),
        x: x.parse().unwrap_or(0),
        y: y.parse().unwrap_or(0),
        scale: field("scale").and_then(|s| s.parse().ok()).unwrap_or(1.0),
        transform: field("transform").and_then(|t| t.parse().ok()).unwrap_or(0),
        vrr: matches!(field("vrr"), Some("1" | "true")),
        mirror: field("mirror").unwrap_or("").to_owned(),
        modes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> Monitor {
        Monitor {
            name: "DP-4".to_owned(),
            description: "Lenovo LEN P27h-10 0x01010101".to_owned(),
            connected: true,
            enabled: true,
            mode: Some(Mode {
                width: 2560,
                height: 1440,
                refresh: 59.951,
            }),
            x: -2560,
            y: -480,
            scale: 1.0,
            transform: 0,
            vrr: false,
            mirror: String::new(),
            modes: Vec::new(),
        }
    }

    #[test]
    fn a_turned_monitor_is_as_tall_as_it_is_wide() {
        let mut upright = monitor();
        upright.scale = 2.0;
        assert_eq!(upright.logical(), (1280.0, 720.0));
        upright.transform = 1;
        assert_eq!(upright.logical(), (720.0, 1280.0));
        upright.transform = 2;
        assert_eq!(upright.logical(), (1280.0, 720.0));
    }

    #[test]
    fn the_live_refresh_rate_rounds_onto_the_mode_list() {
        let json = serde_json::json!({
            "name": "DP-4",
            "description": "Lenovo Group Limited LEN P27h-10 0x01010101",
            "width": 2560,
            "height": 1440,
            "refreshRate": 59.951,
            "x": -2560,
            "y": -480,
            "scale": 1.0,
            "transform": 0,
            "vrr": false,
            "disabled": false,
            "mirrorOf": "none",
            "availableModes": ["2560x1440@59.95Hz", "1920x1080@60.00Hz"],
        });
        let monitor = live_monitor(&json);
        assert_eq!(monitor.mode, Some(monitor.modes[0]));
        assert_eq!(monitor.mode.unwrap().text(), "2560x1440@59.95");
        assert_eq!(
            monitor.output(),
            "desc:Lenovo Group Limited LEN P27h-10 0x01010101"
        );
        assert!(monitor.connected && monitor.enabled);
        assert_eq!(monitor.mirror, "");
        assert_eq!(monitor.logical(), (2560.0, 1440.0));
    }

    #[test]
    fn a_mirror_is_named_by_the_monitor_it_copies() {
        let list = serde_json::json!([
            {"id": 0, "name": "eDP-1", "width": 2880, "height": 1920, "mirrorOf": "none"},
            {"id": 1, "name": "TABLET", "width": 2360, "height": 1640, "mirrorOf": "0"},
        ]);
        let monitors = live_monitors(list.as_array().unwrap());
        assert_eq!(monitors[0].mirror, "");
        assert_eq!(monitors[1].mirror, "eDP-1");
    }

    #[test]
    fn a_disabled_monitor_keeps_the_mode_it_was_saved_with() {
        let json = serde_json::json!({
            "name": "DP-4",
            "description": "a screen",
            "width": 0,
            "height": 0,
            "refreshRate": 0.0,
            "scale": 0.0,
            "disabled": true,
            "availableModes": [],
        });
        let monitor = live_monitor(&json);
        assert!(monitor.connected && !monitor.enabled);
        assert_eq!(monitor.mode, None);
        assert_eq!(monitor.logical(), (0.0, 0.0));
        assert!(lua_block(&monitor).contains("disabled = true"));
    }

    #[test]
    fn a_written_layout_reads_back_the_same() {
        let mut off = monitor();
        off.description = "BOE NE135A1M-NY1".to_owned();
        off.enabled = false;
        let mut turned = monitor();
        turned.transform = 3;
        turned.scale = 1.25;
        turned.vrr = true;
        turned.mirror = "eDP-1".to_owned();
        let written: String = [&monitor(), &turned, &off].map(lua_block).concat();
        assert!(written.contains("output = \"desc:Lenovo LEN P27h-10 0x01010101\""));
        assert!(written.contains("mode = \"2560x1440@59.95\""));
        assert!(written.contains("position = \"-2560x-480\""));
        assert!(lua_block(&monitor()).contains("mirror = \"\""));
        assert!(lua_block(&turned).contains("position = \"auto\""));

        let read = parse_layout(&written);
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].description, "Lenovo LEN P27h-10 0x01010101");
        assert_eq!(read[0].mode.unwrap().width, 2560);
        assert_eq!((read[0].x, read[0].y), (-2560, -480));
        assert!(read[0].enabled && !read[0].vrr);
        assert_eq!(read[1].transform, 3);
        assert_eq!(read[1].scale, 1.25);
        assert_eq!(read[1].mirror, "eDP-1");
        assert!(read[1].vrr);
        assert!(!read[2].enabled);
        assert_eq!(read[2].description, "BOE NE135A1M-NY1");
    }

    #[test]
    fn a_plain_connector_name_survives_the_layout_file() {
        let mut named = monitor();
        named.description = String::new();
        let read = parse_layout(&lua_block(&named));
        assert_eq!(read[0].name, "DP-4");
        assert_eq!(read[0].output(), "DP-4");
    }
}
