use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::finder::Signal;

/// How often the finder asks for a scan while looking for a network it isn't
/// connected to.
const FIND_RESCAN: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Wifi,
    Ethernet,
    Other(String),
}

impl Kind {
    fn parse(raw: &str) -> Self {
        match raw {
            "802-11-wireless" => Self::Wifi,
            "802-3-ethernet" => Self::Ethernet,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Wifi => "Wi-Fi",
            Self::Ethernet => "Ethernet",
            Self::Other(raw) => match raw.as_str() {
                "vpn" => "VPN",
                "wireguard" => "WireGuard",
                "bridge" => "Bridge",
                "bond" => "Bond",
                "vlan" => "VLAN",
                "gsm" => "Mobile Broadband",
                "loopback" => "Loopback",
                other => other,
            },
        }
    }

    fn nmcli_type(&self) -> &str {
        match self {
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
            Self::Other(raw) => raw,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Connection {
    pub name: String,
    pub uuid: String,
    pub kind: Kind,
    pub device: String,
    pub active: bool,
    pub ssid: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub name: String,
    pub kind: String,
    pub state: String,
    /// The first IPv4 address without its prefix length, or empty.
    pub ip: String,
}

#[derive(Clone, Debug)]
pub struct AccessPoint {
    pub in_use: bool,
    pub ssid: String,
    pub signal: u8,
    pub security: String,
    pub device: String,
}

/// One access point from the kernel's scan results. NetworkManager on iwd
/// reports made-up channels for networks it isn't connected to, so the
/// channel radar asks the kernel through `iw` instead.
#[derive(Clone, Debug)]
pub struct Beacon {
    pub ssid: String,
    pub bssid: String,
    /// MHz
    pub frequency: u32,
    /// dBm
    pub signal: f32,
    pub associated: bool,
}

/// The IEEE channel number of a frequency in MHz.
pub fn channel(frequency: u32) -> u32 {
    match frequency {
        2484 => 14,
        ..=2483 => frequency.saturating_sub(2407) / 5,
        5925.. => (frequency - 5950) / 5,
        _ => frequency.saturating_sub(5000) / 5,
    }
}

/// A device connected to a hotspot running on one of our Wi-Fi interfaces.
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub device: String,
    pub mac: String,
    /// dBm of the last frame heard from it.
    pub signal: f32,
    pub inactive_ms: u64,
    pub connected_secs: u64,
    /// From the hotspot's DHCP leases; empty when it has none.
    pub ip: String,
    pub hostname: String,
}

impl Station {
    pub fn name(&self) -> &str {
        if self.hostname.is_empty() {
            &self.mac
        } else {
            &self.hostname
        }
    }

    pub fn percent(&self) -> u8 {
        (wifi_level(self.signal) * 100.0).round() as u8
    }
}

impl AccessPoint {
    pub fn secured(&self) -> bool {
        !self.security.is_empty() && self.security != "--"
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub connections: Vec<Connection>,
    pub devices: Vec<Device>,
    pub access_points: Vec<AccessPoint>,
    pub stations: Vec<Station>,
    pub wifi_enabled: bool,
    /// Wi-Fi devices were told to scan before the networks were listed.
    pub rescanned: bool,
}

#[derive(Clone, Debug, Default)]
pub struct IpSettings {
    pub method: String,
    pub addresses: String,
    pub gateway: String,
    pub dns: String,
    pub search: String,
    pub never_default: bool,
    pub ignore_auto_routes: bool,
    pub ignore_auto_dns: bool,
    pub required: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub uuid: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub device: String,
    pub ssid: String,
    pub mode: String,
    pub key_mgmt: String,
    pub password: String,
    pub password_changed: bool,
    pub bssid: String,
    pub cloned_mac: String,
    pub mtu: String,
    pub ipv4: IpSettings,
    pub ipv6: IpSettings,
    pub autoconnect: bool,
    pub all_users: bool,
}

impl Settings {
    pub fn new(kind: Kind, name: String) -> Self {
        let ip = |method: &str| IpSettings {
            method: method.to_owned(),
            ..IpSettings::default()
        };
        Self {
            uuid: None,
            kind,
            name,
            device: String::new(),
            ssid: String::new(),
            mode: "infrastructure".to_owned(),
            key_mgmt: String::new(),
            password: String::new(),
            password_changed: false,
            bssid: String::new(),
            cloned_mac: String::new(),
            mtu: String::new(),
            ipv4: ip("auto"),
            ipv6: ip("auto"),
            autoconnect: true,
            all_users: true,
        }
    }

    fn from_properties(props: &HashMap<String, String>) -> Self {
        let get = |key: &str| props.get(key).cloned().unwrap_or_default();
        let yes = |key: &str| props.get(key).is_some_and(|value| value == "yes");
        let kind = Kind::parse(&get("connection.type"));
        let link = match kind {
            Kind::Ethernet => "802-3-ethernet",
            _ => "802-11-wireless",
        };
        let ip = |family: &str| IpSettings {
            method: get(&format!("{family}.method")),
            addresses: get(&format!("{family}.addresses")),
            gateway: get(&format!("{family}.gateway")),
            dns: get(&format!("{family}.dns")),
            search: get(&format!("{family}.dns-search")),
            never_default: yes(&format!("{family}.never-default")),
            ignore_auto_routes: yes(&format!("{family}.ignore-auto-routes")),
            ignore_auto_dns: yes(&format!("{family}.ignore-auto-dns")),
            required: !yes(&format!("{family}.may-fail")),
        };
        let mtu = get(&format!("{link}.mtu"));
        let password = get("802-11-wireless-security.psk");
        Self {
            uuid: Some(get("connection.uuid")),
            name: get("connection.id"),
            device: get("connection.interface-name"),
            ssid: get("802-11-wireless.ssid"),
            mode: get("802-11-wireless.mode"),
            key_mgmt: get("802-11-wireless-security.key-mgmt"),
            password: if password == "<hidden>" {
                String::new()
            } else {
                password
            },
            password_changed: false,
            bssid: get("802-11-wireless.bssid"),
            cloned_mac: get(&format!("{link}.cloned-mac-address")),
            mtu: if mtu == "auto" || mtu == "0" {
                String::new()
            } else {
                mtu
            },
            ipv4: ip("ipv4"),
            ipv6: ip("ipv6"),
            autoconnect: yes("connection.autoconnect"),
            all_users: get("connection.permissions").is_empty(),
            kind,
        }
    }

    fn properties(&self) -> Vec<(String, String)> {
        let flag = |on: bool| if on { "yes" } else { "no" }.to_owned();
        let mut out = vec![
            ("connection.id".to_owned(), self.name.clone()),
            ("connection.interface-name".to_owned(), self.device.clone()),
            ("connection.autoconnect".to_owned(), flag(self.autoconnect)),
            (
                "connection.permissions".to_owned(),
                if self.all_users {
                    String::new()
                } else {
                    format!("user:{}", std::env::var("USER").unwrap_or_default())
                },
            ),
        ];
        let mtu = if self.mtu.trim().is_empty() {
            "0".to_owned()
        } else {
            self.mtu.trim().to_owned()
        };
        match self.kind {
            Kind::Wifi => {
                out.push(("802-11-wireless.ssid".into(), self.ssid.clone()));
                out.push(("802-11-wireless.mode".into(), self.mode.clone()));
                out.push(("802-11-wireless.bssid".into(), self.bssid.clone()));
                out.push((
                    "802-11-wireless.cloned-mac-address".into(),
                    self.cloned_mac.clone(),
                ));
                out.push(("802-11-wireless.mtu".into(), mtu));
                if !self.key_mgmt.is_empty() {
                    out.push((
                        "802-11-wireless-security.key-mgmt".into(),
                        self.key_mgmt.clone(),
                    ));
                    if self.password_changed && matches!(self.key_mgmt.as_str(), "wpa-psk" | "sae")
                    {
                        out.push(("802-11-wireless-security.psk".into(), self.password.clone()));
                    }
                }
            }
            Kind::Ethernet => {
                out.push((
                    "802-3-ethernet.cloned-mac-address".into(),
                    self.cloned_mac.clone(),
                ));
                out.push(("802-3-ethernet.mtu".into(), mtu));
            }
            Kind::Other(_) => {}
        }
        for (family, ip) in [("ipv4", &self.ipv4), ("ipv6", &self.ipv6)] {
            let key = |name: &str| format!("{family}.{name}");
            out.push((key("method"), ip.method.clone()));
            out.push((key("addresses"), ip.addresses.clone()));
            out.push((key("gateway"), ip.gateway.clone()));
            out.push((key("dns"), ip.dns.clone()));
            out.push((key("dns-search"), ip.search.clone()));
            out.push((key("never-default"), flag(ip.never_default)));
            out.push((key("ignore-auto-routes"), flag(ip.ignore_auto_routes)));
            out.push((key("ignore-auto-dns"), flag(ip.ignore_auto_dns)));
            out.push((key("may-fail"), flag(!ip.required)));
        }
        out
    }
}

#[derive(Clone, Debug)]
pub enum Op {
    Export,
    Save,
    Delete,
    Activate,
    Deactivate,
    Wifi,
}

pub enum Request {
    Refresh {
        rescan: bool,
    },
    Load(String),
    /// Writes the NixOS module for every profile that needs no secret.
    Export(Vec<String>),
    Save(Box<Settings>),
    Delete(String),
    Up(String),
    Down(String),
    Connect {
        ssid: String,
        password: Option<String>,
        device: String,
    },
    SetWifi(bool),
    /// The kernel's scan results for these Wi-Fi devices.
    Beacons(Vec<String>),
    /// What a connected device is linked to: access point, addresses, DNS.
    Details {
        device: String,
        wifi: bool,
    },
}

impl Request {
    /// Changes are counted as in flight and followed by a fresh snapshot.
    fn changes(&self) -> bool {
        !matches!(
            self,
            Self::Refresh { .. } | Self::Load(_) | Self::Beacons(_) | Self::Details { .. }
        )
    }
}

pub enum Reply {
    Snapshot(Result<Snapshot, String>),
    Loaded(Result<Box<Settings>, String>),
    Done(Op, Result<(), String>),
    /// `iw_error` says why the beacons came from nmcli instead of iw.
    Beacons {
        beacons: Result<Vec<Beacon>, String>,
        iw_error: Option<String>,
    },
    /// Label and value rows, ready to show.
    Details {
        device: String,
        rows: Result<Vec<(&'static str, String)>, String>,
    },
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
            .name("nmcli".into())
            .spawn(move || {
                for request in inbox {
                    // A rescan takes seconds; the networks already known show meanwhile.
                    if matches!(request, Request::Refresh { rescan: true })
                        && outbox
                            .send(Reply::Snapshot(snapshot(false).map_err(|err| {
                                failed("NM-115", "refresh before the rescan", err)
                            })))
                            .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                    let refresh = request.changes();
                    let reply = handle(request);
                    if outbox.send(reply).is_err() {
                        break;
                    }
                    if refresh
                        && outbox
                            .send(Reply::Snapshot(
                                snapshot(false)
                                    .map_err(|err| failed("NM-110", "refresh after change", err)),
                            ))
                            .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
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

fn handle(request: Request) -> Reply {
    match request {
        Request::Refresh { rescan } => {
            Reply::Snapshot(snapshot(rescan).map_err(|err| failed("NM-101", "refresh", err)))
        }
        Request::Load(uuid) => Reply::Loaded(
            load(&uuid)
                .map(Box::new)
                .map_err(|err| failed("NM-102", "load connection", err)),
        ),
        Request::Export(uuids) => {
            let mut profiles = Vec::new();
            for uuid in &uuids {
                match load(uuid) {
                    Ok(settings) => profiles.push(settings),
                    Err(err) => {
                        log::warn!("{}", failed("NM-114", "load connection for NixOS", err))
                    }
                }
            }
            Reply::Done(
                Op::Export,
                crate::nix::write("network.nix", &crate::nix::network(&profiles)),
            )
        }
        Request::Save(settings) => Reply::Done(
            Op::Save,
            save(&settings).map_err(|err| failed("NM-103", "save connection", err)),
        ),
        Request::Delete(uuid) => {
            let result = nmcli(["connection", "delete", "uuid", &uuid]);
            Reply::Done(
                Op::Delete,
                result
                    .map(drop)
                    .map_err(|err| failed("NM-104", "delete connection", err)),
            )
        }
        Request::Up(uuid) => {
            let result = nmcli(["--wait", "60", "connection", "up", "uuid", &uuid]);
            Reply::Done(
                Op::Activate,
                result
                    .map(drop)
                    .map_err(|err| failed("NM-105", "activate connection", err)),
            )
        }
        Request::Down(uuid) => {
            let result = nmcli(["connection", "down", "uuid", &uuid]);
            Reply::Done(
                Op::Deactivate,
                result
                    .map(drop)
                    .map_err(|err| failed("NM-106", "deactivate connection", err)),
            )
        }
        Request::Connect {
            ssid,
            password,
            device,
        } => {
            let mut args = vec![
                "--wait", "60", "device", "wifi", "connect", &ssid, "ifname", &device,
            ];
            if let Some(password) = &password {
                args.extend(["password", password]);
            }
            let result = nmcli(args);
            Reply::Done(
                Op::Activate,
                result
                    .map(drop)
                    .map_err(|err| failed("NM-107", "connect to network", err)),
            )
        }
        Request::SetWifi(on) => {
            let result = nmcli(["radio", "wifi", if on { "on" } else { "off" }]);
            Reply::Done(
                Op::Wifi,
                result
                    .map(drop)
                    .map_err(|err| failed("NM-109", "switch Wi-Fi", err)),
            )
        }
        Request::Beacons(devices) => {
            let mut beacons = Vec::new();
            for device in &devices {
                match run("iw", ["dev", device, "scan", "dump"]) {
                    Ok(dump) => beacons.extend(parse_scan_dump(&dump)),
                    Err(err) => {
                        let iw_error = Some(failed("IW-1", "read scan results", err));
                        log::info!("falling back to nmcli for the channels");
                        return Reply::Beacons {
                            beacons: nmcli_beacons()
                                .map_err(|err| failed("NM-111", "list channels", err)),
                            iw_error,
                        };
                    }
                }
            }
            Reply::Beacons {
                beacons: Ok(beacons),
                iw_error: None,
            }
        }
        Request::Details { device, wifi } => Reply::Details {
            rows: details(&device, wifi)
                .map_err(|err| failed("NM-112", "read connection details", err)),
            device,
        },
    }
}

pub fn failed(code: &str, what: &str, err: String) -> String {
    log::error!("[{code}] {what} failed: {err}");
    format!("[{code}] {err}")
}

fn nmcli<I, S>(args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run("nmcli", args)
}

pub fn run<I, S>(program: &str, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let shown = redact(&args);
    let started = std::time::Instant::now();
    let output = Command::new(program)
        .args(&args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => format!("{program} is not installed"),
            _ => format!("Could not run {program}: {err}"),
        })?;
    log::debug!(
        "{program} {shown} -> {} in {:?}",
        output.status,
        started.elapsed()
    );
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // bluetoothctl reports its errors on stdout.
        let last_line = stdout.lines().rfind(|line| !line.trim().is_empty());
        let message = match stderr.trim() {
            "" => last_line.unwrap_or_default().trim(),
            stderr => stderr,
        }
        .trim_start_matches("Error: ");
        log::warn!("{program} {shown} failed: {message}");
        Err(if message.is_empty() {
            format!("{program} failed ({})", output.status)
        } else {
            message.to_owned()
        })
    }
}

fn redact(args: &[std::ffi::OsString]) -> String {
    let mut shown = Vec::new();
    let mut secret_next = false;
    for arg in args {
        let arg = arg.to_string_lossy();
        shown.push(if secret_next {
            "<secret>".into()
        } else {
            arg.to_string()
        });
        secret_next = arg == "password" || arg.ends_with(".psk");
    }
    shown.join(" ")
}

fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => fields.last_mut().unwrap().extend(chars.next()),
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// `setting.property:value` lines, which `nmcli` does not escape.
fn properties(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| line.split_once(':'))
}

fn snapshot(rescan: bool) -> Result<Snapshot, String> {
    let mut connections: Vec<Connection> = nmcli([
        "-t",
        "-f",
        "NAME,UUID,TYPE,DEVICE,ACTIVE",
        "connection",
        "show",
    ])?
    .lines()
    .map(split_terse)
    .filter(|fields| fields.len() >= 5)
    .map(|fields| Connection {
        name: fields[0].clone(),
        uuid: fields[1].clone(),
        kind: Kind::parse(&fields[2]),
        device: fields[3].clone(),
        active: fields[4] == "yes",
        ssid: String::new(),
    })
    .collect();

    let wifi: Vec<&str> = connections
        .iter()
        .filter(|c| c.kind == Kind::Wifi)
        .map(|c| c.uuid.as_str())
        .collect();
    if !wifi.is_empty() {
        let mut args = vec![
            "-t",
            "-f",
            "connection.uuid,802-11-wireless.ssid",
            "connection",
            "show",
        ];
        args.extend(wifi);
        let mut ssids = HashMap::new();
        let mut uuid = String::new();
        for (key, value) in properties(&nmcli(args)?) {
            match key {
                "connection.uuid" => uuid = value.to_owned(),
                "802-11-wireless.ssid" => {
                    ssids.insert(uuid.clone(), value.to_owned());
                }
                _ => {}
            }
        }
        for connection in &mut connections {
            if let Some(ssid) = ssids.remove(&connection.uuid) {
                connection.ssid = ssid;
            }
        }
    }

    let ips = first_ips(&nmcli([
        "-t",
        "-f",
        "GENERAL.DEVICE,IP4.ADDRESS",
        "device",
        "show",
    ])?);
    let devices = nmcli(["-t", "-f", "DEVICE,TYPE,STATE", "device"])?
        .lines()
        .map(split_terse)
        .filter(|fields| fields.len() >= 3)
        .map(|fields| Device {
            name: fields[0].clone(),
            kind: fields[1].clone(),
            state: fields[2].clone(),
            ip: ips.get(&fields[0]).cloned().unwrap_or_default(),
        })
        .collect();

    let rescanned = rescan;
    let rescan = if rescan { "yes" } else { "auto" };
    // No Wi-Fi device, or the radio is off: no networks rather than an error.
    let access_points = nmcli([
        "-t",
        "-f",
        "IN-USE,SSID,SIGNAL,SECURITY,DEVICE",
        "device",
        "wifi",
        "list",
        "--rescan",
        rescan,
    ])
    .unwrap_or_default()
    .lines()
    .map(split_terse)
    .filter(|fields| fields.len() >= 5)
    .map(|fields| AccessPoint {
        in_use: fields[0] == "*",
        ssid: fields[1].clone(),
        signal: fields[2].parse().unwrap_or(0),
        security: fields[3].clone(),
        device: fields[4].clone(),
    })
    .collect();

    let wifi_enabled = nmcli(["-t", "-f", "WIFI", "radio"])?.trim() == "enabled";

    Ok(Snapshot {
        connections,
        devices,
        access_points,
        stations: hotspot_stations(),
        wifi_enabled,
        rescanned,
    })
}

/// `iw dev <device> scan dump`: a `BSS` line per access point, then its
/// properties one tab in; deeper lines belong to nested elements.
fn parse_scan_dump(dump: &str) -> Vec<Beacon> {
    let mut beacons: Vec<Beacon> = Vec::new();
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("BSS ") {
            beacons.push(Beacon {
                ssid: String::new(),
                bssid: rest.split('(').next().unwrap_or_default().trim().to_owned(),
                frequency: 0,
                signal: -100.0,
                associated: rest.contains("-- associated"),
            });
            continue;
        }
        let (Some(beacon), Some(property)) = (beacons.last_mut(), line.strip_prefix('\t')) else {
            continue;
        };
        let Some((key, value)) = property.split_once(": ") else {
            continue;
        };
        match key {
            "freq" if beacon.frequency == 0 => {
                beacon.frequency = value.parse::<f32>().unwrap_or(0.0) as u32;
            }
            "signal" => {
                let dbm = value.trim_end_matches(" dBm");
                beacon.signal = dbm.parse().unwrap_or(-100.0);
            }
            // A hidden network sends its SSID as zero bytes.
            "SSID" if !value.starts_with("\\x00") => beacon.ssid = value.to_owned(),
            _ => {}
        }
    }
    beacons
}

/// The backup when iw is missing. Right with wpa_supplicant, but NetworkManager
/// on iwd reports channel 2 for every network it isn't connected to.
fn nmcli_beacons() -> Result<Vec<Beacon>, String> {
    let list = nmcli([
        "-t",
        "-f",
        "IN-USE,BSSID,SSID,FREQ,SIGNAL",
        "device",
        "wifi",
        "list",
        "--rescan",
        "no",
    ])?;
    Ok(list
        .lines()
        .map(split_terse)
        .filter(|fields| fields.len() >= 5)
        .map(|fields| Beacon {
            associated: fields[0] == "*",
            bssid: fields[1].clone(),
            ssid: fields[2].clone(),
            frequency: fields[3]
                .split_whitespace()
                .next()
                .and_then(|mhz| mhz.parse().ok())
                .unwrap_or(0),
            // NetworkManager maps -90..-20 dBm linearly onto 0..100 %.
            signal: -90.0 + fields[4].parse::<f32>().unwrap_or(0.0) * 0.7,
        })
        .collect())
}

fn details(device: &str, wifi: bool) -> Result<Vec<(&'static str, String)>, String> {
    let mut rows = Vec::new();
    if wifi {
        match run("iw", ["dev", device, "link"]) {
            Ok(link) => rows.extend(parse_link(&link)),
            Err(err) => {
                log::warn!("[IW-2] read Wi-Fi link failed: {err}; falling back to nmcli");
                let list = nmcli([
                    "-t",
                    "-f",
                    "IN-USE,BSSID,CHAN,FREQ,SIGNAL,RATE",
                    "device",
                    "wifi",
                    "list",
                    "ifname",
                    device,
                    "--rescan",
                    "no",
                ])?;
                rows.extend(parse_in_use(&list));
            }
        }
    }
    let show = nmcli([
        "-t",
        "-f",
        "GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS,IP4.DOMAIN,IP6.ADDRESS,IP6.GATEWAY,IP6.DNS",
        "device",
        "show",
        device,
    ])?;
    rows.extend(parse_device_show(&show));
    rows.push((
        "Hostname",
        nmcli(["general", "hostname"])?.trim().to_owned(),
    ));
    rows.retain(|(_, value)| !value.is_empty());
    Ok(rows)
}

/// The scans the finder asked NetworkManager for.
#[derive(Default)]
pub struct Rescans {
    running: Option<Child>,
    at: Option<Instant>,
}

/// The connected access point's signal when it sends `ssid`, which is fresh
/// with every frame; else the strongest of its access points in the last
/// scan, with a new scan every few seconds.
pub fn find_signal(
    device: &str,
    ssid: &str,
    rescans: &mut Rescans,
) -> Result<Option<Signal>, String> {
    let link = run("iw", ["dev", device, "link"])
        .map_err(|err| failed("IW-3", "read the Wi-Fi link for the finder", err))?;
    if let Some(dbm) = link_signal(&link, ssid) {
        return Ok(Some(Signal {
            text: format!("{dbm:.0} dBm"),
            level: wifi_level(dbm),
        }));
    }
    let scanning = rescans
        .running
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
    if !scanning && rescans.at.is_none_or(|at| at.elapsed() >= FIND_RESCAN) {
        rescans.at = Some(Instant::now());
        log::debug!("$ nmcli device wifi rescan ifname {device}");
        rescans.running = Command::new("nmcli")
            .args(["device", "wifi", "rescan", "ifname", device])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| failed("NM-113", "rescan for the finder", err.to_string()))
            .ok();
    }
    let dump = run("iw", ["dev", device, "scan", "dump"])
        .map_err(|err| failed("IW-4", "read the scan results for the finder", err))?;
    let strongest = parse_scan_dump(&dump)
        .into_iter()
        .filter(|beacon| beacon.ssid == ssid)
        .map(|beacon| beacon.signal)
        .reduce(f32::max);
    Ok(strongest.map(|dbm| Signal {
        text: format!("{dbm:.0} dBm (last scan)"),
        level: wifi_level(dbm),
    }))
}

/// -90 dBm is hardly there, -30 dBm is next to the access point.
fn wifi_level(dbm: f32) -> f32 {
    ((dbm + 90.0) / 60.0).clamp(0.0, 1.0)
}

/// The signal in `iw dev <device> link` when it is connected to `ssid`.
fn link_signal(link: &str, ssid: &str) -> Option<f32> {
    let mut connected_to = None;
    let mut signal = None;
    for (key, value) in link.lines().filter_map(|line| line.trim().split_once(": ")) {
        match key {
            "SSID" => connected_to = Some(value),
            "signal" => signal = value.trim_end_matches(" dBm").parse().ok(),
            _ => {}
        }
    }
    signal.filter(|_| connected_to == Some(ssid))
}

/// The devices on each hotspot we run. Logged only: without iw there is
/// just no list of them.
fn hotspot_stations() -> Vec<Station> {
    let interfaces = match run("iw", ["dev"]) {
        Ok(interfaces) => interfaces,
        Err(err) => {
            failed("IW-5", "list the Wi-Fi interfaces", err);
            return Vec::new();
        }
    };
    let mut stations = Vec::new();
    for device in access_point_interfaces(&interfaces) {
        let dump = match run("iw", ["dev", &device, "station", "dump"]) {
            Ok(dump) => dump,
            Err(err) => {
                failed("IW-6", "list the hotspot's devices", err);
                continue;
            }
        };
        // NetworkManager's dnsmasq hands out the hotspot's addresses.
        let leases = format!("/var/lib/NetworkManager/dnsmasq-{device}.leases");
        let leases = std::fs::read_to_string(&leases).unwrap_or_else(|err| {
            log::debug!("no leases in {leases}: {err}");
            String::new()
        });
        stations.extend(parse_station_dump(&device, &dump, &leases));
    }
    stations
}

/// The interfaces in `iw dev` that run as an access point.
fn access_point_interfaces(iw_dev: &str) -> Vec<String> {
    let mut interface = None;
    let mut found = Vec::new();
    for line in iw_dev.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("Interface ") {
            interface = Some(name.to_owned());
        } else if line == "type AP"
            && let Some(name) = interface.take()
        {
            found.push(name);
        }
    }
    found
}

/// `iw dev <device> station dump`: a `Station <mac> (on <device>)` line per
/// device, then its properties one tab in. `leases` is dnsmasq's leases file:
/// `<expiry> <mac> <ip> <hostname> <client id>` per line, `*` for no hostname.
fn parse_station_dump(device: &str, dump: &str, leases: &str) -> Vec<Station> {
    let mut stations: Vec<Station> = Vec::new();
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("Station ") {
            let mac = rest.split_whitespace().next().unwrap_or_default();
            let lease: Vec<&str> = leases
                .lines()
                .map(|line| line.split_whitespace().collect::<Vec<_>>())
                .find(|fields| fields.len() >= 4 && fields[1].eq_ignore_ascii_case(mac))
                .unwrap_or_default();
            stations.push(Station {
                device: device.to_owned(),
                mac: mac.to_owned(),
                signal: -100.0,
                inactive_ms: 0,
                connected_secs: 0,
                ip: lease.get(2).map_or(String::new(), |ip| (*ip).to_owned()),
                hostname: lease
                    .get(3)
                    .filter(|name| **name != "*")
                    .map_or(String::new(), |name| (*name).to_owned()),
            });
            continue;
        }
        let Some(station) = stations.last_mut() else {
            continue;
        };
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let number = value.split_whitespace().next().unwrap_or_default();
        match key {
            "signal" => station.signal = number.parse().unwrap_or(-100.0),
            "inactive time" => station.inactive_ms = number.parse().unwrap_or(0),
            "connected time" => station.connected_secs = number.parse().unwrap_or(0),
            _ => {}
        }
    }
    stations
}

/// The signal of a device on our hotspot, from the last frame it sent.
pub fn station_signal(device: &str, mac: &str) -> Result<Option<Signal>, String> {
    let dump = run("iw", ["dev", device, "station", "dump"])
        .map_err(|err| failed("IW-7", "read the hotspot device's signal", err))?;
    let station = parse_station_dump(device, &dump, "")
        .into_iter()
        .find(|station| station.mac.eq_ignore_ascii_case(mac));
    Ok(station.map(|station| {
        let mut text = format!("{:.0} dBm", station.signal);
        if station.inactive_ms >= 2000 {
            text += &format!(", heard {} s ago", station.inactive_ms / 1000);
        }
        Signal {
            text,
            level: wifi_level(station.signal),
        }
    }))
}

/// `iw dev <device> link`: the access point, then its properties one tab in.
fn parse_link(link: &str) -> Vec<(&'static str, String)> {
    let Some(first) = link.lines().next() else {
        return Vec::new();
    };
    let Some(rest) = first.strip_prefix("Connected to ") else {
        return Vec::new();
    };
    let bssid = rest.split_whitespace().next().unwrap_or_default();
    let mut frequency = 0;
    let (mut signal, mut rx, mut tx) = ("", "", "");
    for (key, value) in link.lines().filter_map(|line| line.trim().split_once(": ")) {
        match key {
            "freq" => frequency = value.parse::<f32>().unwrap_or(0.0) as u32,
            "signal" => signal = value,
            "rx bitrate" => rx = value,
            "tx bitrate" => tx = value,
            _ => {}
        }
    }
    // "650.0 MBit/s VHT-MCS 7 80MHz short GI": the speed, then how it is sent.
    let speed = |bitrate: &str| {
        bitrate
            .split(" MBit/s")
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let width = tx
        .split_whitespace()
        .find_map(|word| word.strip_suffix("MHz"))
        .map_or(String::new(), |mhz| format!(", {mhz} MHz wide"));
    let channel_text = format!("{}  ({frequency} MHz{width})", channel(frequency));
    vec![
        ("Access point", bssid.to_uppercase()),
        ("Channel", channel_text),
        ("Signal", signal.to_owned()),
        (
            "Link speed",
            format!("↓ {} Mbit/s  ↑ {} Mbit/s", speed(rx), speed(tx)),
        ),
    ]
}

/// The access point a device is connected to, from nmcli's Wi-Fi list.
fn parse_in_use(list: &str) -> Vec<(&'static str, String)> {
    let Some(fields) = list
        .lines()
        .map(split_terse)
        .find(|fields| fields.len() >= 6 && fields[0] == "*")
    else {
        return Vec::new();
    };
    vec![
        ("Access point", fields[1].clone()),
        ("Channel", format!("{}  ({})", fields[2], fields[3])),
        ("Signal", format!("{} %", fields[4])),
        ("Link speed", fields[5].clone()),
    ]
}

fn parse_device_show(show: &str) -> Vec<(&'static str, String)> {
    // Lists come as `IP4.DNS[1]`, `IP4.DNS[2]`…; one row shows them all.
    let values = |names: &[&str]| {
        properties(show)
            .filter(|(key, value)| {
                names.contains(&key.split('[').next().unwrap_or_default())
                    && !value.is_empty()
                    && *value != "--"
            })
            .map(|(_, value)| value)
            .collect::<Vec<_>>()
            .join("\n")
    };
    vec![
        ("MAC address", values(&["GENERAL.HWADDR"])),
        ("IPv4 address", values(&["IP4.ADDRESS"])),
        ("Gateway", values(&["IP4.GATEWAY"])),
        ("DNS servers", values(&["IP4.DNS", "IP6.DNS"])),
        ("Domain", values(&["IP4.DOMAIN"])),
        ("IPv6 address", values(&["IP6.ADDRESS"])),
        ("IPv6 gateway", values(&["IP6.GATEWAY"])),
    ]
}

/// Device name → its first IPv4 address, from `device show` property lines.
fn first_ips(text: &str) -> HashMap<String, String> {
    let mut ips = HashMap::new();
    let mut device = String::new();
    for (key, value) in properties(text) {
        if key == "GENERAL.DEVICE" {
            device = value.to_owned();
        } else if key.starts_with("IP4.ADDRESS") {
            let ip = value.split('/').next().unwrap_or_default();
            ips.entry(device.clone()).or_insert_with(|| ip.to_owned());
        }
    }
    ips
}

fn load(uuid: &str) -> Result<Settings, String> {
    let text = nmcli([
        "-t",
        "--show-secrets",
        "-f",
        "connection,802-11-wireless,802-11-wireless-security,802-3-ethernet,ipv4,ipv6",
        "connection",
        "show",
        "uuid",
        uuid,
    ])?;
    let props = properties(&text)
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    Ok(Settings::from_properties(&props))
}

fn save(settings: &Settings) -> Result<(), String> {
    let props = settings.properties();
    let pairs = props
        .iter()
        .flat_map(|(key, value)| [key.as_str(), value.as_str()]);
    match &settings.uuid {
        Some(uuid) => {
            let mut args = vec!["connection", "modify", "uuid", uuid];
            args.extend(pairs);
            nmcli(args)?;
            if settings.kind == Kind::Wifi && settings.key_mgmt.is_empty() {
                // An open network: drop the security setting if it had one.
                let _ = nmcli([
                    "connection",
                    "modify",
                    "uuid",
                    uuid,
                    "remove",
                    "802-11-wireless-security",
                ]);
            }
        }
        None => {
            let mut args = vec!["connection", "add", "type", settings.kind.nmcli_type()];
            args.extend(pairs);
            nmcli(args)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_never_reach_the_log() {
        let args: Vec<std::ffi::OsString> = [
            "device",
            "wifi",
            "connect",
            "Home",
            "password",
            "hunter22",
            "802-11-wireless-security.psk",
            "x",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let shown = redact(&args);
        assert!(
            !shown.contains("hunter22") && !shown.ends_with(" x"),
            "{shown}"
        );
    }

    #[test]
    fn scan_dump_lists_every_access_point() {
        let dump = "BSS 5c:49:79:54:30:d2(on wlan0) -- associated\n\
                    \tlast seen: 9885.686s [boottime]\n\
                    \tfreq: 5300.0\n\
                    \tsignal: -69.00 dBm\n\
                    \tSSID: 50:34:30:5F:35\n\
                    \tHT operation:\n\
                    \t\t * primary channel: 60\n\
                    BSS 78:20:51:8b:38:07(on wlan0)\n\
                    \tfreq: 2417\n\
                    \tsignal: -85.00 dBm\n\
                    \tSSID: \\x00\\x00\\x00\n";
        let beacons = parse_scan_dump(dump);
        assert_eq!(beacons.len(), 2);
        assert_eq!(beacons[0].bssid, "5c:49:79:54:30:d2");
        assert_eq!(beacons[0].ssid, "50:34:30:5F:35");
        assert_eq!(beacons[0].frequency, 5300);
        assert_eq!(beacons[0].signal, -69.0);
        assert!(beacons[0].associated);
        assert_eq!(beacons[1].frequency, 2417);
        assert_eq!(beacons[1].ssid, "");
        assert!(!beacons[1].associated);
    }

    #[test]
    fn first_ipv4_address_per_device() {
        let ips = first_ips(
            "GENERAL.DEVICE:wlan0\nIP4.ADDRESS[1]:192.168.188.108/24\nIP4.ADDRESS[2]:10.0.0.5/8\n\n\
             GENERAL.DEVICE:eth0\n\nGENERAL.DEVICE:lo\nIP4.ADDRESS[1]:127.0.0.1/8\n",
        );
        assert_eq!(ips["wlan0"], "192.168.188.108");
        assert_eq!(ips["lo"], "127.0.0.1");
        assert!(!ips.contains_key("eth0"));
    }

    #[test]
    fn wifi_link_shows_channel_width_and_speed() {
        let rows = parse_link(
            "Connected to 5c:49:79:54:30:d2 (on wlan0)\n\tSSID: Home\n\tfreq: 5300.0\n\
             \tsignal: -59 dBm\n\trx bitrate: 263.3 MBit/s VHT-MCS 6 80MHz VHT-NSS 1\n\
             \ttx bitrate: 650.0 MBit/s VHT-MCS 7 80MHz short GI VHT-NSS 2\n",
        );
        assert_eq!(
            rows,
            [
                ("Access point", "5C:49:79:54:30:D2".to_owned()),
                ("Channel", "60  (5300 MHz, 80 MHz wide)".to_owned()),
                ("Signal", "-59 dBm".to_owned()),
                ("Link speed", "↓ 263.3 Mbit/s  ↑ 650.0 Mbit/s".to_owned()),
            ]
        );
        assert!(parse_link("Not connected.\n").is_empty());
    }

    #[test]
    fn the_finder_reads_the_link_only_for_its_network() {
        let link = "Connected to 5c:49:79:54:30:d2 (on wlan0)\n\tSSID: Home\n\tsignal: -59 dBm\n";
        assert_eq!(link_signal(link, "Home"), Some(-59.0));
        assert_eq!(link_signal(link, "Neighbour"), None);
        assert_eq!(link_signal("Not connected.\n", "Home"), None);
        assert!(wifi_level(-90.0) == 0.0 && wifi_level(-60.0) == 0.5 && wifi_level(-20.0) == 1.0);
    }

    #[test]
    fn hotspot_devices_come_with_their_lease() {
        let iw_dev = "phy#0\n\tInterface p2p-dev-wlan0\n\t\ttype P2P-device\n\
                      \tInterface wlan0\n\t\tifindex 3\n\t\tssid laptop\n\t\ttype AP\n\
                      phy#1\n\tInterface wlan1\n\t\ttype managed\n";
        assert_eq!(access_point_interfaces(iw_dev), ["wlan0"]);

        let dump = "Station 24:0a:c4:12:34:56 (on wlan0)\n\
                    \tinactive time:\t3400 ms\n\trx bytes:\t1234\n\
                    \tsignal:  \t-61 [-61] dBm\n\tsignal avg:\t-63 [-63] dBm\n\
                    \tconnected time:\t95 seconds\n\
                    Station aa:bb:cc:dd:ee:ff (on wlan0)\n\tsignal:  \t-40 [-40] dBm\n";
        let leases = "1761600000 24:0a:c4:12:34:56 10.42.0.17 esp32-123456 01:24:0a:c4:12:34:56\n\
                      1761600000 aa:bb:cc:dd:ee:ff 10.42.0.18 * *\n";
        let stations = parse_station_dump("wlan0", dump, leases);
        assert_eq!(stations.len(), 2);
        let esp = &stations[0];
        assert_eq!(esp.mac, "24:0a:c4:12:34:56");
        assert_eq!(esp.signal, -61.0);
        assert_eq!(esp.inactive_ms, 3400);
        assert_eq!(esp.connected_secs, 95);
        assert_eq!(esp.ip, "10.42.0.17");
        assert_eq!(esp.name(), "esp32-123456");
        assert_eq!(stations[1].name(), "aa:bb:cc:dd:ee:ff");
        assert_eq!(stations[1].ip, "10.42.0.18");
        assert!(stations[1].percent() > esp.percent());
    }

    #[test]
    fn device_details_join_lists_and_skip_blanks() {
        let rows = parse_device_show(
            "GENERAL.HWADDR:14:AC:60:00:00:FD\nIP4.ADDRESS[1]:192.168.188.108/24\n\
             IP4.GATEWAY:192.168.188.1\nIP4.DNS[1]:192.168.188.1\nIP6.GATEWAY:--\n\
             IP6.DNS[1]:fd0b::1\nIP6.DNS[2]:2001:db8::1\n",
        );
        let row = |label: &str| rows.iter().find(|(l, _)| *l == label).unwrap().1.clone();
        assert_eq!(row("MAC address"), "14:AC:60:00:00:FD");
        assert_eq!(row("DNS servers"), "192.168.188.1\nfd0b::1\n2001:db8::1");
        assert_eq!(row("IPv6 gateway"), "");
    }

    #[test]
    fn terse_fields_unescape_colons_and_backslashes() {
        assert_eq!(
            split_terse(r"*:My\:Net:98\:9B\:CB:62:WPA2:wlan0"),
            ["*", "My:Net", "98:9B:CB", "62", "WPA2", "wlan0"]
        );
        assert_eq!(split_terse(r"a\\b:"), [r"a\b", ""]);
    }

    #[test]
    fn settings_survive_a_round_trip_through_properties() {
        let text = "connection.id:Home\nconnection.uuid:u\nconnection.type:802-11-wireless\n\
                    connection.autoconnect:yes\nconnection.permissions:\n802-11-wireless.ssid:a:b\n\
                    802-11-wireless.mode:infrastructure\n802-11-wireless.mtu:auto\n\
                    802-11-wireless-security.key-mgmt:wpa-psk\n802-11-wireless-security.psk:secret\n\
                    ipv4.method:manual\nipv4.addresses:10.0.0.2/24\nipv4.may-fail:no\nipv6.method:auto\nipv6.may-fail:yes\n";
        let props = properties(text)
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let settings = Settings::from_properties(&props);
        assert_eq!(settings.kind, Kind::Wifi);
        assert_eq!(settings.ssid, "a:b");
        assert_eq!(settings.password, "secret");
        assert_eq!(settings.mtu, "");
        assert!(settings.ipv4.required && !settings.ipv6.required);
        assert!(settings.autoconnect && settings.all_users);

        let out: HashMap<_, _> = settings.properties().into_iter().collect();
        assert_eq!(out["802-11-wireless.ssid"], "a:b");
        assert_eq!(out["802-11-wireless.mtu"], "0");
        assert_eq!(out["ipv4.addresses"], "10.0.0.2/24");
        assert!(!out.contains_key("802-11-wireless-security.psk"));
    }
}
