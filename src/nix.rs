//! The settings pages write NixOS modules here, one file per page, imported
//! together by `settings-dialog/default.nix`. Only what a NixOS option can carry
//! goes in, and never a secret: no Wi-Fi password, no static address.

use std::path::{Path, PathBuf};

use crate::nm::{Kind, Settings};

const DIR: &str = "/etc/nixos";
const OURS: &str = "settings-dialog";
const FILES: [&str; 3] = ["sound.nix", "bluetooth.nix", "network.nix"];
const HEADER: &str = "# Written by settings-dialog. Edits here are overwritten.\n";

pub fn dir() -> PathBuf {
    std::env::var_os("SETTINGS_DIALOG_NIX_DIR")
        .map_or_else(|| Path::new(DIR).join(OURS), PathBuf::from)
}

/// Whether the NixOS config imports the directory, without which a written
/// file does nothing.
pub fn imported() -> bool {
    let Ok(entries) = std::fs::read_dir(DIR) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.path().extension().is_some_and(|end| end == "nix")
            && std::fs::read_to_string(entry.path()).is_ok_and(|text| text.contains(OURS))
    })
}

/// Writes one module and the `default.nix` that imports all of them.
pub fn write(file: &str, body: &str) -> Result<(), String> {
    write_in(&dir(), file, body)
}

fn write_in(dir: &Path, file: &str, body: &str) -> Result<(), String> {
    log::info!("writing {}", dir.join(file).display());
    std::fs::create_dir_all(dir)
        .map_err(|err| failed("NIX-1", "make the NixOS module directory", err.to_string()))?;
    std::fs::write(dir.join(file), format!("{HEADER}{body}"))
        .map_err(|err| failed("NIX-2", "write the NixOS module", err.to_string()))?;
    let imports: String = FILES
        .iter()
        .filter(|name| dir.join(name).exists())
        .map(|name| format!("    ./{name}\n"))
        .collect();
    let all = format!("{HEADER}{{\n  imports = [\n{imports}  ];\n}}\n");
    std::fs::write(dir.join("default.nix"), all)
        .map_err(|err| failed("NIX-3", "write the NixOS module list", err.to_string()))
}

fn failed(code: &str, what: &str, why: String) -> String {
    let message = format!("[{code}] could not {what}: {why}");
    log::error!("{message}");
    message
}

/// The default output and input, pinned by giving them the highest session
/// priority. Only a device WirePlumber's ALSA or Bluetooth monitor creates can
/// be pinned this way; a filter or a virtual node has no monitor rule.
pub fn sound(default_sink: Option<&str>, default_source: Option<&str>) -> String {
    let mut alsa = Vec::new();
    let mut bluez = Vec::new();
    let mut skipped = Vec::new();
    for name in [default_sink, default_source].into_iter().flatten() {
        if name.starts_with("alsa_") {
            alsa.push(name);
        } else if name.starts_with("bluez_") {
            bluez.push(name);
        } else {
            skipped.push(name);
        }
    }
    let mut out = String::new();
    for name in &skipped {
        out.push_str(&format!(
            "# {name} is not an ALSA or Bluetooth device, so NixOS has no option for it.\n"
        ));
    }
    out.push_str("{\n  services.pipewire.wireplumber.extraConfig.\"90-settings-dialog\" = {\n");
    for (section, names) in [("monitor.alsa.rules", alsa), ("monitor.bluez.rules", bluez)] {
        if names.is_empty() {
            continue;
        }
        out.push_str(&format!("    \"{section}\" = [\n"));
        for name in names {
            out.push_str(&format!(
                "      {{\n        matches = [ {{ \"node.name\" = {}; }} ];\n        actions.update-props.\"priority.session\" = 3000;\n      }}\n",
                quote(name)
            ));
        }
        out.push_str("    ];\n");
    }
    out.push_str("  };\n}\n");
    out
}

pub fn bluetooth(powered: bool) -> String {
    format!(
        "{{\n  hardware.bluetooth.powerOnBoot = {powered};\n  hardware.bluetooth.settings.Policy.AutoEnable = {powered};\n}}\n"
    )
}

/// Every profile NetworkManager can rebuild without a secret. A profile with a
/// password or a static address is left out, because writing it without them
/// would replace the working one with a broken one.
pub fn network(profiles: &[Settings]) -> String {
    let mut out = String::new();
    let mut body = String::new();
    for profile in profiles {
        if let Some(why) = secret(profile) {
            out.push_str(&format!("# {} is left out: {why}.\n", profile.name));
            continue;
        }
        let kind = match profile.kind {
            Kind::Wifi => "wifi",
            Kind::Ethernet => "ethernet",
            Kind::Other(_) => continue,
        };
        body.push_str(&format!("    {} = {{\n", quote(&profile.name)));
        body.push_str(&format!(
            "      connection = {{\n        id = {};\n        type = \"{kind}\";\n        autoconnect = {};\n      }};\n",
            quote(&profile.name),
            profile.autoconnect
        ));
        if profile.kind == Kind::Wifi {
            body.push_str(&format!(
                "      wifi = {{\n        ssid = {};\n        mode = {};\n      }};\n",
                quote(&profile.ssid),
                quote(&profile.mode)
            ));
        }
        body.push_str("      ipv4.method = \"auto\";\n      ipv6.method = \"auto\";\n    };\n");
    }
    out.push_str("{\n  networking.networkmanager.ensureProfiles.profiles = {\n");
    out.push_str(&body);
    out.push_str("  };\n}\n");
    out
}

/// Why a profile can't be written without its secrets.
fn secret(profile: &Settings) -> Option<&'static str> {
    if !profile.key_mgmt.is_empty() && profile.key_mgmt != "none" {
        return Some("it needs a password");
    }
    if profile.ipv4.method != "auto" || profile.ipv6.method != "auto" {
        return Some("it has addresses of its own");
    }
    None
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(name: &str) -> Settings {
        let mut settings = Settings::new(Kind::Wifi, name.to_owned());
        settings.ssid = name.to_owned();
        settings
    }

    #[test]
    fn writing_a_module_also_writes_the_import_list() {
        let dir = std::env::temp_dir().join(format!("settings-dialog-nix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_in(&dir, "bluetooth.nix", &bluetooth(true)).unwrap();
        write_in(&dir, "sound.nix", &sound(None, None)).unwrap();
        let all = std::fs::read_to_string(dir.join("default.nix")).unwrap();
        assert!(all.contains("./sound.nix"));
        assert!(all.contains("./bluetooth.nix"));
        assert!(!all.contains("./network.nix"));
        let written = std::fs::read_to_string(dir.join("bluetooth.nix")).unwrap();
        assert!(written.starts_with(HEADER));
        assert!(written.contains("powerOnBoot = true"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_sound_module_pins_alsa_and_bluetooth_devices() {
        let body = sound(Some("alsa_output.pci-0000_00_1f.3"), Some("bluez_input.AA"));
        assert!(body.contains("monitor.alsa.rules"));
        assert!(body.contains("monitor.bluez.rules"));
        assert!(body.contains("\"priority.session\" = 3000"));
        assert!(body.contains("alsa_output.pci-0000_00_1f.3"));
    }

    #[test]
    fn a_node_without_a_monitor_rule_is_only_mentioned() {
        let body = sound(Some("my-loopback"), None);
        assert!(body.contains("# my-loopback is not an ALSA or Bluetooth device"));
        assert!(!body.contains("monitor.alsa.rules"));
    }

    #[test]
    fn the_bluetooth_module_follows_the_switch() {
        assert!(bluetooth(true).contains("powerOnBoot = true"));
        assert!(bluetooth(false).contains("AutoEnable = false"));
    }

    #[test]
    fn an_open_profile_is_written_whole() {
        let body = network(&[wifi("Cafe")]);
        assert!(body.contains("\"Cafe\" = {"));
        assert!(body.contains("ssid = \"Cafe\""));
        assert!(body.contains("autoconnect = true"));
        assert!(body.contains("ipv4.method = \"auto\""));
    }

    #[test]
    fn a_profile_with_a_password_is_left_out() {
        let mut secured = wifi("Home");
        secured.key_mgmt = "wpa-psk".to_owned();
        let body = network(&[secured]);
        assert!(body.contains("# Home is left out: it needs a password."));
        assert!(!body.contains("ssid"));
    }

    #[test]
    fn a_profile_with_its_own_address_is_left_out() {
        let mut static_ip = wifi("Lab");
        static_ip.ipv4.method = "manual".to_owned();
        let body = network(&[static_ip]);
        assert!(body.contains("# Lab is left out: it has addresses of its own."));
        assert!(!body.contains("192"));
    }

    #[test]
    fn a_name_with_a_quote_in_it_stays_one_string() {
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }
}
