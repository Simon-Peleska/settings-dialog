//! The processes on this computer, read from /proc, for the Tasks page.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::nm::{failed, run};

/// The unit of the CPU times in /proc, USER_HZ, which is 100 on every
/// architecture Linux runs userspace on.
const TICKS: f64 = 100.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    /// The kernel's name for it, at most 15 characters.
    pub name: String,
    pub command: String,
    pub user: String,
    pub state: String,
    pub threads: u32,
    /// Percent of one core, so a process busy on four cores reads 400.
    pub cpu: f32,
    /// Seconds spent on a CPU since it started.
    pub cpu_time: f64,
    /// Resident memory in bytes.
    pub memory: u64,
    /// Seconds since it started.
    pub age: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Busiest first, then the largest. Kernel threads are left out.
    pub processes: Vec<Process>,
    /// Percent of the whole machine.
    pub cpu: f32,
    /// Percent of each core.
    pub cores: Vec<f32>,
    pub memory_used: u64,
    pub memory_total: u64,
    pub gpus: Vec<Gpu>,
}

/// The memory of one graphics card, in bytes, from amdgpu's sysfs files. An
/// integrated GPU has little memory of its own and takes most of what it uses
/// from the system memory it shares (GTT); a card without it reads 0 there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gpu {
    pub card: String,
    pub vram_used: u64,
    pub vram_total: u64,
    pub shared_used: u64,
    pub shared_total: u64,
}

pub enum Request {
    Refresh,
    End { pid: u32, name: String },
    Kill { pid: u32, name: String },
}

pub enum Reply {
    Snapshot(Result<Snapshot, String>),
    Done(Result<(), String>),
}

/// What the last read saw, so the next one can tell how busy everything was
/// in between.
#[derive(Default)]
struct Sample {
    /// All ticks and idle ticks of the whole machine, then of each core.
    cpus: Vec<(u64, u64)>,
    ticks: HashMap<u32, u64>,
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
            .name("tasks".into())
            .spawn(move || {
                let users = users();
                let mut last = Sample::default();
                while let Ok(request) = inbox.recv() {
                    let reply = match request {
                        Request::Refresh => Reply::Snapshot(snapshot(&mut last, &users)),
                        Request::End { pid, name } => {
                            Reply::Done(signal(pid, &name, "TERM", "PS-2", "end a process"))
                        }
                        Request::Kill { pid, name } => {
                            Reply::Done(signal(pid, &name, "KILL", "PS-3", "kill a process"))
                        }
                    };
                    let _ = outbox.send(reply);
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

fn signal(pid: u32, name: &str, signal: &str, code: &str, what: &str) -> Result<(), String> {
    log::info!("sending {signal} to '{name}' ({pid})");
    run("kill", ["-s", signal, &pid.to_string()])
        .map(|_| ())
        .map_err(|err| failed(code, what, err))
}

fn snapshot(last: &mut Sample, users: &HashMap<u32, String>) -> Result<Snapshot, String> {
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .map_err(|err| failed("PS-1", "read the processes", format!("{path}: {err}")))
    };
    let cpus = cpu_ticks(&read("/proc/stat")?);
    let (total, cores) = (
        cpus.first().map_or(0, |cpu| cpu.0),
        cpus.len().saturating_sub(1),
    );
    let uptime: f64 = read("/proc/uptime")?
        .split_whitespace()
        .next()
        .and_then(|up| up.parse().ok())
        .unwrap_or(0.0);
    let (memory_total, memory_available) = memory(&read("/proc/meminfo")?);

    // Ticks of one core that passed since the last read.
    let before = last.cpus.first().map_or(0, |cpu| cpu.0);
    let elapsed = total.saturating_sub(before) as f32 / cores.max(1) as f32;
    let mut ticks = HashMap::new();
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc")
        .map_err(|err| failed("PS-1", "read the processes", err.to_string()))?
        .filter_map(Result::ok)
    {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        // A process that ends while we read it is simply not listed.
        let (Ok(stat), Ok(status)) = (
            std::fs::read_to_string(dir.join("stat")),
            std::fs::read_to_string(dir.join("status")),
        ) else {
            continue;
        };
        let Some(mut process) = parse(pid, &stat, &status, uptime, users) else {
            continue;
        };
        let used = (process.cpu_time * TICKS) as u64;
        if let Some(before) = last.ticks.get(&pid)
            && elapsed > 0.0
        {
            process.cpu = used.saturating_sub(*before) as f32 / elapsed * 100.0;
        }
        ticks.insert(pid, used);
        let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
        process.command = String::from_utf8_lossy(&cmdline)
            .split('\0')
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        process.name = name(&process.name, &process.command);
        processes.push(process);
    }
    processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));

    let mut busy: Vec<f32> = cpus
        .iter()
        .enumerate()
        .map(|(index, now)| busy_percent(*now, last.cpus.get(index).copied()))
        .collect();
    let cpu = if busy.is_empty() { 0.0 } else { busy.remove(0) };
    *last = Sample { cpus, ticks };
    log::debug!("{} processes, CPU {cpu:.0} %", processes.len());
    Ok(Snapshot {
        processes,
        cpu,
        cores: busy,
        memory_used: memory_total.saturating_sub(memory_available),
        memory_total,
        gpus: gpus(),
    })
}

/// All ticks and idle ticks (idle and iowait) of the whole machine, then of
/// each core, from /proc/stat.
fn cpu_ticks(stat: &str) -> Vec<(u64, u64)> {
    stat.lines()
        .take_while(|line| line.starts_with("cpu"))
        .map(|line| {
            let fields: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|n| n.parse().ok())
                .collect();
            let idle = fields.get(3).unwrap_or(&0) + fields.get(4).unwrap_or(&0);
            (fields.iter().sum(), idle)
        })
        .collect()
}

/// How busy a CPU was between two reads of its ticks; 0 without an earlier one.
fn busy_percent(now: (u64, u64), before: Option<(u64, u64)>) -> f32 {
    let Some(before) = before else {
        return 0.0;
    };
    let all = now.0.saturating_sub(before.0);
    let idle = now.1.saturating_sub(before.1).min(all);
    match all {
        0 => 0.0,
        all => (all - idle) as f32 / all as f32 * 100.0,
    }
}

/// Every card that amdgpu reports memory for. Other drivers don't put it in
/// sysfs, so their cards are left out.
fn gpus() -> Vec<Gpu> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut found: Vec<Gpu> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let card = entry.file_name().to_string_lossy().into_owned();
            // "card1-DP-1" and the like are its connectors.
            if !card.starts_with("card") || card.contains('-') {
                return None;
            }
            let device = entry.path().join("device");
            let number = |file: &str| -> Option<u64> {
                std::fs::read_to_string(device.join(file))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            Some(Gpu {
                vram_used: number("mem_info_vram_used")?,
                vram_total: number("mem_info_vram_total")?,
                shared_used: number("mem_info_gtt_used").unwrap_or(0),
                shared_total: number("mem_info_gtt_total").unwrap_or(0),
                card,
            })
        })
        .collect();
    found.sort_by(|a, b| a.card.cmp(&b.card));
    found
}

/// Total and available memory in bytes, from /proc/meminfo.
fn memory(meminfo: &str) -> (u64, u64) {
    let field = |name: &str| {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0)
            * 1024
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

/// One process from its /proc/<pid>/stat and status. None for a kernel thread,
/// which has no memory of its own, and for anything that doesn't parse.
fn parse(
    pid: u32,
    stat: &str,
    status: &str,
    uptime: f64,
    users: &HashMap<u32, String>,
) -> Option<Process> {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    let memory_kb: u64 = field("VmRSS:")?
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    let uid: u32 = field("Uid:")?.split_whitespace().next()?.parse().ok()?;
    // The name sits in parentheses and may hold spaces and parentheses itself.
    let name = &stat[stat.find('(')? + 1..stat.rfind(')')?];
    let rest: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    let number = |index: usize| rest.get(index)?.parse::<u64>().ok();
    let state = match *rest.first()? {
        "R" => "Running",
        "S" => "Sleeping",
        "D" => "Waiting for the disk",
        "Z" => "Zombie (ended, not yet collected)",
        "T" => "Stopped",
        "t" => "Stopped by a debugger",
        "I" => "Idle",
        other => other,
    };
    Some(Process {
        pid,
        parent: number(1)? as u32,
        name: name.to_owned(),
        command: String::new(),
        user: users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
        state: state.to_owned(),
        threads: number(17)? as u32,
        cpu: 0.0,
        cpu_time: (number(11)? + number(12)?) as f64 / TICKS,
        memory: memory_kb * 1024,
        age: (uptime - number(19)? as f64 / TICKS).max(0.0),
    })
}

/// The kernel cuts names to 15 characters, so the program's own file name is
/// used when the two agree. NixOS wraps a program as ".firefox-wrapped" (in
/// the kernel's name, and in the file name unless the wrapper sets its own),
/// which reads as "firefox".
fn name(kernel: &str, command: &str) -> String {
    let program = command.split(' ').next().unwrap_or_default();
    let file = unwrapped(program.rsplit('/').next().unwrap_or_default());
    let kernel = unwrapped(kernel);
    if !file.is_empty() && (file.starts_with(kernel) || kernel.starts_with(file)) {
        return file.to_owned();
    }
    kernel.to_owned()
}

/// ".firefox-wrapped" and ".firefox-wra", cut short by the kernel, are
/// "firefox"; a name without the leading dot is left alone.
fn unwrapped(name: &str) -> &str {
    let Some(bare) = name.strip_prefix('.') else {
        return name;
    };
    match bare.rfind('-') {
        Some(dash)
            if "-wrapped".starts_with(&bare[dash..]) || "-unwrapped".starts_with(&bare[dash..]) =>
        {
            &bare[..dash]
        }
        _ => bare,
    }
}

fn users() -> HashMap<u32, String> {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_else(|err| {
        log::warn!("/etc/passwd: {err}; users show as numbers");
        String::new()
    });
    passwd
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.nth(1)?.parse().ok()?;
            Some((uid, name.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4242 (Web Content (x)) S 1 4242 4242 0 -1 4194560 100 0 0 0 \
                        250 50 0 0 20 0 7 0 1000 0 0 18446744073709551615";
    const STATUS: &str = "Name:\tWeb Content\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  20480 kB\n";

    #[test]
    fn a_process_comes_out_of_its_stat_and_status() {
        let users = HashMap::from([(1000, "simon".to_owned())]);
        let process = parse(4242, STAT, STATUS, 60.0, &users).unwrap();
        assert_eq!(
            process,
            Process {
                pid: 4242,
                parent: 1,
                name: "Web Content (x)".to_owned(),
                command: String::new(),
                user: "simon".to_owned(),
                state: "Sleeping".to_owned(),
                threads: 7,
                cpu: 0.0,
                cpu_time: 3.0,
                memory: 20480 * 1024,
                age: 50.0,
            }
        );
    }

    #[test]
    fn names_come_whole_from_the_command_and_without_nix_wrappers() {
        let claude = "/nix/store/abc-claude/bin/.claude-unwrapped --resume";
        assert_eq!(name(".claude-unwrapp", claude), "claude");
        assert_eq!(
            name(".spotify-wrappe", "/run/x/.spotify-wrapped"),
            "spotify"
        );
        assert_eq!(
            name("systemd-journal", "/lib/systemd/systemd-journald"),
            "systemd-journald"
        );
        assert_eq!(
            name(
                ".Hyprland-wrapp",
                "/nix/store/x/bin/Hyprland --watchdog-fd 4"
            ),
            "Hyprland"
        );
        assert_eq!(name(".ghostty-wrappe", "ghostty"), "ghostty");
        assert_eq!(name(".quickshell-wra", ""), "quickshell");
        assert_eq!(name("my-app", "/bin/my-app"), "my-app");
        assert_eq!(name("MainThread", "/usr/bin/python3 app.py"), "MainThread");
        assert_eq!(name("zombie", ""), "zombie");
    }

    #[test]
    fn kernel_threads_are_left_out() {
        let status = "Name:\tkworker/0:1\nUid:\t0\t0\t0\t0\n";
        assert!(parse(7, STAT, status, 60.0, &HashMap::new()).is_none());
    }

    #[test]
    fn cpu_ticks_count_idle_for_the_machine_and_each_core() {
        let stat = "cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 5 0 5 35 5 0 0 0 0 0\n\
                    cpu1 5 0 5 35 5 0 0 0 0 0\nintr 1 2 3\n";
        assert_eq!(cpu_ticks(stat), [(100, 80), (50, 40), (50, 40)]);
    }

    #[test]
    fn a_core_is_as_busy_as_the_ticks_it_did_not_idle() {
        assert_eq!(busy_percent((200, 130), Some((100, 80))), 50.0);
        assert_eq!(busy_percent((200, 130), None), 0.0);
        assert_eq!(busy_percent((100, 80), Some((100, 80))), 0.0);
    }

    #[test]
    fn the_real_proc_lists_this_test_with_its_cpu_after_a_second_read() {
        let mut last = Sample::default();
        let users = users();
        snapshot(&mut last, &users).unwrap();
        let spin = std::time::Instant::now();
        while spin.elapsed() < std::time::Duration::from_millis(300) {
            std::hint::black_box(0);
        }
        let snap = snapshot(&mut last, &users).unwrap();
        let me = snap
            .processes
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("this test is listed");
        assert!(me.memory > 0);
        assert!(me.cpu > 10.0, "spun for 300 ms but reads {} %", me.cpu);
        assert!(!snap.cores.is_empty() && snap.memory_total > snap.memory_used);
        assert!(snap.cores.iter().all(|core| (0.0..=100.0).contains(core)));
        assert!(snap.gpus.iter().all(|gpu| gpu.vram_used <= gpu.vram_total));
    }

    #[test]
    fn end_stops_a_process_and_a_missing_one_is_an_error() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        signal(child.id(), "sleep", "TERM", "PS-2", "end a process").unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        let err = signal(child.id(), "sleep", "TERM", "PS-2", "end a process").unwrap_err();
        assert!(err.starts_with("[PS-2]"), "{err}");
    }
}
