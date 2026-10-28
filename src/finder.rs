use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::bt;
use crate::nm::{self, failed};

const RATE: usize = 48_000;
/// One beep and the pause after it.
const BEEP: Duration = Duration::from_millis(120);
const BEEP_GAP: Duration = Duration::from_millis(180);

/// What to find.
pub enum Source {
    Bluetooth {
        address: String,
        path: String,
        connected: bool,
    },
    Wifi {
        device: String,
        ssid: String,
    },
    /// A device on our hotspot; `ip` is pinged so it keeps sending frames.
    Station {
        device: String,
        mac: String,
        ip: String,
    },
}

/// How strong a signal is right now.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub text: String,
    /// 0 is hardly heard, 1 is right here.
    pub level: f32,
}

/// Beeps over and over, higher the stronger the signal, to walk towards what
/// sends it. A connected Bluetooth device with a sink plays it itself, so the
/// sound comes from where it lies; everything else plays on the default
/// output. Dropping it stops the sound.
pub struct Finder {
    /// What the screen started it for, such as a Bluetooth address.
    pub key: String,
    /// None until the first reading; then the newest, None when nothing is heard.
    pub signal: Option<Option<Signal>>,
    pub error: Option<String>,
    stop: Arc<AtomicBool>,
    readings: Receiver<Result<Option<Signal>, String>>,
}

impl Finder {
    pub fn start(ctx: egui::Context, key: String, source: Source) -> Self {
        let (sender, readings) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        log::info!("finding {key}");
        std::thread::Builder::new()
            .name("finder".into())
            .spawn(move || {
                let target = match &source {
                    Source::Bluetooth {
                        address,
                        connected: true,
                        ..
                    } => bt::sink_of(address),
                    _ => None,
                };
                let mut args = vec![
                    "--playback".to_owned(),
                    "--raw".to_owned(),
                    "--format".to_owned(),
                    "f32".to_owned(),
                    "--rate".to_owned(),
                    RATE.to_string(),
                    "--channels".to_owned(),
                    "1".to_owned(),
                    // No restore, so WirePlumber saves no state entry for it.
                    "-P".to_owned(),
                    "{ node.name = settings-dialog-finder application.name = settings-dialog \
                     media.name = \"Find by sound\" state.restore-props = false \
                     state.restore-target = false }"
                        .to_owned(),
                ];
                if let Some(target) = target {
                    args.extend(["--target".to_owned(), target]);
                }
                args.push("-".to_owned());
                log::debug!("$ pw-cat {}", args.join(" "));
                let child = Command::new("pw-cat")
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn();
                let mut child = match child {
                    Ok(child) => child,
                    Err(err) => {
                        let err = failed("BT-11", "start the find sound", err.to_string());
                        let _ = sender.send(Err(err));
                        ctx.request_repaint();
                        return;
                    }
                };
                let mut stdin = child.stdin.take().expect("piped stdin");
                let mut rescans = nm::Rescans::default();
                let mut ping: Option<Child> = None;
                // Small writes, so stopping kills pw-cat at once instead of
                // waiting for a full pipe to drain.
                let chunk = RATE / 50;
                'beeps: while !stopped.load(Ordering::Relaxed) {
                    let signal = match &source {
                        Source::Bluetooth { address, path, .. } => {
                            Ok(bt::find_signal(address, path))
                        }
                        Source::Wifi { device, ssid } => {
                            nm::find_signal(device, ssid, &mut rescans)
                        }
                        Source::Station { device, mac, ip } => {
                            keep_awake(ip, &mut ping);
                            nm::station_signal(device, mac)
                        }
                    };
                    let pitch = match &signal {
                        Ok(Some(signal)) => Some(pitch(signal.level)),
                        _ => None,
                    };
                    if sender.send(signal).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    for samples in beep(pitch).chunks(chunk) {
                        if stopped.load(Ordering::Relaxed) {
                            break 'beeps;
                        }
                        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                        if stdin.write_all(&bytes).is_err() {
                            let mut stderr = String::new();
                            if let Some(mut pipe) = child.stderr.take() {
                                let _ = pipe.read_to_string(&mut stderr);
                            }
                            let err = format!("pw-cat stopped: {}", stderr.trim());
                            let _ = sender.send(Err(failed("BT-12", "play the find sound", err)));
                            ctx.request_repaint();
                            break 'beeps;
                        }
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                if let Some(mut ping) = ping {
                    let _ = ping.kill();
                    let _ = ping.wait();
                }
                log::info!("stopped finding");
            })
            .expect("a finder thread");
        Self {
            key,
            signal: None,
            error: None,
            stop,
            readings,
        }
    }

    pub fn poll(&mut self) {
        for reading in self.readings.try_iter() {
            match reading {
                Ok(signal) => {
                    self.signal = Some(signal);
                    self.error = None;
                }
                Err(err) => self.error = Some(err),
            }
        }
    }

    /// For the pane's Signal row.
    pub fn text(&self) -> String {
        match &self.signal {
            None => "measuring…".to_owned(),
            Some(None) => "not heard".to_owned(),
            Some(Some(signal)) => signal.text.clone(),
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// An idle device sends nothing, so its signal would stand still; a ping now
/// and then makes it answer. One at a time, without waiting for it.
fn keep_awake(ip: &str, ping: &mut Option<Child>) {
    if ip.is_empty()
        || ping
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    {
        return;
    }
    log::debug!("$ ping -c 1 -W 1 {ip}");
    *ping = Command::new("ping")
        .args(["-c", "1", "-W", "1", ip])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| failed("NET-1", "ping the device being found", err.to_string()))
        .ok();
}

/// Two octaves up from 300 Hz.
fn pitch(level: f32) -> f32 {
    300.0 * 4.0f32.powf(level.clamp(0.0, 1.0))
}

/// One beep and its pause as samples; only the pause when nothing is heard.
fn beep(pitch: Option<f32>) -> Vec<f32> {
    let tone = (BEEP.as_secs_f32() * RATE as f32) as usize;
    let gap = (BEEP_GAP.as_secs_f32() * RATE as f32) as usize;
    let mut samples = vec![0.0; tone + gap];
    let Some(pitch) = pitch else {
        return samples;
    };
    // Fading in and out keeps it from clicking.
    let fade = RATE / 200;
    for (i, sample) in samples[..tone].iter_mut().enumerate() {
        let envelope = (i.min(tone - i) as f32 / fade as f32).min(1.0);
        let t = i as f32 / RATE as f32;
        *sample = 0.5 * envelope * (std::f32::consts::TAU * pitch * t).sin();
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stronger_signal_beeps_higher() {
        assert!(pitch(0.0) < pitch(0.5) && pitch(0.5) < pitch(1.0));
        let silent = beep(None);
        let loud = beep(Some(pitch(1.0)));
        assert_eq!(silent.len(), loud.len());
        assert!(silent.iter().all(|s| *s == 0.0));
        assert!(loud.iter().any(|s| s.abs() > 0.4));
        assert!(loud.iter().all(|s| s.abs() <= 0.5));
    }
}
