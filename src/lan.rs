use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::nm::{failed, run};

/// The kernel asks for a MAC address three times, a second apart; most
/// answer within milliseconds, so this catches all but the sleepiest.
const ARP_WAIT: Duration = Duration::from_secs(2);
const KNOCKS_PER_SOCKET: u32 = 64;
const NEIGHBOUR_POLL: Duration = Duration::from_millis(200);
/// The widest net one scan knocks on: a /22, 1022 addresses.
const WIDEST_PREFIX: u8 = 22;

/// A device seen on the local network.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Host {
    pub ipv4: String,
    /// Global addresses first, then link-local.
    pub ipv6: Vec<String>,
    pub mac: String,
    pub vendor: String,
    /// From reverse DNS; empty when nobody knows it.
    pub hostname: String,
    /// Milliseconds of one ping, once it answered.
    pub ping: Option<f32>,
    pub router: bool,
    pub me: bool,
}

#[derive(Clone, Debug)]
pub struct Found {
    /// Like `192.168.178.0/24`, or empty without IPv4.
    pub subnet: String,
    pub hosts: Vec<Host>,
    /// Names and pings are in too.
    pub done: bool,
}

/// One scan on its own thread: the hosts arrive first, then again with their
/// names and pings. Dropping it lets the thread run out unheard.
pub struct Scan {
    replies: Receiver<Result<Found, String>>,
    pub found: Option<Result<Found, String>>,
}

impl Scan {
    pub fn start(ctx: egui::Context, device: String) -> Self {
        let (outbox, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("lan-scan".into())
            .spawn(move || {
                scan(&device, |found| {
                    let _ = outbox.send(found);
                    ctx.request_repaint();
                })
            })
            .expect("a scan thread");
        Self {
            replies,
            found: None,
        }
    }

    pub fn poll(&mut self) {
        if let Some(found) = self.replies.try_iter().last() {
            self.found = Some(found);
        }
    }
}

fn scan(device: &str, send: impl Fn(Result<Found, String>)) {
    log::info!("scanning the network on {device}");
    let started = Instant::now();
    let addr = match run("ip", ["-j", "addr", "show", "dev", device]) {
        Ok(addr) => addr,
        Err(err) => return send(Err(failed("LAN-1", "read the device's addresses", err))),
    };
    let mut me = parse_addr(&addr);
    me.me = true;
    me.hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let gateway = match run("ip", ["-j", "route", "show", "default", "dev", device]) {
        Ok(routes) => parse_gateway(&routes),
        Err(err) => {
            failed("LAN-4", "read the default route", err);
            String::new()
        }
    };

    let mut subnet = String::new();
    if let Some((ip, prefix)) = parse_ipv4(&addr) {
        let (network, prefix) = net(ip, prefix);
        subnet = format!("{network}/{prefix}");
        match knock(ip, network, prefix) {
            Ok(sent) => log::debug!("knocked on {sent} addresses in {subnet}"),
            Err(err) => {
                failed("LAN-3", "knock on the subnet", err.to_string());
            }
        }
    }
    // Every IPv6 node answers the all-nodes address, which fills in their
    // neighbour entries while ARP finishes for IPv4. This is iputils' ping;
    // GNU inetutils' refuses it and the list just has fewer IPv6 addresses.
    let all_nodes = format!("ff02::1%{device}");
    let all_nodes = spawn(
        "ping",
        &["-6", "-n", "-c", "2", "-i", "0.5", "-w", "2", &all_nodes],
    );

    // Hosts show as soon as the kernel knows their MAC, and their names and
    // pings as soon as each lookup answers, each on its own thread so a
    // silent device holds up nobody else.
    let vendors = vendor_list().unwrap_or_default();
    let (answers, inbox) = mpsc::channel();
    let mut hosts = vec![me];
    let mut looked_up = Vec::new();
    let mut pending = 0;
    let mut sent = None;
    let mut arrived = None;
    loop {
        let listening = started.elapsed() < ARP_WAIT;
        if listening || sent.is_none() {
            let neighbours = match run("ip", ["-j", "neigh", "show", "dev", device]) {
                Ok(neighbours) => neighbours,
                Err(err) => return send(Err(failed("LAN-2", "read the neighbour table", err))),
            };
            for fresh in parse_neigh(&neighbours, &gateway) {
                let host = match hosts.iter_mut().find(|host| host.mac == fresh.mac) {
                    Some(host) => {
                        host.ipv4 = fresh.ipv4;
                        host.ipv6 = fresh.ipv6;
                        host.router = fresh.router;
                        host
                    }
                    None => {
                        hosts.push(Host {
                            vendor: vendor(&vendors, &fresh.mac),
                            ..fresh
                        });
                        hosts.last_mut().unwrap()
                    }
                };
                if !host.ipv4.is_empty() && !looked_up.contains(&host.ipv4) {
                    looked_up.push(host.ipv4.clone());
                    pending += 2;
                    look_up(&answers, &host.mac, &host.ipv4);
                }
            }
        }
        for answer in arrived.take().into_iter().chain(inbox.try_iter()) {
            pending -= 1;
            if let Some(host) = hosts.iter_mut().find(|host| host.mac == answer.mac) {
                match answer.found {
                    Answer::Name(name) => host.hostname = name,
                    Answer::Ping(ms) => host.ping = ms,
                }
            }
        }
        sort(&mut hosts);
        let done = !listening && pending == 0;
        if sent.as_ref() != Some(&hosts) || done {
            send(Ok(Found {
                subnet: subnet.clone(),
                hosts: hosts.clone(),
                done,
            }));
            sent = Some(hosts.clone());
        }
        if done {
            break;
        }
        // Until ARP is through, the neighbour table is read again this often;
        // after that only the lookups are waited for.
        let wait = if listening {
            NEIGHBOUR_POLL
        } else {
            Duration::from_secs(10)
        };
        arrived = inbox.recv_timeout(wait).ok();
    }
    finish(all_nodes);
    log::info!(
        "{} devices on {device}, named and pinged after {:?}",
        hosts.len(),
        started.elapsed()
    );
}

struct Answered {
    mac: String,
    found: Answer,
}

enum Answer {
    Name(String),
    Ping(Option<f32>),
}

/// Asks for the host's name and pings it, each answer sent when it comes.
fn look_up(answers: &mpsc::Sender<Answered>, mac: &str, ip: &str) {
    let (name, mac_too, ip_too) = (answers.clone(), mac.to_owned(), ip.to_owned());
    std::thread::spawn(move || {
        let found = Answer::Name(parse_getent(&finish(spawn("getent", &["hosts", &ip_too]))));
        let _ = name.send(Answered {
            mac: mac_too,
            found,
        });
    });
    let (ping, mac, ip) = (answers.clone(), mac.to_owned(), ip.to_owned());
    std::thread::spawn(move || {
        // -W is the timeout in iputils and the linger in inetutils: one or
        // two seconds either way.
        let found = Answer::Ping(parse_ping(&finish(spawn(
            "ping",
            &["-n", "-c", "1", "-W", "1", &ip],
        ))));
        let _ = ping.send(Answered { mac, found });
    });
}

/// The network a /22 at most around `ip`, so a big net isn't flooded.
fn net(ip: Ipv4Addr, prefix: u8) -> (Ipv4Addr, u8) {
    let prefix = prefix.clamp(WIDEST_PREFIX, 32);
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    (Ipv4Addr::from(u32::from(ip) & mask), prefix)
}

/// One UDP packet to every address of the net. Nobody has to listen on the
/// discard port: the packet only makes the kernel ask for each MAC with ARP,
/// and the answers land in the neighbour table. Needs no root, unlike ARP itself.
fn knock(me: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> std::io::Result<usize> {
    let size = 1u32 << (32 - u32::from(prefix));
    let mut sent = 0;
    let mut socket = None;
    for offset in 1..size.saturating_sub(1) {
        // A packet waiting for ARP holds on to its socket's send buffer until
        // the MAC is known or given up on after 3 s, so one socket blocks
        // after a few hundred addresses; a fresh one per block never does.
        if offset % KNOCKS_PER_SOCKET == 1 {
            let fresh = UdpSocket::bind((me, 0))?;
            fresh.set_nonblocking(true)?;
            socket = Some(fresh);
        }
        let ip = Ipv4Addr::from(u32::from(network) + offset);
        if let Some(socket) = &socket
            && ip != me
            && socket.send_to(&[0], (ip, 9)).is_ok()
        {
            sent += 1;
        }
    }
    Ok(sent)
}

/// A lookup that runs while the others do, with its command line for the log.
struct Lookup {
    shown: String,
    started: Instant,
    child: Child,
}

fn spawn(program: &str, args: &[&str]) -> Option<Lookup> {
    let shown = format!("{program} {}", args.join(" "));
    let child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(child) => Some(Lookup {
            shown,
            started: Instant::now(),
            child,
        }),
        Err(err) => {
            log::warn!("could not run {shown}: {err}");
            None
        }
    }
}

/// The child's output, or nothing when it failed; for a lookup, failing just
/// means no answer.
fn finish(lookup: Option<Lookup>) -> String {
    let Some(Lookup {
        shown,
        started,
        child,
    }) = lookup
    else {
        return String::new();
    };
    match child.wait_with_output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            log::debug!(
                "{shown} -> {} in {:?}: {}",
                output.status,
                started.elapsed(),
                stdout.lines().find(|l| !l.is_empty()).unwrap_or_default()
            );
            if output.status.success() {
                stdout
            } else {
                String::new()
            }
        }
        Err(err) => {
            log::warn!("{shown} failed: {err}");
            String::new()
        }
    }
}

fn json(text: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(text).unwrap_or_else(|err| {
        log::warn!("unreadable ip output: {err}");
        Vec::new()
    })
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// `ip -j addr show dev <device>`: our own MAC and addresses, as a host.
fn parse_addr(addr: &str) -> Host {
    let mut me = Host::default();
    for link in json(addr) {
        me.mac = text(&link, "address");
        for info in link["addr_info"].as_array().into_iter().flatten() {
            let local = text(info, "local");
            match info["family"].as_str() {
                Some("inet") if me.ipv4.is_empty() => me.ipv4 = local,
                Some("inet6") => me.ipv6.push(local),
                _ => {}
            }
        }
    }
    me
}

fn parse_ipv4(addr: &str) -> Option<(Ipv4Addr, u8)> {
    json(addr).iter().find_map(|link| {
        let info = link["addr_info"]
            .as_array()?
            .iter()
            .find(|info| info["family"] == "inet")?;
        let ip = info["local"].as_str()?.parse().ok()?;
        Some((ip, u8::try_from(info["prefixlen"].as_u64()?).ok()?))
    })
}

fn parse_gateway(routes: &str) -> String {
    json(routes)
        .iter()
        .map(|route| text(route, "gateway"))
        .find(|gateway| !gateway.is_empty())
        .unwrap_or_default()
}

/// `ip -j neigh show dev <device>`: one entry per address, joined by MAC.
/// Entries that never got an answer have no MAC or a failed state.
fn parse_neigh(neighbours: &str, gateway: &str) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    for entry in json(neighbours) {
        let mac = text(&entry, "lladdr").to_lowercase();
        let failed = entry["state"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|state| state == "FAILED" || state == "INCOMPLETE");
        if mac.is_empty() || failed {
            continue;
        }
        let ip = text(&entry, "dst");
        let index = match hosts.iter().position(|host| host.mac == mac) {
            Some(index) => index,
            None => {
                hosts.push(Host {
                    mac,
                    ..Host::default()
                });
                hosts.len() - 1
            }
        };
        let host = &mut hosts[index];
        host.router |= entry.get("router").is_some() || ip == gateway;
        if ip.contains(':') {
            host.ipv6.push(ip);
        } else if host.ipv4.is_empty() {
            host.ipv4 = ip;
        }
    }
    hosts
}

/// Router first, then this computer, then by IPv4 address.
fn sort(hosts: &mut [Host]) {
    for host in hosts.iter_mut() {
        host.ipv6.sort_by_key(|ip| ip.starts_with("fe80:"));
    }
    hosts.sort_by_key(|host| {
        let ipv4 = host.ipv4.parse::<Ipv4Addr>().map_or(u32::MAX, u32::from);
        (!host.router, !host.me, ipv4, host.mac.clone())
    });
}

/// `getent hosts <ip>`: `<ip>  <name> [aliases]`. systemd-resolved calls the
/// router `_gateway`, which the router tag already says.
fn parse_getent(text: &str) -> String {
    text.split_whitespace()
        .nth(1)
        .filter(|name| *name != "_gateway")
        .unwrap_or_default()
        .to_owned()
}

/// `… icmp_seq=1 ttl=64 time=3.21 ms`
fn parse_ping(text: &str) -> Option<f32> {
    let (_, rest) = text.split_once("time=")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// The manufacturer list nmap ships, found beside the nmap on PATH.
fn vendor_list() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let nmap = std::env::split_paths(&path)
        .map(|dir| dir.join("nmap"))
        .find(|nmap| nmap.exists())?;
    let list = nmap
        .canonicalize()
        .ok()?
        .parent()?
        .parent()?
        .join("share/nmap/nmap-mac-prefixes");
    match std::fs::read_to_string(&list) {
        Ok(list) => Some(list),
        Err(err) => {
            log::debug!("no manufacturer list at {}: {err}", list.display());
            None
        }
    }
}

/// `list` has a `<hex prefix> <name>` line per block; the longest prefix
/// that matches names the manufacturer.
fn vendor(list: &str, mac: &str) -> String {
    let hex: String = mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_uppercase();
    // The locally administered bit: phones pick a random MAC per network.
    let local = u8::from_str_radix(hex.get(..2).unwrap_or("00"), 16).unwrap_or(0) & 2 != 0;
    if local {
        return "Private address".to_owned();
    }
    list.lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(prefix, _)| hex.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map_or(String::new(), |(_, name)| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_nets_shrink_to_a_slash_22_around_us() {
        let ip = Ipv4Addr::new(10, 1, 7, 9);
        assert_eq!(net(ip, 8), (Ipv4Addr::new(10, 1, 4, 0), 22));
        assert_eq!(net(ip, 24), (Ipv4Addr::new(10, 1, 7, 0), 24));
        assert_eq!(net(ip, 32), (ip, 32));
    }

    #[test]
    fn neighbours_join_by_mac_and_skip_the_silent() {
        let neigh = r#"[
            {"dst":"192.168.178.1","lladdr":"2c:91:ab:00:00:01","state":["REACHABLE"]},
            {"dst":"fe80::2e91:abff:fe00:1","lladdr":"2C:91:AB:00:00:01","router":null,"state":["STALE"]},
            {"dst":"2001:db8:0:1::1","lladdr":"2c:91:ab:00:00:01","router":null,"state":["STALE"]},
            {"dst":"192.168.178.20","lladdr":"de:ad:be:ef:00:01","state":["DELAY"]},
            {"dst":"192.168.178.21","state":["FAILED"]},
            {"dst":"192.168.178.22","lladdr":"00:11:22:33:44:55","state":["FAILED"]}
        ]"#;
        let mut hosts = parse_neigh(neigh, "192.168.178.1");
        hosts.push(Host {
            ipv4: "192.168.178.35".into(),
            me: true,
            ..Host::default()
        });
        sort(&mut hosts);
        assert_eq!(hosts.len(), 3);
        assert!(hosts[0].router);
        assert_eq!(hosts[0].ipv4, "192.168.178.1");
        assert_eq!(
            hosts[0].ipv6,
            ["2001:db8:0:1::1", "fe80::2e91:abff:fe00:1"]
        );
        assert!(hosts[1].me);
        assert_eq!(hosts[2].mac, "de:ad:be:ef:00:01");
        assert!(!hosts[2].router);
    }

    #[test]
    fn our_own_addresses_come_from_ip_addr() {
        let addr = r#"[{"ifname":"wlan0","address":"14:ac:60:00:00:fd","addr_info":[
            {"family":"inet","local":"192.168.178.35","prefixlen":24},
            {"family":"inet6","local":"fe80::2f3:4d08:56b8:fd21","prefixlen":64}]}]"#;
        let me = parse_addr(addr);
        assert_eq!(me.mac, "14:ac:60:00:00:fd");
        assert_eq!(me.ipv4, "192.168.178.35");
        assert_eq!(me.ipv6, ["fe80::2f3:4d08:56b8:fd21"]);
        assert_eq!(
            parse_ipv4(addr),
            Some((Ipv4Addr::new(192, 168, 178, 35), 24))
        );
        let routes = r#"[{"dst":"default","gateway":"192.168.178.1","metric":600}]"#;
        assert_eq!(parse_gateway(routes), "192.168.178.1");
    }

    #[test]
    fn names_pings_and_manufacturers_are_read() {
        assert_eq!(
            parse_getent("192.168.178.5   nas.fritz.box\n"),
            "nas.fritz.box"
        );
        assert_eq!(parse_getent("192.168.178.1   _gateway\n"), "");
        assert_eq!(parse_getent(""), "");
        let ping = "64 bytes from 192.168.178.1: icmp_seq=1 ttl=64 time=3.21 ms\n";
        assert_eq!(parse_ping(ping), Some(3.21));
        assert_eq!(parse_ping(""), None);
        let list = "0050C2 Ieee Registration Authority\n0050C2123 Tiny Maker\n2C91AB AVM GmbH\n";
        assert_eq!(vendor(list, "2c:91:ab:00:00:01"), "AVM GmbH");
        assert_eq!(vendor(list, "00:50:c2:12:34:56"), "Tiny Maker");
        assert_eq!(
            vendor(list, "00:50:c2:99:34:56"),
            "Ieee Registration Authority"
        );
        assert_eq!(vendor(list, "de:ad:be:ef:00:01"), "Private address");
        assert_eq!(vendor(list, "00:00:00:00:00:01"), "");
    }

    #[test]
    fn knocking_reaches_every_address_but_ours() {
        let me = Ipv4Addr::LOCALHOST;
        let (network, prefix) = net(me, 30);
        assert_eq!(knock(me, network, prefix).unwrap(), 1);
    }
}
