use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::Value;

use crate::finder::Signal;
use crate::nm::{failed, run};
use crate::pw;

/// How long one discovery runs; the Bluetooth screen starts the next one.
const DISCOVERY_SECONDS: &str = "30";

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub adapter: Option<Adapter>,
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default)]
pub struct Adapter {
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    /// The BlueZ object, such as "/org/bluez/hci0/dev_88_C9_E8_0B_08_F0".
    pub path: String,
    pub address: String,
    /// "public" or "random".
    pub address_type: String,
    /// What the device calls itself; many LE devices send none.
    pub name: Option<String>,
    /// The name to show: set by the user, else the device's name, else its address.
    pub alias: String,
    /// A freedesktop icon name such as "audio-headset".
    pub icon: String,
    pub class: u32,
    pub paired: bool,
    pub trusted: bool,
    pub blocked: bool,
    pub connected: bool,
    /// dBm, only while discovery hears the device advertise.
    pub rssi: Option<i16>,
    pub tx_power: Option<i16>,
    /// Percent.
    pub battery: Option<u8>,
    pub uuids: Vec<String>,
    pub modalias: String,
    pub manufacturers: Vec<u16>,
}

#[derive(Clone, Debug)]
pub enum Op {
    Power,
    Pair,
    Connect,
    Disconnect,
    Trust,
    Block,
    Remove,
}

pub enum Request {
    Refresh,
    /// Starts a discovery unless one is running.
    Scan,
    StopScan,
    Power(bool),
    /// Pairs, trusts and connects.
    Pair {
        address: String,
        name: String,
    },
    Connect {
        address: String,
        name: String,
    },
    Disconnect {
        address: String,
        name: String,
    },
    Trust {
        address: String,
        on: bool,
    },
    Block {
        address: String,
        on: bool,
    },
    Remove {
        address: String,
        name: String,
    },
}

impl Request {
    /// Changes are counted as in flight and followed by a fresh snapshot.
    fn changes(&self) -> bool {
        !matches!(self, Self::Refresh | Self::Scan | Self::StopScan)
    }
}

pub enum Reply {
    Snapshot(Result<Snapshot, String>),
    /// The code to type on the device being paired.
    Passkey(String),
    Scan(Result<(), String>),
    Done(Op, Result<(), String>),
}

pub struct Backend {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    in_flight: usize,
}

impl Backend {
    pub fn start(ctx: egui::Context) -> Self {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("bluetooth".into())
            .spawn(move || {
                // Discovery lasts as long as the bluetoothctl that started it.
                let mut discovery: Option<Child> = None;
                for request in inbox {
                    if let Some(child) = &mut discovery
                        && let Ok(Some(status)) = child.try_wait()
                    {
                        log::debug!("discovery ended: {status}");
                        discovery = None;
                    }
                    let refresh = request.changes();
                    let notify = |reply: Reply| {
                        let _ = outbox.send(reply);
                        ctx.request_repaint();
                    };
                    if let Some(reply) = handle(request, &mut discovery, &notify) {
                        notify(reply);
                    }
                    if refresh {
                        notify(Reply::Snapshot(
                            snapshot().map_err(|err| failed("BT-10", "refresh after change", err)),
                        ));
                    }
                }
                stop_discovery(&mut discovery);
            })
            .expect("a worker thread");
        Self {
            requests,
            replies,
            in_flight: 0,
        }
    }

    pub fn send(&mut self, request: Request) {
        if request.changes() {
            self.in_flight += 1;
        }
        let _ = self.requests.send(request);
    }

    pub fn busy(&self) -> bool {
        self.in_flight > 0
    }

    pub fn poll(&mut self) -> Vec<Reply> {
        let replies: Vec<Reply> = self.replies.try_iter().collect();
        for reply in &replies {
            if matches!(reply, Reply::Done(..)) {
                self.in_flight = self.in_flight.saturating_sub(1);
            }
        }
        replies
    }
}

fn handle(
    request: Request,
    discovery: &mut Option<Child>,
    notify: &dyn Fn(Reply),
) -> Option<Reply> {
    let reply = match request {
        Request::Refresh => {
            Reply::Snapshot(snapshot().map_err(|err| failed("BT-1", "read devices", err)))
        }
        Request::Scan => {
            if discovery.is_some() {
                return None;
            }
            let started = Command::new("bluetoothctl")
                .args(["--timeout", DISCOVERY_SECONDS, "scan", "on"])
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            log::debug!("bluetoothctl --timeout {DISCOVERY_SECONDS} scan on");
            match started {
                Ok(child) => {
                    *discovery = Some(child);
                    Reply::Scan(Ok(()))
                }
                Err(err) => Reply::Scan(Err(failed(
                    "BT-3",
                    "start discovery",
                    format!("Could not run bluetoothctl: {err}"),
                ))),
            }
        }
        Request::StopScan => {
            stop_discovery(discovery);
            return None;
        }
        Request::Power(on) => Reply::Done(
            Op::Power,
            power(on).map_err(|err| failed("BT-2", "switch Bluetooth", err)),
        ),
        Request::Pair { address, name } => {
            log::info!("pairing with '{name}' ({address})");
            // BlueZ pairs more reliably while it isn't discovering.
            stop_discovery(discovery);
            let result = pair(&address, notify)
                .map_err(|err| failed("BT-4", "pair", err))
                .and_then(|()| {
                    bluetoothctl(["trust", &address])
                        .map_err(|err| failed("BT-7", "trust device", err))
                })
                .and_then(|_| {
                    bluetoothctl(["connect", &address])
                        .map_err(|err| failed("BT-5", "connect device", err))
                });
            Reply::Done(Op::Pair, result.map(drop))
        }
        Request::Connect { address, name } => {
            log::info!("connecting '{name}' ({address})");
            Reply::Done(
                Op::Connect,
                bluetoothctl(["connect", &address])
                    .map(drop)
                    .map_err(|err| failed("BT-5", "connect device", err)),
            )
        }
        Request::Disconnect { address, name } => {
            log::info!("disconnecting '{name}' ({address})");
            Reply::Done(
                Op::Disconnect,
                bluetoothctl(["disconnect", &address])
                    .map(drop)
                    .map_err(|err| failed("BT-6", "disconnect device", err)),
            )
        }
        Request::Trust { address, on } => Reply::Done(
            Op::Trust,
            bluetoothctl([if on { "trust" } else { "untrust" }, &address])
                .map(drop)
                .map_err(|err| failed("BT-7", "trust device", err)),
        ),
        Request::Block { address, on } => Reply::Done(
            Op::Block,
            bluetoothctl([if on { "block" } else { "unblock" }, &address])
                .map(drop)
                .map_err(|err| failed("BT-8", "block device", err)),
        ),
        Request::Remove { address, name } => {
            log::info!("removing '{name}' ({address})");
            Reply::Done(
                Op::Remove,
                bluetoothctl(["remove", &address])
                    .map(drop)
                    .map_err(|err| failed("BT-9", "remove device", err)),
            )
        }
    };
    Some(reply)
}

fn stop_discovery(discovery: &mut Option<Child>) {
    if let Some(mut child) = discovery.take() {
        log::debug!("stopping discovery");
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn bluetoothctl<const N: usize>(args: [&str; N]) -> Result<String, String> {
    run("bluetoothctl", args)
        .map(|out| plain(&out))
        .map_err(|err| plain(&err))
}

fn power(on: bool) -> Result<(), String> {
    if !on {
        return bluetoothctl(["power", "off"]).map(drop);
    }
    // A soft-blocked radio can't be powered; once unblocked, the adapter needs
    // a moment before it takes commands.
    run("rfkill", ["unblock", "bluetooth"])?;
    let mut tries = 0;
    loop {
        match bluetoothctl(["power", "on"]) {
            Ok(_) => return Ok(()),
            Err(err) if tries >= 10 => return Err(err),
            Err(err) => log::debug!("power on not taken yet: {err}"),
        }
        tries += 1;
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Runs `pair` with an agent that can only display, so BlueZ never asks us to
/// confirm or type anything: a passkey the device needs is shown to the user.
fn pair(address: &str, notify: &dyn Fn(Reply)) -> Result<(), String> {
    log::debug!("bluetoothctl --agent DisplayOnly pair {address}");
    let mut child = Command::new("bluetoothctl")
        .args(["--agent", "DisplayOnly", "pair", address])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("Could not run bluetoothctl: {err}"))?;
    let mut last = String::new();
    let stdout = child.stdout.take().expect("piped stdout");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = plain(&line);
        log::debug!("bluetoothctl: {line}");
        if let Some(code) = passkey(&line) {
            notify(Reply::Passkey(code));
        }
        if !line.trim().is_empty() {
            last = line.trim().to_owned();
        }
    }
    let status = child
        .wait()
        .map_err(|err| format!("bluetoothctl failed: {err}"))?;
    log::debug!("bluetoothctl pair {address} -> {status}");
    if status.success() {
        Ok(())
    } else if last.is_empty() {
        Err(format!("bluetoothctl failed ({status})"))
    } else {
        Err(last)
    }
}

/// "[agent] Passkey: 123456" or, for old devices, "[agent] PIN code: 0000".
fn passkey(line: &str) -> Option<String> {
    let (_, rest) = line
        .split_once("Passkey: ")
        .or_else(|| line.split_once("PIN code: "))?;
    let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!code.is_empty()).then_some(code)
}

/// bluetoothctl colours its output and marks its prompt for readline.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            '\x01' | '\x02' | '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// All of BlueZ's objects in one D-Bus call, instead of a bluetoothctl run per device.
fn snapshot() -> Result<Snapshot, String> {
    let json = run(
        "busctl",
        [
            "--json=short",
            "call",
            "org.bluez",
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
        ],
    )?;
    parse_objects(&json)
}

fn parse_objects(json: &str) -> Result<Snapshot, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|err| format!("Unexpected answer from BlueZ: {err}"))?;
    let objects = root["data"][0]
        .as_object()
        .ok_or("Unexpected answer from BlueZ: no objects")?;
    let text =
        |props: &Value, key: &str| props[key]["data"].as_str().unwrap_or_default().to_owned();
    let flag = |props: &Value, key: &str| props[key]["data"].as_bool().unwrap_or(false);
    let number = |props: &Value, key: &str| props[key]["data"].as_i64();

    // Object paths come sorted, so the first adapter is hci0.
    let Some((adapter_path, adapter)) = objects
        .iter()
        .find_map(|(path, interfaces)| Some((path, interfaces.get("org.bluez.Adapter1")?)))
    else {
        return Ok(Snapshot::default());
    };
    let mut snap = Snapshot {
        adapter: Some(Adapter {
            powered: flag(adapter, "Powered"),
            discovering: flag(adapter, "Discovering"),
        }),
        devices: Vec::new(),
    };
    for (path, interfaces) in objects {
        let Some(device) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        if device["Adapter"]["data"].as_str() != Some(adapter_path) {
            continue;
        }
        let manufacturers = device["ManufacturerData"]["data"]
            .as_object()
            .map(|data| data.keys().filter_map(|id| id.parse().ok()).collect())
            .unwrap_or_default();
        let uuids = device["UUIDs"]["data"]
            .as_array()
            .map(|uuids| {
                uuids
                    .iter()
                    .filter_map(|uuid| uuid.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        snap.devices.push(Device {
            path: path.clone(),
            address: text(device, "Address"),
            address_type: text(device, "AddressType"),
            name: device["Name"]["data"].as_str().map(str::to_owned),
            alias: text(device, "Alias"),
            icon: text(device, "Icon"),
            class: number(device, "Class").unwrap_or(0) as u32,
            paired: flag(device, "Paired"),
            trusted: flag(device, "Trusted"),
            blocked: flag(device, "Blocked"),
            connected: flag(device, "Connected"),
            rssi: number(device, "RSSI").map(|dbm| dbm as i16),
            tx_power: number(device, "TxPower").map(|dbm| dbm as i16),
            battery: number(&interfaces["org.bluez.Battery1"], "Percentage").map(|p| p as u8),
            uuids,
            modalias: text(device, "Modalias"),
            manufacturers,
        });
    }
    Ok(snap)
}

impl Device {
    /// What kind of device this is, from its icon or else its class of device.
    pub fn kind(&self) -> &'static str {
        match self.icon.as_str() {
            "audio-headset" => return "Headset",
            "audio-headphones" => return "Headphones",
            "audio-card" => return "Speaker",
            "input-keyboard" => return "Keyboard",
            "input-mouse" => return "Mouse",
            "input-gaming" => return "Game controller",
            "input-tablet" => return "Drawing tablet",
            "phone" => return "Phone",
            "computer" => return "Computer",
            "camera-video" | "camera-photo" => return "Camera",
            "printer" => return "Printer",
            "multimedia-player" => return "Media player",
            "video-display" => return "Display",
            "network-wireless" => return "Network",
            _ => {}
        }
        match (self.class >> 8) & 0x1f {
            1 => "Computer",
            2 => "Phone",
            3 => "Network",
            4 => "Audio or video",
            5 => "Input device",
            6 => "Printer or camera",
            7 => "Wearable",
            8 => "Toy",
            9 => "Health",
            _ => "Unknown",
        }
    }

    /// "054C:0DF0 (USB IDs)" from "usb:v054Cp0DF0d0251".
    pub fn device_id(&self) -> String {
        let Some((source, ids)) = self.modalias.split_once(':') else {
            return String::new();
        };
        let vendor = ids.strip_prefix('v').and_then(|rest| rest.get(..4));
        let product = ids.get(6..10).filter(|_| ids.get(5..6) == Some("p"));
        match (vendor, product) {
            (Some(vendor), Some(product)) => {
                format!("{vendor}:{product} ({} IDs)", source.to_uppercase())
            }
            _ => self.modalias.clone(),
        }
    }
}

/// The Bluetooth signal in percent for the list's bars: -100 dBm is nothing,
/// -40 dBm and stronger is full.
pub fn signal_percent(rssi: i16) -> u8 {
    ((i32::from(rssi) + 100) * 100 / 60).clamp(0, 100) as u8
}

/// A readable name for a service UUID; vendor services have none.
pub fn service_name(uuid: &str) -> String {
    let short = uuid
        .strip_prefix("0000")
        .and_then(|rest| rest.strip_suffix("-0000-1000-8000-00805f9b34fb"));
    let Some(short) = short else {
        return format!("Vendor service ({uuid})");
    };
    let name = match short {
        "1101" => "Serial port",
        "1105" => "Object push",
        "1106" => "File transfer",
        "1108" | "1131" => "Headset",
        "110a" => "Audio source",
        "110b" => "Audio sink",
        "110c" | "110e" | "110f" => "Remote control",
        "110d" => "Advanced audio",
        "1112" => "Headset gateway",
        "1115" | "1116" => "Personal area network",
        "111e" => "Hands-free",
        "111f" => "Hands-free gateway",
        "1124" => "Human interface device",
        "112f" => "Phone book",
        "1132" => "Messages",
        "1200" => "Device identification",
        "1800" => "Generic access",
        "1801" => "Generic attribute",
        "180a" => "Device information",
        "180f" => "Battery",
        "1812" => "Human interface device (LE)",
        "1843" | "1844" => "Audio volume",
        "184e" | "1850" | "184f" => "LE audio",
        "fe2c" => "Google Fast Pair",
        _ => return format!("Service 0x{short}"),
    };
    name.to_owned()
}

pub fn manufacturer_name(id: u16) -> String {
    let name = match id {
        0x0006 => "Microsoft",
        0x004c => "Apple",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x00e0 => "Google",
        0x012d => "Sony",
        0x0171 => "Amazon",
        0x02e5 => "Espressif",
        _ => return format!("0x{id:04X}"),
    };
    format!("{name} (0x{id:04X})")
}

/// The signal of a connected device, else what discovery last heard of it.
pub fn find_signal(address: &str, path: &str) -> Option<Signal> {
    if let Ok(out) = run("hcitool", ["rssi", address])
        && let Some(rssi) = connection_rssi(&out)
    {
        // A connection's RSSI is how far it is from the controller's ideal
        // range, 0 when inside it, not dBm.
        return Some(Signal {
            text: format!("{rssi} dB from the ideal range"),
            level: ((f32::from(rssi) + 20.0) / 25.0).clamp(0.0, 1.0),
        });
    }
    let json = run(
        "busctl",
        [
            "--json=short",
            "get-property",
            "org.bluez",
            path,
            "org.bluez.Device1",
            "RSSI",
        ],
    )
    .ok()?;
    let rssi = serde_json::from_str::<Value>(&json).ok()?["data"].as_i64()? as i16;
    Some(Signal {
        text: format!("{rssi} dBm"),
        level: f32::from(signal_percent(rssi)) / 100.0,
    })
}

/// "RSSI return value: -5" from `hcitool rssi`.
fn connection_rssi(out: &str) -> Option<i16> {
    out.trim()
        .strip_prefix("RSSI return value:")?
        .trim()
        .parse()
        .ok()
}

/// The device's own output, so the find sound comes from where it lies.
pub fn sink_of(address: &str) -> Option<String> {
    let underscored = address.replace(':', "_");
    let snap = pw::snapshot()
        .map_err(|err| failed("BT-13", "find the device's sink", err))
        .ok()?;
    let sink = snap
        .nodes
        .into_iter()
        .find(|n| n.class == "Audio/Sink" && n.name.contains(&underscored))?;
    log::debug!("find sound goes to {}", sink.name);
    Some(sink.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from `busctl --json=short call org.bluez / … GetManagedObjects`.
    const OBJECTS: &str = r#"{"type":"a{oa{sa{sv}}}","data":[{
        "/org/bluez":{"org.bluez.AgentManager1":{}},
        "/org/bluez/hci0":{"org.bluez.Adapter1":{"Address":{"type":"s","data":"14:AC:60:00:00:FE"},
            "Alias":{"type":"s","data":"Framework"},"Powered":{"type":"b","data":true},
            "PowerState":{"type":"s","data":"on"},"Discovering":{"type":"b","data":true}}},
        "/org/bluez/hci0/dev_88_C9_E8_0B_08_F0":{"org.bluez.Device1":{
            "Address":{"type":"s","data":"88:C9:E8:00:00:F0"},"AddressType":{"type":"s","data":"public"},
            "Name":{"type":"s","data":"WH-1000XM5"},"Alias":{"type":"s","data":"WH-1000XM5"},
            "Class":{"type":"u","data":2360324},"Icon":{"type":"s","data":"audio-headset"},
            "Paired":{"type":"b","data":true},"Trusted":{"type":"b","data":true},
            "Blocked":{"type":"b","data":false},"Connected":{"type":"b","data":true},
            "UUIDs":{"type":"as","data":["0000110b-0000-1000-8000-00805f9b34fb","2573f2dd-a2f8-486c-8476-a63734909acb"]},
            "Modalias":{"type":"s","data":"usb:v054Cp0DF0d0251"},
            "Adapter":{"type":"o","data":"/org/bluez/hci0"}},
          "org.bluez.Battery1":{"Percentage":{"type":"y","data":80}}},
        "/org/bluez/hci0/dev_5A_11_22_33_44_55":{"org.bluez.Device1":{
            "Address":{"type":"s","data":"5A:11:22:33:44:55"},"AddressType":{"type":"s","data":"random"},
            "Alias":{"type":"s","data":"5A-11-22-33-44-55"},"Paired":{"type":"b","data":false},
            "RSSI":{"type":"n","data":-67},"TxPower":{"type":"n","data":12},
            "ManufacturerData":{"type":"a{qv}","data":{"76":{"type":"ay","data":[16,5]}}},
            "Adapter":{"type":"o","data":"/org/bluez/hci0"}}}
    }]}"#;

    #[test]
    fn objects_give_the_adapter_and_its_devices() {
        let snap = parse_objects(OBJECTS).unwrap();
        let adapter = snap.adapter.unwrap();
        assert!(adapter.powered && adapter.discovering);
        assert_eq!(snap.devices.len(), 2);

        let headset = &snap.devices[1];
        assert_eq!(headset.alias, "WH-1000XM5");
        assert!(headset.paired && headset.trusted && headset.connected);
        assert_eq!(headset.battery, Some(80));
        assert_eq!(headset.path, "/org/bluez/hci0/dev_88_C9_E8_0B_08_F0");
        assert_eq!(headset.rssi, None);
        assert_eq!(headset.kind(), "Headset");
        assert_eq!(headset.device_id(), "054C:0DF0 (USB IDs)");

        let beacon = &snap.devices[0];
        assert_eq!(beacon.name, None);
        assert_eq!(beacon.rssi, Some(-67));
        assert_eq!(beacon.tx_power, Some(12));
        assert_eq!(beacon.manufacturers, [0x004c]);
        assert_eq!(manufacturer_name(beacon.manufacturers[0]), "Apple (0x004C)");
        assert_eq!(beacon.kind(), "Unknown");
    }

    #[test]
    fn a_connection_reads_its_rssi_from_hcitool() {
        assert_eq!(connection_rssi("RSSI return value: -5\n"), Some(-5));
        assert_eq!(connection_rssi("Not connected.\n"), None);
    }

    #[test]
    fn no_adapter_is_an_empty_snapshot() {
        let snap = parse_objects(r#"{"type":"a{oa{sa{sv}}}","data":[{"/org/bluez":{}}]}"#).unwrap();
        assert!(snap.adapter.is_none() && snap.devices.is_empty());
        assert!(parse_objects("not json").is_err());
    }

    #[test]
    fn the_passkey_is_read_from_coloured_agent_output() {
        let line = plain("\x1b[0;93m[agent]\x1b[0m Passkey: \x1b[1;39m024681\x1b[0m\r");
        assert_eq!(passkey(&line).as_deref(), Some("024681"));
        assert_eq!(passkey("[agent] PIN code: 0000").as_deref(), Some("0000"));
        assert_eq!(passkey("Attempting to pair with 88:C9:E8:00:00:F0"), None);
    }

    #[test]
    fn services_and_signal_read_plainly() {
        assert_eq!(
            service_name("0000110b-0000-1000-8000-00805f9b34fb"),
            "Audio sink"
        );
        assert_eq!(
            service_name("00001234-0000-1000-8000-00805f9b34fb"),
            "Service 0x1234"
        );
        assert!(service_name("2573f2dd-a2f8-486c-8476-a63734909acb").starts_with("Vendor"));
        assert_eq!(
            [
                signal_percent(-100),
                signal_percent(-70),
                signal_percent(-30)
            ],
            [0, 50, 100]
        );
    }
}
