use egui::{Frame, Margin, RichText, Stroke, Ui};

use super::Action;
use super::overlay::Overlay;
use crate::finder::{Finder, Source};
use crate::form::{self, Form, Icon};
use crate::nm::{IpSettings, Kind, Request, Settings, Snapshot};
use crate::theme::Palette;

const KINDS: [(&str, Kind); 2] = [("Wi-Fi", Kind::Wifi), ("Ethernet", Kind::Ethernet)];
const LABEL: f32 = 170.0;
const FIELD: f32 = 300.0;
const PADDING: i8 = 16;

/// Label and value rows of what a connected device is linked to.
type Rows = Result<Vec<(&'static str, String)>, String>;

const MODES: [(&str, &str); 3] = [
    ("Client", "infrastructure"),
    ("Access Point", "ap"),
    ("Ad-Hoc", "adhoc"),
];
const SECURITY: [(&str, &str); 4] = [
    ("None", ""),
    ("WPA & WPA2 Personal", "wpa-psk"),
    ("WPA3 Personal", "sae"),
    ("Enhanced Open (OWE)", "owe"),
];
const IPV4: [(&str, &str); 5] = [
    ("Disabled", "disabled"),
    ("Automatic", "auto"),
    ("Link-Local", "link-local"),
    ("Manual", "manual"),
    ("Shared", "shared"),
];
const IPV6: [(&str, &str); 7] = [
    ("Ignore", "ignore"),
    ("Disabled", "disabled"),
    ("Automatic", "auto"),
    ("Automatic (DHCP only)", "dhcp"),
    ("Link-Local", "link-local"),
    ("Manual", "manual"),
    ("Shared", "shared"),
];

/// A network in range, which can be found by sound.
pub struct Find<'a> {
    pub finder: &'a mut Option<Finder>,
    /// The list row it is for.
    pub key: &'a str,
    pub device: &'a str,
    pub ssid: &'a str,
}

pub struct Editor {
    uuid: Option<String>,
    choose_kind: bool,
    focus: Option<&'static str>,
    settings: Option<Settings>,
    show_password: bool,
    show_status: bool,
    show_link: bool,
    show_ipv4: bool,
    show_ipv6: bool,
}

impl Editor {
    /// An editor that waits for `Reply::Loaded`; the caller sends `Request::Load`.
    pub fn load(uuid: String) -> Self {
        Self {
            uuid: Some(uuid),
            choose_kind: false,
            focus: None,
            settings: None,
            show_password: false,
            show_status: true,
            show_link: true,
            show_ipv4: false,
            show_ipv6: false,
        }
    }

    pub fn create(snap: &Snapshot) -> Self {
        let (label, kind) = &KINDS[0];
        let mut editor = Self::load(String::new());
        editor.uuid = None;
        editor.choose_kind = true;
        editor.focus = Some("kind");
        editor.loaded(Settings::new(kind.clone(), next_name(snap, label)));
        editor
    }

    pub fn join(ssid: &str, secured: bool) -> Self {
        let mut settings = Settings::new(Kind::Wifi, ssid.to_owned());
        settings.ssid = ssid.to_owned();
        settings.autoconnect = false;
        if secured {
            settings.key_mgmt = "wpa-psk".to_owned();
        }
        let mut editor = Self::load(String::new());
        editor.uuid = None;
        editor.loaded(settings);
        editor
    }

    pub fn focus_on(&mut self, id: &'static str) {
        self.focus = Some(id);
    }

    pub fn uuid(&self) -> Option<&str> {
        self.uuid.as_deref()
    }

    pub fn loaded(&mut self, settings: Settings) {
        // Like nmtui, IP sections open by themselves only when set by hand.
        self.show_ipv4 = settings.ipv4.method == "manual";
        self.show_ipv6 = settings.ipv6.method == "manual";
        self.settings = Some(settings);
    }

    /// `live` is the device the connection is active on, if it is, and what
    /// that device is linked to.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        form: &mut Form<'_>,
        snap: &Snapshot,
        live: Option<(&str, &Rows)>,
        mut find: Option<Find<'_>>,
    ) -> Option<Action> {
        let pal = form.pal().clone();
        let title = if self.uuid.is_some() {
            "Edit Connection"
        } else {
            "New Connection"
        };
        let heading = RichText::new(title)
            .font(fastframe_fonts::Weight::SemiBold.font_id(16.0))
            .color(pal.text);
        let Some(settings) = &mut self.settings else {
            Frame::new().inner_margin(PADDING).show(ui, |ui| {
                ui.label(heading);
                ui.add_space(10.0);
                ui.label(RichText::new("Loading the connection…").color(pal.dim));
            });
            return None;
        };
        let saved = self
            .uuid
            .as_ref()
            .and_then(|uuid| snap.connections.iter().find(|c| &c.uuid == uuid));
        let finding = find
            .as_ref()
            .and_then(|f| f.finder.as_ref().filter(|finder| finder.key == f.key));
        let finding_text = finding.map(|f| (f.text(), f.error.clone()));
        let (mut found, mut reset, mut save, mut delete) = (false, false, false, false);
        let mut devices = false;
        let header = Margin {
            bottom: 10,
            ..Margin::same(PADDING)
        };
        Frame::new().inner_margin(header).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(heading);
                if self.choose_kind {
                    ui.add_space(12.0);
                    let mut index = KINDS
                        .iter()
                        .position(|(_, kind)| *kind == settings.kind)
                        .unwrap_or(0);
                    let labels = KINDS.map(|(label, _)| label);
                    if form.tabs(ui, "kind", &labels, &mut index) {
                        let (label, kind) = &KINDS[index];
                        log::debug!("new connection type: {label}");
                        *settings = Settings::new(kind.clone(), next_name(snap, label));
                    }
                }
                let buttons = 2.0
                    + f32::from(
                        u8::from(saved.is_some())
                            + u8::from(find.is_some())
                            + u8::from(live.is_some()),
                    );
                let width =
                    buttons * form::FIELD_HEIGHT + (buttons - 1.0) * ui.spacing().item_spacing.x;
                ui.add_space((ui.available_width() - width).max(0.0));
                if live.is_some() {
                    devices =
                        form.icon_button(ui, "devices", Icon::Network, "Devices on this network");
                }
                if find.is_some() {
                    let (icon, tip) = match finding_text {
                        Some(_) => (Icon::Stop, "Stop the sound"),
                        None => (Icon::Find, "Find by sound"),
                    };
                    found = form.icon_button(ui, "find", icon, tip);
                }
                reset = form.icon_button(ui, "reset", Icon::Reset, "Discard changes");
                save = form.icon_button(ui, "save", Icon::Save, "Save");
                if saved.is_some() {
                    delete = form.icon_button(ui, "delete", Icon::Trash, "Delete");
                }
            });
        });
        // The fields scroll edge to edge under this line, so the scrollbar sits in
        // the padding and clipped rows end at a visible border.
        let y = ui.cursor().top();
        ui.painter()
            .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.outline));
        if let Some(id) = self.focus.take() {
            form.focus(id);
            ui.ctx().request_repaint();
        }
        let (show_status, show_password, show_link, show_ipv4, show_ipv6) = (
            &mut self.show_status,
            &mut self.show_password,
            &mut self.show_link,
            &mut self.show_ipv4,
            &mut self.show_ipv6,
        );
        egui::ScrollArea::vertical()
            .id_salt("editor")
            .max_height(ui.available_height().max(60.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                Frame::new().inner_margin(PADDING).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if let Some((text, error)) = &finding_text {
                        if let Some(error) = error {
                            ui.label(RichText::new(error).color(pal.danger));
                        }
                        field(ui, &pal, "Signal", |ui| {
                            ui.label(RichText::new(text).color(pal.text))
                        });
                        ui.add_space(4.0);
                    }
                    if let Some((_, details)) = live {
                        status(ui, form, details, show_status);
                        ui.add_space(4.0);
                    }
                    field(ui, &pal, "Profile name", |ui| {
                        form.entry(ui, "name", &mut settings.name, FIELD, false)
                    });
                    field(ui, &pal, "Device", |ui| {
                        form.entry(ui, "device", &mut settings.device, FIELD, false)
                    });
                    ui.add_space(4.0);
                    match settings.kind {
                        Kind::Wifi => wifi(ui, form, settings, show_link, show_password),
                        Kind::Ethernet => ethernet(ui, form, settings, show_link),
                        Kind::Other(_) => {}
                    }
                    ui.add_space(4.0);
                    ip(
                        ui,
                        form,
                        "ipv4",
                        "IPv4 CONFIGURATION",
                        &IPV4,
                        &mut settings.ipv4,
                        show_ipv4,
                    );
                    ip(
                        ui,
                        form,
                        "ipv6",
                        "IPv6 CONFIGURATION",
                        &IPV6,
                        &mut settings.ipv6,
                        show_ipv6,
                    );
                    ui.add_space(4.0);
                    form.check(
                        ui,
                        "autoconnect",
                        "Automatically connect",
                        &mut settings.autoconnect,
                    );
                    form.check(
                        ui,
                        "all_users",
                        "Available to all users",
                        &mut settings.all_users,
                    );
                });
            });
        if let Some(find) = find.as_mut().filter(|_| found) {
            *find.finder = match finding_text {
                Some(_) => None,
                None => {
                    log::info!("finding Wi-Fi '{}' by sound", find.ssid);
                    let source = Source::Wifi {
                        device: find.device.to_owned(),
                        ssid: find.ssid.to_owned(),
                    };
                    Some(Finder::start(ui.ctx().clone(), find.key.to_owned(), source))
                }
            };
        }
        if let Some((device, _)) = live.filter(|_| devices) {
            log::debug!("devices on {device}");
            let title = format!("Devices on {}", settings.name);
            return Some(Action::Overlay(Overlay::devices(
                ui.ctx().clone(),
                title,
                device.to_owned(),
            )));
        }
        if reset {
            log::debug!("discarding changes to '{}'", settings.name);
            return Some(Action::Pop);
        }
        if let Some(connection) = saved.filter(|_| delete) {
            return Some(Action::Overlay(Overlay::confirm_delete(connection)));
        }
        if !save {
            return None;
        }
        Some(match validate(settings) {
            Ok(()) => Action::Send(Request::Save(Box::new(settings.clone()))),
            Err(message) => Action::Overlay(Overlay::message("Invalid connection", message)),
        })
    }
}

fn next_name(snap: &Snapshot, label: &str) -> String {
    (1..)
        .map(|n| format!("{label} connection {n}"))
        .find(|name| !snap.connections.iter().any(|c| &c.name == name))
        .unwrap()
}

fn validate(settings: &Settings) -> Result<(), String> {
    if settings.name.trim().is_empty() {
        return Err(invalid("EDIT-1", "The profile name is empty."));
    }
    if settings.kind == Kind::Wifi && settings.ssid.is_empty() {
        return Err(invalid("EDIT-2", "The SSID is empty."));
    }
    if settings.kind == Kind::Wifi
        && matches!(settings.key_mgmt.as_str(), "wpa-psk" | "sae")
        && settings.password_changed
        && !(8..=63).contains(&settings.password.len())
    {
        return Err(invalid("EDIT-3", "A WPA password has 8 to 63 characters."));
    }
    Ok(())
}

fn invalid(code: &str, message: &str) -> String {
    log::warn!("[{code}] rejected connection: {message}");
    format!("[{code}] {message}")
}

fn field<R>(ui: &mut Ui, pal: &Palette, text: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        form::label(ui, pal, text, LABEL);
        add(ui)
    })
    .inner
}

fn section(
    ui: &mut Ui,
    form: &mut Form<'_>,
    id: &str,
    title: &str,
    shown: &mut bool,
    extra: impl FnOnce(&mut Ui, &mut Form<'_>),
) {
    let pal = form.pal().clone();
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL, 26.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    RichText::new(title)
                        .color(pal.info)
                        .font(fastframe_fonts::Weight::SemiBold.font_id(13.0)),
                )
            },
        );
        extra(ui, form);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (icon, tip) = if *shown {
                (Icon::ChevronUp, "Hide")
            } else {
                (Icon::ChevronDown, "Show")
            };
            if form.icon_button(ui, &format!("{id}-toggle"), icon, tip) {
                *shown = !*shown;
            }
        });
    });
}

fn status(ui: &mut Ui, form: &mut Form<'_>, details: &Rows, shown: &mut bool) {
    section(ui, form, "status", "CONNECTED", shown, |_, _| {});
    if !*shown {
        return;
    }
    let pal = form.pal().clone();
    let rows = match details {
        Ok(rows) => rows,
        Err(err) => {
            ui.label(RichText::new(err.as_str()).color(pal.danger));
            return;
        }
    };
    for (label, value) in rows {
        // Top-aligned, so a label stays on the first line of a list of addresses.
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(LABEL, 0.0),
                egui::Layout::right_to_left(egui::Align::Min),
                |ui| ui.label(RichText::new(*label).color(pal.secondary)),
            );
            ui.vertical(|ui| {
                for line in value.lines() {
                    let text = RichText::new(line).color(pal.text);
                    match form::web_url(line) {
                        Some(url) => form::link(ui, &pal, text, &url),
                        None => {
                            ui.label(text);
                        }
                    }
                }
            });
        });
    }
}

fn index_of(options: &[(&str, &str)], value: &str) -> Option<usize> {
    options.iter().position(|(_, v)| *v == value)
}

fn wifi(
    ui: &mut Ui,
    form: &mut Form<'_>,
    settings: &mut Settings,
    shown: &mut bool,
    show_password: &mut bool,
) {
    section(ui, form, "wifi", "WI-FI", shown, |_, _| {});
    if !*shown {
        return;
    }
    let pal = form.pal().clone();
    field(ui, &pal, "SSID", |ui| {
        form.entry(ui, "ssid", &mut settings.ssid, FIELD, false)
    });

    let mut mode = index_of(&MODES, &settings.mode).unwrap_or(0);
    let labels: Vec<&str> = MODES.iter().map(|(label, _)| *label).collect();
    if field(ui, &pal, "Mode", |ui| {
        form.select(ui, "mode", &mut mode, &labels, FIELD)
    }) {
        settings.mode = MODES[mode].1.to_owned();
    }

    // A security nmtui offers but this editor does not stays as it is.
    let mut options: Vec<(&str, &str)> = SECURITY.to_vec();
    let other = format!("Other ({})", settings.key_mgmt);
    if index_of(&options, &settings.key_mgmt).is_none() {
        options.push((&other, &settings.key_mgmt));
    }
    let mut security = index_of(&options, &settings.key_mgmt).unwrap_or(0);
    let labels: Vec<&str> = options.iter().map(|(label, _)| *label).collect();
    if field(ui, &pal, "Security", |ui| {
        form.select(ui, "security", &mut security, &labels, FIELD)
    }) {
        settings.key_mgmt = options[security].1.to_owned();
    }

    if matches!(settings.key_mgmt.as_str(), "wpa-psk" | "sae") {
        let changed = field(ui, &pal, "Password", |ui| {
            form.entry(
                ui,
                "password",
                &mut settings.password,
                FIELD,
                !*show_password,
            )
        });
        settings.password_changed |= changed;
        ui.horizontal(|ui| {
            ui.add_space(LABEL + ui.spacing().item_spacing.x);
            form.check(ui, "show-password", "Show password", show_password);
        });
    }
    field(ui, &pal, "BSSID", |ui| {
        form.entry(ui, "bssid", &mut settings.bssid, FIELD, false)
    });
    field(ui, &pal, "Cloned MAC address", |ui| {
        form.entry(ui, "cloned-mac", &mut settings.cloned_mac, FIELD, false)
    });
    mtu(ui, form, &pal, &mut settings.mtu);
}

fn ethernet(ui: &mut Ui, form: &mut Form<'_>, settings: &mut Settings, shown: &mut bool) {
    section(ui, form, "ethernet", "ETHERNET", shown, |_, _| {});
    if !*shown {
        return;
    }
    let pal = form.pal().clone();
    field(ui, &pal, "Cloned MAC address", |ui| {
        form.entry(ui, "cloned-mac", &mut settings.cloned_mac, FIELD, false)
    });
    mtu(ui, form, &pal, &mut settings.mtu);
}

fn mtu(ui: &mut Ui, form: &mut Form<'_>, pal: &Palette, mtu: &mut String) {
    field(ui, pal, "MTU", |ui| {
        form.entry(ui, "mtu", mtu, 90.0, false);
        if mtu.trim().is_empty() {
            ui.label(RichText::new("(default)").color(pal.dim));
        } else {
            ui.label(RichText::new("bytes").color(pal.dim));
        }
    });
}

fn ip(
    ui: &mut Ui,
    form: &mut Form<'_>,
    id: &str,
    title: &str,
    methods: &[(&str, &str)],
    ip: &mut IpSettings,
    shown: &mut bool,
) {
    let mut method = index_of(methods, &ip.method).unwrap_or(0);
    let labels: Vec<&str> = methods.iter().map(|(label, _)| *label).collect();
    section(ui, form, id, title, shown, |ui, form| {
        if form.select(ui, &format!("{id}-method"), &mut method, &labels, 200.0) {
            ip.method = methods[method].1.to_owned();
        }
    });
    if !*shown {
        return;
    }
    let pal = form.pal().clone();
    let entry = |ui: &mut Ui, form: &mut Form<'_>, name: &str, text: &mut String| {
        field(ui, &pal, name, |ui| {
            form.entry(ui, &format!("{id}-{name}"), text, FIELD, false)
        });
    };
    entry(ui, form, "Addresses", &mut ip.addresses);
    entry(ui, form, "Gateway", &mut ip.gateway);
    entry(ui, form, "DNS servers", &mut ip.dns);
    entry(ui, form, "Search domains", &mut ip.search);
    ui.horizontal(|ui| {
        ui.add_space(LABEL + ui.spacing().item_spacing.x);
        ui.label(
            RichText::new("Separate several with commas; addresses as 192.168.1.5/24.")
                .color(pal.dim)
                .small(),
        );
    });
    let family = if id == "ipv4" { "IPv4" } else { "IPv6" };
    for (key, label, value) in [
        (
            "never-default",
            "Never use this network for default route".to_owned(),
            &mut ip.never_default,
        ),
        (
            "ignore-routes",
            "Ignore automatically obtained routes".to_owned(),
            &mut ip.ignore_auto_routes,
        ),
        (
            "ignore-dns",
            "Ignore automatically obtained DNS parameters".to_owned(),
            &mut ip.ignore_auto_dns,
        ),
        (
            "required",
            format!("Require {family} addressing for this connection"),
            &mut ip.required,
        ),
    ] {
        ui.horizontal(|ui| {
            ui.add_space(LABEL + ui.spacing().item_spacing.x);
            form.check(ui, &format!("{id}-{key}"), &label, value);
        });
    }
}
