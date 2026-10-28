//! A tablet as one more screen: Hyprland makes a headless output, Sunshine
//! streams it with the sound, and Moonlight on the tablet shows it and sends
//! touches and keys back.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use serde_json::Value;

use crate::hypr;
use crate::nm::{failed, run};

pub const OUTPUT: &str = "TABLET";
/// Common tablet panels in pixels, for the headless output's Resolution
/// field; it has no modes of its own.
pub const SIZES: [(u32, u32); 10] = [
    (2048, 1536),
    (2160, 1620),
    (2266, 1488),
    (2360, 1640),
    (2388, 1668),
    (2420, 1668),
    (2732, 2048),
    (2752, 2064),
    (1920, 1200),
    (1920, 1080),
];
/// Sunshine asks for the PIN Moonlight shows without ending the line.
const PIN_PROMPT: &str = "Please insert pin:";
const NOT_ENABLED: &str = "Client certificate identity is not enabled";
pub const PIN_LENGTH: usize = 4;

pub struct Tablet {
    /// This computer's addresses, for adding it in Moonlight by hand.
    pub addresses: Vec<String>,
    pub streams: usize,
    /// Moonlight shows a PIN that Sunshine waits for.
    pub pairing: bool,
    pub pin: String,
    pub error: Option<String>,
    /// The output Sunshine streams: the tablet's own, or the one it copies.
    captured: String,
    sunshine: Sunshine,
    ctx: egui::Context,
}

struct Sunshine {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    /// Sunshine's pairings.
    state: PathBuf,
}

impl Tablet {
    pub fn start(ctx: egui::Context) -> Result<Self, String> {
        log::info!("starting the tablet screen");
        let monitors = run("hyprctl", ["-j", "monitors", "all"])
            .map_err(|err| failed("TAB-1", "list the monitors", err))?;
        if !monitors.contains(&format!("\"{OUTPUT}\"")) {
            hyprctl(&["output", "create", "headless", OUTPUT])
                .map_err(|err| failed("TAB-2", "create the tablet output", err))?;
        }
        // A new output gets Hyprland's rules of the moment, not the layout
        // file's, and 200 %.
        let saved = hypr::saved(OUTPUT);
        let chunk = match &saved {
            Some((block, _)) => block.clone(),
            None => format!(
                "hl.monitor({{ output = \"{OUTPUT}\", mode = \"2360x1640@60\", position = \"auto\", scale = 1 }})"
            ),
        };
        if let Err(err) = hyprctl(&["eval", &chunk]) {
            failed("TAB-3", "size the tablet output", err);
        }
        // An output that shows a copy is gone for screen capture.
        let captured = saved
            .map(|(_, monitor)| monitor.mirror)
            .filter(|mirror| !mirror.is_empty())
            .unwrap_or_else(|| OUTPUT.to_owned());

        let sunshine = Sunshine::start(&ctx, &captured)?;
        Ok(Self {
            addresses: addresses().unwrap_or_else(|err| {
                failed("TAB-7", "read this computer's addresses", err);
                Vec::new()
            }),
            streams: 0,
            pairing: false,
            pin: String::new(),
            error: None,
            captured,
            sunshine,
            ctx,
        })
    }

    /// Sunshine picks its output when it starts, so it is started again
    /// when the tablet starts or stops showing a copy.
    pub fn capture(&mut self, output: &str) {
        if self.captured == output {
            return;
        }
        log::info!("the tablet shows {output}");
        self.restart(output);
    }

    fn restart(&mut self, output: &str) {
        // The new one could not take the ports while the old one holds them.
        self.sunshine.stop();
        match Sunshine::start(&self.ctx, output) {
            Ok(sunshine) => {
                self.sunshine = sunshine;
                self.captured = output.to_owned();
                self.streams = 0;
                self.pairing = false;
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
    }

    pub fn pair(&mut self) {
        log::info!("pairing with PIN ****");
        self.pairing = false;
        let pin = std::mem::take(&mut self.pin);
        if let Err(err) = writeln!(self.sunshine.stdin, "{pin}") {
            self.error = Some(failed(
                "TAB-11",
                "pass the PIN to Sunshine",
                err.to_string(),
            ));
        }
    }

    /// Takes in what Sunshine said since the last frame.
    pub fn poll(&mut self) {
        loop {
            let line = match self.sunshine.lines.try_recv() {
                Ok(line) => line,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if self.error.is_none() {
                        let status = self.sunshine.child.try_wait().ok().flatten();
                        let err = format!("Sunshine stopped ({status:?})");
                        self.error = Some(failed("TAB-8", "stream the screen", err));
                        self.streams = 0;
                        self.pairing = false;
                    }
                    return;
                }
            };
            if line.starts_with(PIN_PROMPT) {
                log::info!("Moonlight wants to pair");
                self.pairing = true;
            } else if line.ends_with("CLIENT CONNECTED") {
                self.streams += 1;
            } else if line.ends_with("CLIENT DISCONNECTED") {
                self.streams = self.streams.saturating_sub(1);
            } else if line.ends_with(NOT_ENABLED)
                && forget_repeated_pairings(&self.sunshine.state).unwrap_or_else(|err| {
                    failed("TAB-13", "forget repeated pairings", err);
                    false
                })
            {
                log::info!("Sunshine reads its pairings only at start");
                self.restart(&self.captured.clone());
            } else if let Some((_, err)) = line.split_once("]: Fatal: ") {
                self.error = Some(failed("TAB-9", "stream the screen", err.to_owned()));
            }
        }
    }
}

impl Sunshine {
    fn start(ctx: &egui::Context, output: &str) -> Result<Self, String> {
        // Pairings and keys live here, so a paired tablet stays paired.
        let dir = crate::state_dir()
            .ok_or("no state directory")
            .map(|dir| dir.join("sunshine"))
            .map_err(|err| failed("TAB-5", "write the Sunshine config", err.to_owned()))?;
        let file = |name: &str| dir.join(name).display().to_string();
        // Sunshine always sends its own output, even when Moonlight asks to play
        // on the PC too, so sound comes out of one side: the default output.
        let config = format!(
            "capture = wlr\noutput_name = {output}\norigin_web_ui_allowed = pc\n\
             audio_sink = sink-sunshine-stereo\n\
             file_state = {}\ncredentials_file = {}\nfile_apps = {}\npkey = {}\ncert = {}\nlog_path = {}\n",
            file("state.json"),
            file("credentials.json"),
            file("apps.json"),
            file("key.pem"),
            file("cert.pem"),
            file("sunshine.log"),
        );
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join("sunshine.conf"), config))
            .and_then(|()| std::fs::write(dir.join("apps.json"), APPS))
            .map_err(|err| failed("TAB-5", "write the Sunshine config", err.to_string()))?;

        let state = dir.join("state.json");
        if let Err(err) = forget_repeated_pairings(&state) {
            failed("TAB-13", "forget repeated pairings", err);
        }

        let path = dir.join("sunshine.conf");
        log::debug!("sunshine -0 {}", path.display());
        let mut child = Command::new("sunshine")
            .arg("-0")
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| {
                let err = match err.kind() {
                    std::io::ErrorKind::NotFound => "Sunshine is not installed".to_owned(),
                    _ => format!("Could not run Sunshine: {err}"),
                };
                failed("TAB-6", "start Sunshine", err)
            })?;
        let stdin = child.stdin.take().expect("a piped stdin");
        let mut stdout = child.stdout.take().expect("a piped stdout");
        let (send, lines) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("sunshine".into())
            .spawn(move || {
                let mut pending = String::new();
                let mut buffer = [0u8; 4096];
                while let Ok(read @ 1..) = stdout.read(&mut buffer) {
                    pending += &String::from_utf8_lossy(&buffer[..read]);
                    let mut lines: Vec<String> = Vec::new();
                    while let Some((line, rest)) = pending.split_once('\n') {
                        lines.push(line.to_owned());
                        pending = rest.to_owned();
                    }
                    if pending.trim_end() == PIN_PROMPT {
                        lines.push(std::mem::take(&mut pending));
                    }
                    for line in lines {
                        if line.contains("drop_elevated_privileges") {
                            continue;
                        }
                        log::debug!("sunshine: {line}");
                        if send.send(line).is_err() {
                            return;
                        }
                    }
                    ctx.request_repaint();
                }
                ctx.request_repaint();
            })
            .expect("a reader thread");
        Ok(Self {
            child,
            stdin,
            lines,
            state,
        })
    }
}

impl Sunshine {
    fn stop(&mut self) {
        log::info!("stopping Sunshine");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Sunshine {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Drop for Tablet {
    fn drop(&mut self) {
        log::info!("stopping the tablet screen");
        // Hyprland cannot remove an output while it shows a copy.
        let chunk =
            format!("hl.monitor({{ output = \"{OUTPUT}\", mirror = \"\", position = \"auto\" }})");
        if let Err(err) = hyprctl(&["eval", &chunk]) {
            failed("TAB-12", "stop the tablet copying a screen", err);
        }
        if let Err(err) = hyprctl(&["output", "remove", OUTPUT]) {
            failed("TAB-10", "remove the tablet output", err);
        }
    }
}

/// Only the desktop: Sunshine's own list also offers Steam and an xrandr
/// mode switch.
const APPS: &str =
    r#"{ "env": {}, "apps": [ { "name": "Desktop", "image-path": "desktop.png" } ] }"#;

/// Sunshine records a Moonlight that pairs again a second time, and then
/// turns it away for matching more than one record.
fn forget_repeated_pairings(state: &Path) -> Result<bool, String> {
    let text = match std::fs::read_to_string(state) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.to_string()),
    };
    let mut json: Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let Some(devices) = json
        .pointer_mut("/root/named_devices")
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    let before = devices.len();
    let mut certs = HashSet::new();
    devices.retain(|device| certs.insert(device["cert"].as_str().unwrap_or_default().to_owned()));
    let repeated = before - devices.len();
    if repeated == 0 {
        return Ok(false);
    }
    log::info!("forgetting {repeated} repeated Moonlight pairings");
    let text = serde_json::to_string_pretty(&json).map_err(|err| err.to_string())?;
    std::fs::write(state, text).map_err(|err| err.to_string())?;
    Ok(true)
}

/// hyprctl answers "ok", or why not, and exits with 0 either way.
fn hyprctl(args: &[&str]) -> Result<(), String> {
    let answer = run("hyprctl", args)?;
    match answer.trim() {
        "ok" => Ok(()),
        other => Err(other.trim_start_matches("error: ").to_owned()),
    }
}

fn addresses() -> Result<Vec<String>, String> {
    let json = run("ip", ["-j", "-4", "addr", "show", "scope", "global"])?;
    Ok(parse_addresses(&json))
}

fn parse_addresses(json: &str) -> Vec<String> {
    let links: Vec<Value> = serde_json::from_str(json).unwrap_or_default();
    links
        .iter()
        .flat_map(|link| link["addr_info"].as_array().cloned().unwrap_or_default())
        .filter_map(|info| info["local"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_global_address_is_offered() {
        let json = r#"[
          {"ifindex":3,"ifname":"wlan0","addr_info":[{"family":"inet","local":"192.168.188.108","prefixlen":24}]},
          {"ifindex":4,"ifname":"enp1","addr_info":[{"family":"inet","local":"10.0.0.5","prefixlen":8}]}
        ]"#;
        assert_eq!(parse_addresses(json), ["192.168.188.108", "10.0.0.5"]);
    }

    #[test]
    fn a_moonlight_that_paired_twice_is_kept_once() {
        let state =
            std::env::temp_dir().join(format!("settings-dialog-state-{}.json", std::process::id()));
        let device = |uuid: &str, cert: &str| {
            format!(r#"{{ "name": "", "cert": "{cert}", "uuid": "{uuid}", "enabled": "true" }}"#)
        };
        let text = format!(
            r#"{{ "root": {{ "uniqueid": "X", "named_devices": [ {}, {}, {} ] }} }}"#,
            device("1", "ipad"),
            device("2", "phone"),
            device("3", "ipad"),
        );
        std::fs::write(&state, text).unwrap();

        assert_eq!(forget_repeated_pairings(&state), Ok(true));
        assert_eq!(forget_repeated_pairings(&state), Ok(false));
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        let uuids: Vec<_> = json["root"]["named_devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|device| device["uuid"].as_str().unwrap())
            .collect();
        assert_eq!(uuids, ["1", "2"]);
        assert_eq!(json["root"]["uniqueid"], "X");
        std::fs::remove_file(&state).unwrap();
    }
}
