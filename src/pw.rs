use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::nm::{failed, run};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub nodes: Vec<Node>,
    pub ports: Vec<Port>,
    pub links: Vec<Link>,
    /// The node names sound goes to and comes from right now, picked by WirePlumber.
    pub current_sink: Option<String>,
    pub current_source: Option<String>,
    /// The node names the user chose; WirePlumber saves them and falls back to
    /// another device while they are missing.
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    /// Bluetooth devices, whose profiles pick the codec.
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub id: u32,
    /// Every available profile but "off", one per codec.
    pub codecs: Vec<Codec>,
    /// The profile index in use.
    pub profile: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Codec {
    pub profile: u32,
    /// "LDAC", or "mSBC, headset" for a profile with the microphone.
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub id: u32,
    /// `node.name`, what the default metadata refers to.
    pub name: String,
    pub description: String,
    /// `media.class`, such as "Audio/Sink" or "Stream/Output/Audio".
    pub class: String,
    pub nick: String,
    /// "alsa", "bluez5", "v4l2", …
    pub api: String,
    pub app: String,
    pub media_name: String,
    /// "running", "idle", "suspended", …
    pub state: String,
    /// "FL, FR".
    pub channels: String,
    pub rates: String,
    /// The rate it runs at now, in Hz.
    pub rate: Option<u32>,
    /// The device object it belongs to.
    pub device: Option<u32>,
    /// Per channel, on wpctl's cubic scale, where 0.5 sounds about half as
    /// loud. Empty for nodes without a volume.
    pub volumes: Vec<f32>,
    /// The channel of each volume: "FL", "FR", …
    pub volume_channels: Vec<String>,
    pub muted: bool,
    /// Where WirePlumber keeps a device's volume. None for streams and
    /// virtual devices, whose volume sits on the node itself.
    pub route: Option<Route>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// The device object, which owns the route.
    pub device: u32,
    pub index: u32,
    /// The route's `device`, the node's `card.profile.device`.
    pub profile_device: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Media {
    Audio,
    Midi,
    Video,
    Other,
}

#[derive(Clone, Debug)]
pub struct Port {
    pub id: u32,
    pub node: u32,
    pub name: String,
    pub direction: Direction,
    pub media: Media,
    /// A sink's copy of what it plays, for recording it.
    pub monitor: bool,
    /// "FL", "FR", "MONO", …; empty for MIDI and video.
    pub channel: String,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub id: u32,
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
    pub state: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Output,
    Input,
}

impl Side {
    fn key(self) -> &'static str {
        match self {
            Self::Output => "audio.sink",
            Self::Input => "audio.source",
        }
    }
}

impl Node {
    /// The loudest channel. None for nodes without a volume.
    pub fn volume(&self) -> Option<f32> {
        self.volumes.iter().copied().reduce(f32::max)
    }

    pub fn is_output(&self) -> bool {
        matches!(self.class.as_str(), "Audio/Sink" | "Audio/Duplex")
    }

    pub fn is_input(&self) -> bool {
        matches!(
            self.class.as_str(),
            "Audio/Source" | "Audio/Source/Virtual" | "Audio/Duplex"
        )
    }

    pub fn is_stream(&self) -> bool {
        self.class.starts_with("Stream/")
    }

    /// The name to show: an app's name for its streams, else the device's.
    pub fn label(&self) -> &str {
        let app = self.is_stream().then_some(self.app.as_str());
        [
            app,
            Some(self.description.as_str()),
            Some(self.nick.as_str()),
            Some(self.media_name.as_str()),
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.is_empty())
        .unwrap_or(&self.name)
    }
}

/// The loudest channel at `volume`, the others keeping their balance to it.
pub fn balanced(volumes: &[f32], volume: f32) -> Vec<f32> {
    let loudest = volumes.iter().copied().fold(0.0, f32::max);
    if loudest <= 0.0 {
        return vec![volume; volumes.len()];
    }
    volumes
        .iter()
        .map(|v| (v / loudest * volume).clamp(0.0, 1.0))
        .collect()
}

/// "Front left" for "FL".
pub fn channel_name(channel: &str) -> &str {
    match channel {
        "FL" => "Front left",
        "FR" => "Front right",
        "FC" => "Center",
        "LFE" => "Subwoofer",
        "RL" => "Rear left",
        "RR" => "Rear right",
        "SL" => "Side left",
        "SR" => "Side right",
        "MONO" => "Mono",
        other => other,
    }
}

impl Snapshot {
    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn port(&self, id: u32) -> Option<&Port> {
        self.ports.iter().find(|p| p.id == id)
    }

    pub fn is_current(&self, node: &Node, side: Side) -> bool {
        let current = match side {
            Side::Output => &self.current_sink,
            Side::Input => &self.current_source,
        };
        current.as_deref() == Some(&node.name)
    }

    pub fn is_default(&self, node: &Node, side: Side) -> bool {
        let default = match side {
            Side::Output => &self.default_sink,
            Side::Input => &self.default_source,
        };
        default.as_deref() == Some(&node.name)
    }

    /// The port pairs that link node `a` to node `b`, or `b` to `a` if `a` has
    /// nothing for `b`: channel to channel, a mono output to every input, and
    /// every output to a mono input. Ports already linked are left out.
    pub fn link_nodes(&self, a: u32, b: u32) -> Vec<(u32, u32)> {
        let pairs = self.node_pairs(a, b);
        if pairs.is_empty() {
            self.node_pairs(b, a)
        } else {
            pairs
        }
    }

    fn node_pairs(&self, from: u32, to: u32) -> Vec<(u32, u32)> {
        let side = |node: u32, direction: Direction, monitor: bool| -> Vec<&Port> {
            self.ports
                .iter()
                .filter(|p| p.node == node && p.direction == direction && p.monitor == monitor)
                .collect()
        };
        let mut outputs = side(from, Direction::Out, false);
        if outputs.is_empty() {
            outputs = side(from, Direction::Out, true);
        }
        let inputs = side(to, Direction::In, false);
        let mut pairs = Vec::new();
        for media in [Media::Audio, Media::Midi, Media::Video, Media::Other] {
            let outs: Vec<&Port> = outputs
                .iter()
                .copied()
                .filter(|p| p.media == media)
                .collect();
            let ins: Vec<&Port> = inputs
                .iter()
                .copied()
                .filter(|p| p.media == media)
                .collect();
            if outs.len() == 1 || ins.len() == 1 {
                pairs.extend(outs.iter().flat_map(|o| ins.iter().map(|i| (o.id, i.id))));
                continue;
            }
            let by_channel: Vec<(u32, u32)> = outs
                .iter()
                .flat_map(|o| {
                    ins.iter()
                        .filter(|i| !o.channel.is_empty() && i.channel == o.channel)
                        .map(|i| (o.id, i.id))
                })
                .collect();
            if by_channel.is_empty() {
                pairs.extend(outs.iter().zip(&ins).map(|(o, i)| (o.id, i.id)));
            } else {
                pairs.extend(by_channel);
            }
        }
        pairs.retain(|(output, input)| {
            !self
                .links
                .iter()
                .any(|l| l.output_port == *output && l.input_port == *input)
        });
        pairs
    }

    /// The output and input port to link when the user picks `a` and then `b`,
    /// in either order.
    pub fn linkable(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (a, b) = (self.port(a)?, self.port(b)?);
        if a.node == b.node || a.direction == b.direction || a.media != b.media {
            return None;
        }
        let (output, input) = match a.direction {
            Direction::Out => (a.id, b.id),
            Direction::In => (b.id, a.id),
        };
        let linked = self
            .links
            .iter()
            .any(|l| l.output_port == output && l.input_port == input);
        (!linked).then_some((output, input))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Current,
    Default,
    Link,
    Unlink,
    Volume,
    Mute,
    Codec,
}

#[derive(Debug, PartialEq)]
pub enum Request {
    Refresh,
    /// Sends sound there until a device comes or goes; not saved.
    UseNow {
        name: String,
        side: Side,
    },
    /// Saved by WirePlumber, and used straight away.
    MakeDefault {
        id: u32,
        name: String,
    },
    /// Output and input port pairs.
    Link(Vec<(u32, u32)>),
    Unlink(Vec<u32>),
    /// Per channel, cubic.
    Volume {
        id: u32,
        route: Option<Route>,
        volumes: Vec<f32>,
    },
    Mute {
        id: u32,
        muted: bool,
    },
    /// Picks a Bluetooth device's codec through its profile; saved by WirePlumber.
    Codec {
        device: u32,
        profile: u32,
    },
}

impl Request {
    fn changes(&self) -> bool {
        !matches!(self, Self::Refresh)
    }
}

pub enum Reply {
    Snapshot(Result<Snapshot, String>),
    Done(Op, Result<(), String>),
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
            .name("sound".into())
            .spawn(move || {
                while let Ok(first) = inbox.recv() {
                    let batch = squash(std::iter::once(first).chain(inbox.try_iter()).collect());
                    let refresh = batch.iter().any(Request::changes);
                    for request in batch {
                        let _ = outbox.send(handle(request));
                    }
                    if refresh {
                        let snap =
                            snapshot().map_err(|err| failed("SND-6", "refresh after change", err));
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

/// A dragged slider sends a volume every frame, faster than wpctl runs, so
/// only the newest volume per node and one refresh are kept.
fn squash(batch: Vec<Request>) -> Vec<Request> {
    let mut seen = Vec::new();
    let mut kept = Vec::new();
    for request in batch.into_iter().rev() {
        let key = match &request {
            Request::Refresh => Some(None),
            Request::Volume { id, .. } => Some(Some(*id)),
            _ => None,
        };
        if let Some(key) = key {
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
        }
        kept.push(request);
    }
    kept.reverse();
    kept
}

fn handle(request: Request) -> Reply {
    match request {
        Request::Refresh => {
            Reply::Snapshot(snapshot().map_err(|err| failed("SND-1", "read the graph", err)))
        }
        Request::UseNow { name, side } => {
            log::info!("using '{name}' now as {}", side.key());
            let value = serde_json::json!({ "name": name }).to_string();
            let key = format!("default.{}", side.key());
            let result = run(
                "pw-metadata",
                ["-n", "default", "0", &key, &value, "Spa:String:JSON"],
            );
            Reply::Done(
                Op::Current,
                result
                    .map(drop)
                    .map_err(|err| failed("SND-2", "use a device now", err)),
            )
        }
        Request::MakeDefault { id, name } => {
            log::info!("making '{name}' ({id}) the default");
            let result = run("wpctl", ["set-default", &id.to_string()]);
            Reply::Done(
                Op::Default,
                result
                    .map(drop)
                    .map_err(|err| failed("SND-3", "make a device the default", err)),
            )
        }
        Request::Link(pairs) => {
            log::info!("linking ports {pairs:?}");
            let result = pairs.iter().try_for_each(|(output, input)| {
                run("pw-link", [output.to_string(), input.to_string()]).map(drop)
            });
            Reply::Done(
                Op::Link,
                result.map_err(|err| failed("SND-4", "link ports", err)),
            )
        }
        Request::Unlink(links) => {
            log::info!("unlinking {links:?}");
            let result = links
                .iter()
                .try_for_each(|id| run("pw-link", ["-d", &id.to_string()]).map(drop));
            Reply::Done(
                Op::Unlink,
                result.map_err(|err| failed("SND-5", "unlink ports", err)),
            )
        }
        Request::Volume { id, route, volumes } => {
            log::info!("setting the volume of {id} to {volumes:.2?}");
            let linear: Vec<String> = volumes
                .iter()
                .map(|v| format!("{:.6}", v.clamp(0.0, 1.0).powi(3)))
                .collect();
            let props = format!("channelVolumes: [ {} ]", linear.join(", "));
            // Like wpctl: a device's volume goes to its route, so the sound card's
            // mixer follows and WirePlumber saves it.
            let (object, param, value) = match route {
                Some(route) => (
                    route.device,
                    "Route",
                    format!(
                        "{{ index: {}, device: {}, props: {{ {props} }}, save: true }}",
                        route.index, route.profile_device
                    ),
                ),
                None => (id, "Props", format!("{{ {props} }}")),
            };
            let result = pw_cli(&["set-param", &object.to_string(), param, &value]);
            Reply::Done(
                Op::Volume,
                result.map_err(|err| failed("SND-7", "set the volume", err)),
            )
        }
        Request::Mute { id, muted } => {
            log::info!("{} {id}", if muted { "muting" } else { "unmuting" });
            let result = run(
                "wpctl",
                ["set-mute", &id.to_string(), if muted { "1" } else { "0" }],
            );
            Reply::Done(
                Op::Mute,
                result
                    .map(drop)
                    .map_err(|err| failed("SND-8", "mute or unmute", err)),
            )
        }
        Request::Codec { device, profile } => {
            log::info!("switching device {device} to profile {profile}");
            let value = format!("{{ index: {profile}, save: true }}");
            let result = pw_cli(&["set-param", &device.to_string(), "Profile", &value]);
            Reply::Done(
                Op::Codec,
                result.map_err(|err| failed("SND-11", "switch the codec", err)),
            )
        }
    }
}

/// pw-cli exits with 0 even when it fails, and says so on stderr.
fn pw_cli(args: &[&str]) -> Result<(), String> {
    let started = Instant::now();
    let output = Command::new("pw-cli")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "pw-cli is not installed".to_owned(),
            _ => format!("Could not run pw-cli: {err}"),
        })?;
    log::debug!(
        "pw-cli {} -> {} in {:?}",
        args.join(" "),
        output.status,
        started.elapsed()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    match stderr.lines().find(|l| l.to_lowercase().contains("error")) {
        Some(line) => Err(line.trim().to_owned()),
        None if output.status.success() => Ok(()),
        None => Err(format!("pw-cli failed: {}", output.status)),
    }
}

pub fn snapshot() -> Result<Snapshot, String> {
    parse_dump(&run("pw-dump", ["--no-colors"])?)
}

fn parse_dump(json: &str) -> Result<Snapshot, String> {
    let objects: Vec<Value> = serde_json::from_str(json)
        .map_err(|err| format!("Unexpected answer from pw-dump: {err}"))?;
    let mut snap = Snapshot::default();
    // Node, device and the device's `card.profile.device`; and per device its
    // routes' index and device.
    let mut on_devices = Vec::new();
    let mut routes = Vec::new();
    for object in &objects {
        let id = object["id"].as_u64().unwrap_or_default() as u32;
        let info = &object["info"];
        let props = &info["props"];
        let text = |key: &str| match &props[key] {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        match object["type"].as_str().unwrap_or_default() {
            "PipeWire:Interface:Node" => {
                let props_param = info["params"]["Props"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|p| p["channelVolumes"].is_array());
                // pw-dump has the linear amplitude; wpctl and the sliders are cubic.
                let volumes: Vec<f32> = props_param
                    .and_then(|p| p["channelVolumes"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_f64)
                    .map(|linear| linear.cbrt() as f32)
                    .collect();
                let volume_channels: Vec<String> = props_param
                    .and_then(|p| p["channelMap"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
                if let (Some(device), Some(profile_device)) = (
                    props["device.id"].as_u64(),
                    props["card.profile.device"].as_u64(),
                ) {
                    on_devices.push((id, device as u32, profile_device as u32));
                }
                snap.nodes.push(Node {
                    id,
                    name: text("node.name"),
                    description: text("node.description"),
                    class: text("media.class"),
                    nick: text("node.nick"),
                    api: text("device.api"),
                    app: text("application.name"),
                    media_name: text("media.name"),
                    state: info["state"].as_str().unwrap_or_default().to_owned(),
                    channels: list(&text("audio.position")),
                    rates: list(&text("audio.allowed-rates")),
                    rate: info["params"]["Format"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find_map(|f| f["rate"].as_u64())
                        .map(|rate| rate as u32),
                    device: props["device.id"].as_u64().map(|id| id as u32),
                    volumes,
                    volume_channels,
                    muted: props_param.is_some_and(|p| p["mute"] == true),
                    route: None,
                });
            }
            "PipeWire:Interface:Device" => {
                for route in info["params"]["Route"].as_array().into_iter().flatten() {
                    if let (Some(index), Some(device)) =
                        (route["index"].as_u64(), route["device"].as_u64())
                    {
                        routes.push((id, index as u32, device as u32));
                    }
                }
                if props["device.api"] == "bluez5" {
                    let params = &info["params"];
                    snap.devices.push(Device {
                        id,
                        codecs: params["EnumProfile"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|p| p["name"] != "off" && p["available"] != "no")
                            .filter_map(|p| {
                                Some(Codec {
                                    profile: p["index"].as_u64()? as u32,
                                    label: codec_label(
                                        p["name"].as_str()?,
                                        p["description"].as_str()?,
                                    ),
                                })
                            })
                            .collect(),
                        profile: params["Profile"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find_map(|p| p["index"].as_u64())
                            .map(|index| index as u32),
                    });
                }
            }
            "PipeWire:Interface:Port" => {
                let format = text("format.dsp");
                snap.ports.push(Port {
                    id,
                    node: props["node.id"].as_u64().unwrap_or_default() as u32,
                    name: text("port.name"),
                    direction: match info["direction"].as_str() {
                        Some("input") => Direction::In,
                        _ => Direction::Out,
                    },
                    // Video ports carry no DSP format.
                    media: if format.ends_with("audio") {
                        Media::Audio
                    } else if format.ends_with("midi") {
                        Media::Midi
                    } else if format.is_empty() {
                        Media::Video
                    } else {
                        Media::Other
                    },
                    monitor: props["port.monitor"].as_bool().unwrap_or(false),
                    channel: text("audio.channel"),
                });
            }
            "PipeWire:Interface:Link" => {
                let number = |key: &str| info[key].as_u64().unwrap_or_default() as u32;
                snap.links.push(Link {
                    id,
                    output_node: number("output-node-id"),
                    output_port: number("output-port-id"),
                    input_node: number("input-node-id"),
                    input_port: number("input-port-id"),
                    state: info["state"].as_str().unwrap_or_default().to_owned(),
                });
            }
            "PipeWire:Interface:Metadata" if object["props"]["metadata.name"] == "default" => {
                for entry in object["metadata"].as_array().into_iter().flatten() {
                    let name = default_name(&entry["value"]);
                    match entry["key"].as_str().unwrap_or_default() {
                        "default.audio.sink" => snap.current_sink = name,
                        "default.audio.source" => snap.current_source = name,
                        "default.configured.audio.sink" => snap.default_sink = name,
                        "default.configured.audio.source" => snap.default_source = name,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    for (node, device, profile_device) in on_devices {
        let route = routes
            .iter()
            .find(|(d, _, rd)| *d == device && *rd == profile_device);
        if let (Some(node), Some((_, index, _))) =
            (snap.nodes.iter_mut().find(|n| n.id == node), route)
        {
            node.route = Some(Route {
                device,
                index: *index,
                profile_device,
            });
        }
    }
    // Our own level meters are no part of the user's graph.
    let meters: Vec<u32> = snap
        .nodes
        .iter()
        .filter(|n| n.name.starts_with(METER))
        .map(|n| n.id)
        .collect();
    snap.nodes.retain(|n| !meters.contains(&n.id));
    snap.ports.retain(|p| !meters.contains(&p.node));
    snap.links
        .retain(|l| !meters.contains(&l.input_node) && !meters.contains(&l.output_node));
    // Video devices show up as video ports only on video nodes.
    for port in &mut snap.ports {
        let video = snap
            .nodes
            .iter()
            .any(|n| n.id == port.node && n.class.contains("Video"));
        if port.media == Media::Video && !video {
            port.media = Media::Other;
        }
    }
    Ok(snap)
}

const METER: &str = "settings-dialog-meter";
const METER_RATE: usize = 12_000;
/// Samples per channel in one reading: 50 ms.
const METER_WINDOW: usize = METER_RATE / 20;

/// Reads the signal level on output ports: a `pw-record` that nothing links
/// to by itself, with one channel per port, linked to those ports with
/// `pw-link`. Dropping it stops the recording and the links go with it.
pub struct Meter {
    pub ports: Vec<u32>,
    child: Option<Child>,
    levels: Receiver<Result<Vec<f32>, String>>,
}

impl Meter {
    pub fn start(ctx: egui::Context, ports: Vec<u32>) -> Self {
        static STARTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let count = STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("{METER}-{}-{count}", std::process::id());
        let (sender, levels) = mpsc::channel();
        let channels = ports.len();
        let map: Vec<String> = (0..channels).map(|i| format!("AUX{i}")).collect();
        // Passive, so it never keeps a device awake; no restore, so WirePlumber
        // doesn't save a state entry for every meter.
        let properties = format!(
            "{{ node.name = {name} application.name = settings-dialog node.passive = true \
             node.dont-reconnect = true state.restore-props = false state.restore-target = false }}"
        );
        let args = [
            "--target".to_owned(),
            "0".to_owned(),
            "--raw".to_owned(),
            "--format".to_owned(),
            "f32".to_owned(),
            "--rate".to_owned(),
            METER_RATE.to_string(),
            "--channels".to_owned(),
            channels.to_string(),
            "--channel-map".to_owned(),
            map.join(","),
            "--latency".to_owned(),
            "50ms".to_owned(),
            "-P".to_owned(),
            properties,
            "-".to_owned(),
        ];
        log::debug!("$ pw-record {}", args.join(" "));
        let child = Command::new("pw-record")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(err) => {
                let _ = sender.send(Err(failed(
                    "SND-9",
                    "start the level meter",
                    err.to_string(),
                )));
                return Self {
                    ports,
                    child: None,
                    levels,
                };
            }
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        let linked = ports.clone();
        std::thread::Builder::new()
            .name("meter".into())
            .spawn(move || {
                // The node shows up a moment after pw-record starts.
                let started = Instant::now();
                for (channel, port) in linked.iter().enumerate() {
                    let target = format!("{name}:input_AUX{channel}");
                    loop {
                        match run("pw-link", [port.to_string(), target.clone()]) {
                            Ok(_) => break,
                            Err(_) if started.elapsed() < Duration::from_secs(2) => {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            Err(err) => {
                                let err = failed("SND-10", "link the level meter", err);
                                let _ = sender.send(Err(err));
                                return;
                            }
                        }
                    }
                }
                let mut buffer = vec![0u8; channels * 4 * METER_WINDOW];
                while stdout.read_exact(&mut buffer).is_ok() {
                    let mut peaks = vec![0.0f32; channels];
                    for (index, sample) in buffer.as_chunks::<4>().0.iter().enumerate() {
                        let sample = f32::from_le_bytes(*sample);
                        let peak = &mut peaks[index % channels];
                        *peak = peak.max(sample.abs());
                    }
                    if sender.send(Ok(peaks)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
                log::debug!("level meter {name} stopped");
            })
            .expect("a meter thread");
        Self {
            ports,
            child: Some(child),
            levels,
        }
    }

    /// The newest peak level per port, 0 to 1, if any came since the last call.
    pub fn poll(&self) -> Option<Result<Vec<f32>, String>> {
        self.levels.try_iter().last()
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `{"name":"…"}`, which older versions of pw-dump give as a string.
fn default_name(value: &Value) -> Option<String> {
    let parsed;
    let value = match value {
        Value::String(text) => {
            parsed = serde_json::from_str::<Value>(text).ok()?;
            &parsed
        }
        other => other,
    };
    value["name"].as_str().map(str::to_owned)
}

/// "FL, FR" from `[ FL, FR ]` or `["FL","FR"]`, however it is spaced.
fn list(text: &str) -> String {
    text.split(|c: char| c == ',' || c.is_whitespace() || "[]\"".contains(c))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The description is in the PipeWire server's language, but ends in a
/// translated "(…, codec LDAC)"; the profile name tells the headset apart.
fn codec_label(name: &str, description: &str) -> String {
    let codec = description
        .strip_suffix(')')
        .and_then(|d| d.rsplit_once(", "))
        .and_then(|(_, codec)| codec.split_once(' '))
        .map_or(description, |(_, codec)| codec);
    let codec = if codec == "MSBC" { "mSBC" } else { codec };
    if name.starts_with("headset") {
        format!("{codec}, headset")
    } else {
        codec.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},
       "metadata":[
         {"subject":0,"key":"default.configured.audio.sink","type":"Spa:String:JSON","value":{"name":"headphones"}},
         {"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"speakers"}},
         {"subject":0,"key":"default.audio.source","type":"Spa:String:JSON","value":"{\"name\":\"mic\"}"}]},
      {"id":53,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
         "node.name":"speakers","node.description":"Speakers","media.class":"Audio/Sink",
         "device.api":"alsa","audio.position":"[ FL, FR ]","audio.allowed-rates":"[\n 44100,\n 48000\n]",
         "device.id":46,"card.profile.device":3},
         "params":{"Props":[{"device":"hw:1"},{"volume":1.0,"mute":true,"channelVolumes":[0.125,0.064],"channelMap":["FL","FR"]}]}}},
      {"id":46,"type":"PipeWire:Interface:Device","info":{"props":{},"params":{"Route":[
         {"index":0,"device":0,"direction":"Input"},{"index":2,"device":3,"direction":"Output"}]}}},
      {"id":54,"type":"PipeWire:Interface:Node","info":{"state":"suspended","props":{
         "node.name":"mic","node.description":"Microphone","media.class":"Audio/Source"}}},
      {"id":80,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
         "node.name":"Firefox","application.name":"Firefox","media.name":"Video","media.class":"Stream/Output/Audio"}}},
      {"id":58,"type":"PipeWire:Interface:Port","info":{"direction":"input","props":{
         "node.id":53,"port.name":"playback_FL","format.dsp":"32 bit float mono audio"}}},
      {"id":60,"type":"PipeWire:Interface:Port","info":{"direction":"output","props":{
         "node.id":53,"port.name":"monitor_FL","format.dsp":"32 bit float mono audio","port.monitor":true}}},
      {"id":64,"type":"PipeWire:Interface:Port","info":{"direction":"output","props":{
         "node.id":54,"port.name":"capture_FL","format.dsp":"32 bit float mono audio"}}},
      {"id":81,"type":"PipeWire:Interface:Port","info":{"direction":"output","props":{
         "node.id":80,"port.name":"output_FL","format.dsp":"32 bit float mono audio"}}},
      {"id":90,"type":"PipeWire:Interface:Link","info":{"output-node-id":80,"output-port-id":81,
         "input-node-id":53,"input-port-id":58,"state":"active"}}
    ]"#;

    #[test]
    fn a_bluetooth_device_gives_its_codecs_and_the_rate_in_use() {
        let snap = parse_dump(
            r#"[
          {"id":79,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
             "node.name":"bluez_output.88_C9","media.class":"Audio/Sink","device.api":"bluez5","device.id":78},
             "params":{"Format":[{"mediaType":"audio","format":"F32LE","rate":96000,"channels":2}]}}},
          {"id":78,"type":"PipeWire:Interface:Device","info":{"props":{"device.api":"bluez5"},"params":{
             "EnumProfile":[
               {"index":0,"name":"off","description":"Aus","available":"yes"},
               {"index":131073,"name":"a2dp-sink-sbc","description":"High Fidelity-Wiedergabe (A2DP-Senke, Codec SBC)","available":"yes"},
               {"index":131076,"name":"a2dp-sink-aac","description":"High Fidelity-Wiedergabe (A2DP-Senke, Codec AAC)","available":"no"},
               {"index":131077,"name":"a2dp-sink-aptx_hd","description":"High Fidelity Playback (A2DP Sink, codec aptX HD)","available":"yes"},
               {"index":131080,"name":"a2dp-sink","description":"High Fidelity-Wiedergabe (A2DP-Senke, Codec LDAC)","available":"yes"},
               {"index":196865,"name":"headset-head-unit","description":"Sprechkopfhörer-Einheit (HSP/HFP, Codec MSBC)","available":"yes"}],
             "Profile":[{"index":131080,"name":"a2dp-sink","save":false}]}}}
        ]"#,
        )
        .unwrap();
        let node = snap.node(79).unwrap();
        assert_eq!((node.rate, node.device), (Some(96000), Some(78)));
        let device = &snap.devices[0];
        assert_eq!((device.id, device.profile), (78, Some(131080)));
        let codecs: Vec<(u32, &str)> = device
            .codecs
            .iter()
            .map(|c| (c.profile, c.label.as_str()))
            .collect();
        assert_eq!(
            codecs,
            [
                (131073, "SBC"),
                (131077, "aptX HD"),
                (131080, "LDAC"),
                (196865, "mSBC, headset")
            ]
        );
    }

    #[test]
    fn the_dump_gives_devices_ports_links_and_defaults() {
        let snap = parse_dump(DUMP).unwrap();
        assert_eq!(snap.current_sink.as_deref(), Some("speakers"));
        assert_eq!(snap.default_sink.as_deref(), Some("headphones"));
        assert_eq!(snap.current_source.as_deref(), Some("mic"));
        assert_eq!(snap.default_source, None);

        let speakers = snap.node(53).unwrap();
        assert!(speakers.is_output() && !speakers.is_input());
        assert_eq!(speakers.channels, "FL, FR");
        assert_eq!(speakers.rates, "44100, 48000");
        assert!(snap.is_current(speakers, Side::Output));
        assert!(!snap.is_default(speakers, Side::Output));
        assert_eq!(snap.node(80).unwrap().label(), "Firefox");
        assert!(
            (speakers.volume().unwrap() - 0.5).abs() < 1e-4,
            "cubic, loudest channel"
        );
        assert!((speakers.volumes[1] - 0.4).abs() < 1e-4);
        assert_eq!(speakers.volume_channels, ["FL", "FR"]);
        assert!(speakers.muted);
        assert_eq!(
            speakers.route,
            Some(Route {
                device: 46,
                index: 2,
                profile_device: 3
            }),
            "the device's output route"
        );
        assert_eq!(snap.node(54).unwrap().volume(), None);
        assert_eq!(snap.node(80).unwrap().route, None);

        let monitor = snap.port(60).unwrap();
        assert!(monitor.monitor && monitor.direction == Direction::Out);
        assert_eq!(monitor.media, Media::Audio);
        assert_eq!(snap.links[0].input_port, 58);
    }

    #[test]
    fn ports_link_output_to_input_of_the_same_media_once() {
        let snap = parse_dump(DUMP).unwrap();
        assert_eq!(snap.linkable(58, 64), Some((64, 58)));
        assert_eq!(snap.linkable(64, 58), Some((64, 58)));
        assert_eq!(snap.linkable(81, 58), None, "already linked");
        assert_eq!(snap.linkable(60, 58), None, "same node");
        assert_eq!(snap.linkable(64, 81), None, "two outputs");
    }

    #[test]
    fn linking_two_nodes_pairs_their_channels() {
        let port = |id, node, direction, channel: &str, monitor| Port {
            id,
            node,
            name: format!("port{id}"),
            direction,
            media: Media::Audio,
            monitor,
            channel: channel.to_owned(),
        };
        use Direction::{In, Out};
        let snap = Snapshot {
            ports: vec![
                // 1: a stereo player, 2: speakers with monitors, 3: a mono mic,
                // 4: a mono recorder.
                port(10, 1, Out, "FL", false),
                port(11, 1, Out, "FR", false),
                port(21, 2, In, "FR", false),
                port(20, 2, In, "FL", false),
                port(22, 2, Out, "FL", true),
                port(23, 2, Out, "FR", true),
                port(30, 3, Out, "MONO", false),
                port(40, 4, In, "MONO", false),
            ],
            links: vec![Link {
                id: 90,
                output_node: 1,
                output_port: 10,
                input_node: 2,
                input_port: 20,
                state: "active".to_owned(),
            }],
            ..Snapshot::default()
        };
        assert_eq!(snap.link_nodes(1, 2), [(11, 21)], "FL is linked already");
        assert_eq!(snap.link_nodes(2, 1), [(11, 21)], "either order");
        assert_eq!(snap.link_nodes(3, 2), [(30, 21), (30, 20)], "mono to both");
        assert_eq!(snap.link_nodes(1, 4), [(10, 40), (11, 40)], "both to mono");
        assert_eq!(
            snap.link_nodes(2, 4),
            [(22, 40), (23, 40)],
            "a sink records from its monitors"
        );
        assert_eq!(snap.link_nodes(3, 1), [], "two sources");
    }

    #[test]
    fn a_dragged_slider_sends_only_its_last_volume() {
        let volume = |id, volume| Request::Volume {
            id,
            route: None,
            volumes: vec![volume],
        };
        let batch = vec![
            Request::Refresh,
            volume(1, 0.1),
            volume(2, 0.5),
            Request::Mute { id: 1, muted: true },
            volume(1, 0.2),
            Request::Refresh,
            volume(1, 0.3),
        ];
        assert_eq!(
            squash(batch),
            [
                volume(2, 0.5),
                Request::Mute { id: 1, muted: true },
                Request::Refresh,
                volume(1, 0.3),
            ]
        );
    }

    #[test]
    fn the_volume_keeps_the_balance_between_channels() {
        assert_eq!(balanced(&[0.8, 0.4], 0.4), [0.4, 0.2]);
        assert_eq!(balanced(&[0.0, 0.0], 0.3), [0.3, 0.3], "none to keep");
    }
}
