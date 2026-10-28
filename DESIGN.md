# settings-dialog design specification

A small settings app: a NetworkManager front end with the screens and keyboard
model of `nmtui`, a Bluetooth page on top of BlueZ, and a Sound page on top of
PipeWire and WirePlumber. Built on fastframe
(Rust, egui 0.36) and themed Gruvbox Dark Hard.

## Goals

- Anyone who knows `nmtui` can use it without reading anything: the same
  editor fields in the same order, the same keys. Unlike `nmtui`, editing and
  activating share one list, with the actions inline on each row.
- Fully usable with the keyboard alone; the mouse works too.
- Never block the UI on NetworkManager or BlueZ. Every `nmcli`, `busctl`, `bluetoothctl`, `ip` and PipeWire tool call runs off the UI thread.
- Every failure is shown in place with a stable error code that is also in the log.

Non-goals: hostname and WWAN settings, VPN, bond, bridge, VLAN and other connection types, 802.1X
enterprise authentication, replacing `nmcli` or `bluetoothctl` with a D-Bus client library,
or an own Bluetooth pairing agent (bluetoothctl's DisplayOnly agent is used),
and a PipeWire client library (`pw-dump`, `pw-metadata`, `wpctl`, `pw-cli` and `pw-link` are used).

## Layout

A Settings menu (200 px) on the left picks the page: Network, Bluetooth, Sound or
Screens. Each
page is a list with a pane on the right that always shows the selected row.
Prompts are small popups on top of it; there are no nested dialog boxes.

```
┌ OS window ──────────────────────────────────────────────────────────┐
│ Settings    Networks (accent, SemiBold 18)  [◎] [⟳] [+]  (●) Wi-Fi  │
│ ┌ menu ───┐ ┌ list ───────────────────────┐ ┌ editor (600 px) ────┐ │
│ │ Network │ │ Wi-Fi (wlan0)               │ │ Edit Connection ↺ 💾 🗑│ │
│ │Bluetooth│ │  Home         ▂▄▆█  (●━)    │ │  Profile name [   ] │ │
│ │ Sound   │ │  Cafe         ▂▄▆_  (━○)    │ │  …                  │ │
│ │         │ │ Saved networks              │ │                     │ │
│ │         │ │  Office                     │ │                     │ │
│ └─────────┘ └─────────────────────────────┘ └─────────────────────┘ │
└─────────────────────────────────────────────────────────────────────┘
```

- The menu and the page share one focus model, so Tab moves from the menu into the page. Enter on the menu moves the focus to the page's list. Leaving the Bluetooth page stops its discovery.

- The Wi-Fi on/off switch sits in the top-right corner and calls `nmcli radio wifi on|off` straight away. To its left sit a radar button (Wi-Fi channels), a reload button (search for networks) and a "+" button (add a connection); a spinner shows left of them while a search runs. They come first in the Tab order. The screen title is on the left.
- Margins: 28 px on the sides, 20 px at the top and bottom. There is no key hint footer.
- The editor is a 600 px pane on the right and the list takes the rest. Only when no row can be edited (an empty list) does the list take the full width. The window starts at 1360×720 and is at least 1200×480.
- Lists stretch to their area. A list row can carry an activation switch on its right edge; signal bars sit left of it, with the signal in percent before them. A connected row also shows, dimmed before the percentage, its device's IPv4 address and its download and upload rates (`↓ 1.2 MB/s  ↑ 34 kB/s`). The rates come from the `/sys/class/net/<device>/statistics` byte counters, read every second.
- The editor pane's actions are icon buttons in its top-right corner, beside the title: reset (discard changes), save, and a bin (delete) for saved profiles. Each has a tooltip. A line separates this header from the fields, which scroll edge to edge below it, so the scrollbar sits in the padding and cut-off rows end at a border instead of in the margin.
- Form screens use one column: labels are right-aligned, and fields follow them on the same line.
- Prompts put their button rows (OK, Cancel) right-aligned at the bottom.
- Every prompt (a message, the delete or remove confirmation, a password, or "please wait") is a small centred popup over the dimmed page, which gets no keys and can't be clicked until the prompt is answered. The screen keeps its state, including an open editor.

## Screens

| Screen | Opened by | Contents | Esc |
|---|---|---|---|
| Networks | start | one list: networks in range per Wi-Fi device (strongest first, connected one on top), then the devices on each hotspot we run (an interface in AP mode, from `iw dev <device> station dump`, named by their DHCP lease's hostname from NetworkManager's dnsmasq, else their MAC), then saved Wi-Fi networks that are out of range, Ethernet, and other connections grouped by type; "+" in the top bar adds a connection and the save button beside it writes the NixOS module. Refreshes every 5 s, with a rescan the first time and whenever the reload button is pressed; before a rescan, which takes seconds, the networks already known are listed | quit |
| Edit Connection (right pane) | selecting a row | For a connection that is active, a CONNECTED section first, read-only and refreshed every 5 s: access point (BSSID), channel with frequency and width, signal in dBm, link speed down and up (from `iw dev <device> link`; without iw, nmcli's in-use access point gives the same rows with signal in % and one rate), then MAC address, IPv4 address, gateway, DNS servers, domain, IPv6 addresses and gateway, and the hostname; the addresses, DNS servers and domain are links. Then Profile name and Device; WI-FI section (SSID, Mode, Security, Password, BSSID, Cloned MAC, MTU); IPv4 and IPv6 CONFIGURATION with a chevron to show or hide them; Automatically connect; Available to all users. For a Wi-Fi network in range a sound-lines button (Find by sound, a square to stop) comes first, then, for an active connection, a network button (Devices on this network), then reset, save and delete sit top right. While finding, a Signal row at the top shows each reading; the beeps are those of the Bluetooth finder and play on the default output, to walk around with the laptop. The signal is the link's when connected to that network (fresh with every frame), else the strongest access point with that SSID in the kernel's scan results, with a NetworkManager rescan every 5 s, so it moves only every few seconds. It stops on the button, when another row is selected, when Wi-Fi goes off or the page is left. A saved profile is loaded in the background while the previous one stays on screen. A network in range without a profile gets a draft profile with its SSID, WPA if secured, and Automatically connect off. Selecting another row drops unsaved edits. Reset reverts; a successful save or a deleted connection reloads it | back to the list |
| Hotspot device (right pane) | selecting a hotspot device | Name and a Find by sound button top right; signal in dBm, MAC address, IPv4 address, hostname, connected time, the hotspot's interface. While finding, the signal is the last frame heard from the device (`iw station dump`, with how long ago once over 2 s), and the device is pinged one at a time so an idle one keeps sending. It stops like the network finder | back to the list |
| Bluetooth | the menu | Top bar: a spinner while discovering, a save button (Save to NixOS) and the Bluetooth switch (`rfkill unblock bluetooth`, then `bluetoothctl power on`, retried for 2 s). One list: My devices (paired, connected first, then by name) and Nearby devices (unpaired with a name, strongest first), then Nearby devices without a name (strongest first, shown by their address). Rows show the battery and the signal in dBm, signal bars, and a switch while powered: on disconnects, off connects a paired device or pairs, trusts and connects a new one. Reads `GetManagedObjects` from BlueZ every 2 s and runs discovery (`bluetoothctl --timeout 30 scan on`, restarted when it ends) the whole time the page is open and Bluetooth is on, because BlueZ only reports RSSI from advertisements seen during discovery | back to the list, otherwise quit |
| Device (right pane) | selecting a Bluetooth row | Name, then icon buttons top right with tooltips: sound lines (Find by sound, a square to stop) and unplug (Disconnect), plug (Connect) or Bluetooth (Pair and connect) while Bluetooth is on, and a bin (Remove device) for paired devices. DEVICE section: type, status, signal (dBm and %, or "unknown" when not advertising — usually so for connected devices), transmit power, battery, address with its type, device ID (vendor:product from the modalias), manufacturer. Trusted and Blocked checkboxes for paired devices. SERVICES lists the UUIDs by name. Find by sound beeps about three times a second, higher the stronger the signal (300 Hz at nothing to 1200 Hz at full), through the device's own sink when it is connected so the sound comes from where it lies, else through the default output; the signal row then shows each reading. It stops on the button, when another device is selected, when Bluetooth goes off or the page is left | back to the list |
| Sound | the menu | Top bar: a graph button (Node graph) and a save button (Save to NixOS). One list: Outputs (sinks), Inputs (sources), then Streams (app playback and recording, and filters' streams: every other node with a volume); a duplex device is in both. Rows are detailed "current", "default" and the volume ("42 %" or "muted"). A status line warns when the saved default is not connected. Reads `pw-dump` every 2 s. Enter or a double click makes the row the default | back to the list, otherwise quit |
| Sound device (right pane) | selecting a Sound row | Name, then icon buttons top right: a speaker (Use now) unless it is the current device, and a star (Make default) unless it is the saved default; streams have neither. VOLUME section: a slider (0–100 %) for the loudest channel that keeps the balance between channels, then, with more than one channel, a slider per channel ("Front left", "Front right", …), and a Muted checkbox (`wpctl set-mute`). DEVICE (or STREAM) section: for a Bluetooth device a Codec select first, one entry per available profile of the device ("SBC", "AAC", "LDAC", …; the headset profiles, which also bring the microphone, as "mSBC, headset"), which switches the profile (`pw-cli set-param <device> Profile`, saved by WirePlumber) and keeps the device selected although its node gets a new ID; then status, state, kind, driver (ALSA, Bluetooth or PipeWire virtual), channels, the sample rate it runs at now ("96 kHz"), allowed rates, node name and ID. CONNECTED lists the nodes linked to it; streams show as "App: media" | back to the list |
| Screens | the menu | One list: Connected and Not connected monitors, with the resolution and refresh rate as the detail and a switch that turns a monitor on or off. The arrange map spans the top of the page, above the list and the pane; the pane holds the fields of the selected monitor. Reads `hyprctl -j monitors all` every 2 s, but a snapshot is only adopted while nothing is edited, so a refresh never drops an edit. Reset and save sit top right; saving focuses this window and brings the pointer onto it first, so the layout cannot move the window to a screen the mouse isn't on, then applies the layout live and asks to keep it: a "Keep this layout?" popup counts ten seconds down, and unless Keep is pressed the last layout is applied again, so a screen that went dark cannot lock the desktop out. Keep writes the layout to the file Hyprland loads. Saving every monitor off is refused. Top bar: a tablet button (Use a tablet as a screen / Stop the tablet screen) | back to the list, otherwise quit |
| Screen (right pane) | selecting a Screens row | The arrange map, across the top of the page, draws every monitor that is on to scale, as it sits on the desktop, fitted into the band; the selected one is accented. Click a monitor to select it and drag it to move it; its edges snap to another monitor's edges within 60 desktop pixels while it is dragged, and when it is let go it settles flush against the nearest neighbour, never overlapping one and never standing free, because Hyprland lays the desktop out by these coordinates. Changing a screen's resolution, scale or rotation settles it the same way, and so does turning one on. The pane itself holds: On, Resolution, Refresh rate (the rates that resolution has), Scale (100 to 300 %), Rotation (normal, 90° left, 180°, 90° right), Flipped, Variable refresh, Shows a copy of (mirror; a copy is left off the map and takes no room on the desktop, and Hyprland reports the copied monitor by ID, which is turned back into its name), and read-only Position, Output and the file the layout is saved in. A monitor that isn't plugged in keeps its saved settings and shows only the read-only rows. A warning line appears when Hyprland's config does not load the layout file. For the tablet's output the pane first shows this computer's addresses (for adding it in Moonlight by hand) and how many Moonlight streams are open, and while Moonlight wants to pair a Moonlight PIN field, which hands the PIN to Sunshine once four digits are typed | back to the list |
| Tablet screen | the tablet button on Screens | Hyprland gets a headless output `TABLET` (`hyprctl output create headless`), with the call `monitors.lua` has for it applied at once (Hyprland gives a new output only the rules it holds right now), or else 2360x1640 at 100 % to the right of the others; it then shows in the list and the map like any monitor, with common tablet sizes as its resolutions, and is arranged and saved the same way. Sunshine (`sunshine -0`, its config, pairings and keys in `$XDG_STATE_HOME/settings-dialog/sunshine/`) streams that output with the sound to Moonlight on the tablet, which sends touches, keys and mouse back. A Moonlight that pairs again is recorded a second time, and Sunshine then turns it away ("Client certificate identity is not enabled"), so repeated records are dropped from `state.json` before Sunshine starts, and when that warning shows up Sunshine is started again on them. Sound reaches the tablet only through Sunshine's own output `sink-sunshine-stereo` (`audio_sink`), even when Moonlight asks to play on the PC as well, so it plays on one side: Sunshine makes that output the default when a stream starts and restores the earlier default when it ends, and in between the Sound screen picks the side like for any other output. Sunshine captures with `wlr` screencopy, offers only the Desktop app, and its web UI only answers this computer. Pairing: Moonlight shows a PIN, Sunshine asks for it on stdin (`Please insert pin:`, no line end), and the pane asks for it. An output that shows a copy is gone for screen capture, so when the tablet's output copies another screen Sunshine is started again on that screen (it picks its output at start), and again on `TABLET` when the copy ends. Every layout block carries `mirror`, empty for none, because Hyprland keeps an earlier mirror when the field is left out. A copy is written with `position = "auto"`, and the copy is ended at stop the same way: Hyprland reports a copy at 0x0, and a screen placed there overlaps the laptop and raises Hyprland's "monitor layout is set up incorrectly" warning. Pressing the button again, or leaving the app, stops Sunshine, ends any copy (Hyprland cannot remove an output that shows one) and removes the output. The system configuration provides Sunshine (`services.sunshine` with `autoStart = false`, which also gives uinput access and Avahi so Moonlight finds the computer) and opens its ports, not 47990 | — |
| New Connection (right pane) | "+" | Wi-Fi / Ethernet tabs beside the title, then the same fields as Edit Connection. Switching tabs starts over with the defaults of that type. Stays until another row is selected | back to the list |

A row shows a switch when the network is in range, the connection is active, a
cable is plugged in (Ethernet device not `unavailable`), or the connection is of
another type. On: deactivate. Off: activate the saved profile, or join the
network (asking for a password if it is secured).

Enter or a double click on a row flips its switch; on a row without a switch it
moves the focus into the editor.

Prompts:

| Prompt | Shown when |
|---|---|
| Delete Connection | the editor's bin pressed. The default button is Cancel |
| Remove Device | the device pane's bin pressed. The default button is Cancel |
| Please wait (pairing) | pairing a device. When the agent shows a passkey or PIN, the text becomes "Type NNNNNN on the device, then press Enter." |
| Authentication required | a secured network that has no saved profile is activated |
| Please wait | a change is running (save, delete, up, down, connect) |
| Message | an operation failed |
| Node graph | the Sound page's graph button. Every PipeWire node with ports as a box: sources and players on the left, filters and devices with both sides in the middle, sinks on the right; each media in its own band, audio on top, then MIDI, then video, so MIDI devices don't sit among the audio links; links run under the boxes; inputs on a box's left edge, outputs on its right, coloured by media (audio, MIDI, video). Links are curves. Drag from one port to another, or pick both with Enter, to link them (`pw-link`, only output to input of the same media). A node side with more than one port has a ringed all-channels dot on that edge level with the title, above its ports and clear of their links; dragging from it to another node's all-channels dot or port, or picking both, links all channels at once: channel to channel (FL to FL, FR to FR), a mono output to every input, every output to a mono input, by order when the channel names don't match; from a sink its monitor ports; either order works. Link curves get thicker and port dots bigger with the signal level right now (-40 to 0 dB; a curve from 1 to 14 px, a dot from 5 to 20 px across); an input shows the sum of what its links bring, an all-channels dot its loudest port. Every node with a volume has a strip at the bottom: a speaker icon that mutes and unmutes, a volume bar to click or drag (it keeps the balance between channels), and the percentage; with more than one channel, one more strip per channel labelled with its short name (FL, FR). Click a link to select it; Disconnect removes the selected link, or all links of the port or all-channels dot under the keyboard cursor. Arrow keys walk the ports and all-channels dots, Esc first drops a picked port. Reads `pw-dump` every second while open |
| Devices on &lt;connection&gt; | the editor's network button on an active connection. A count line ("6 devices on 192.168.178.0/24 (wlan0)"), then a table: name (reverse DNS through `getent hosts`, with "router" or "this computer" under it), address (IPv4, then the IPv6 addresses dimmed under it, global first), MAC address, manufacturer (the longest matching prefix in nmap's `nmap-mac-prefixes`, found beside the `nmap` on PATH; "Private address" for a locally administered MAC; empty without nmap) and the ping in ms. Names and addresses are links. The scan sends one UDP packet to the discard port of every address in the subnet (a /22 at most, around our address), which makes the kernel ARP for each, pings the IPv6 all-nodes address, waits 2 s and reads `ip -j neigh show dev <device>`, dropping failed and incomplete entries and joining the rest by MAC. Nothing waits for the whole: this computer shows at once, the neighbour table is read every 200 ms for those 2 s and each device shows the moment it answers, and each device's name lookup and ping (`ping -c 1 -W 1`) start on their own threads as soon as it shows and fill in its row when they answer. A spinner sits beside the count until the last answer is in. Needs no root. Runs on its own thread once per opening; Scan again starts over |
| Wi-Fi channels | the radar button. One chart per band (2.4, 5, 6 GHz) that has networks: each access point is a hill centred on its channel, 20 MHz wide, as high as its signal (-100 to -30 dBm), coloured per SSID; the connected one has a thicker line. Usable channels are spaced evenly, so the 5 GHz gaps take no room. Each channel tick has its number and, below it, its centre frequency in MHz; a row drops the ticks it has no room for. Names crowding one channel stack downwards and those that don't fit are dropped; hovering a channel lists all its networks in a table (name, channel, dBm, BSSID). The data is the kernel's scan cache (`iw dev <device> scan dump`, no root needed), read every 5 s while the popup is open. iwd often scans one band at a time and the kernel then forgets the other, so an access point stays on the chart for 60 s after it was last seen. NetworkManager on iwd reports channel 2 and a fake BSSID for every network you aren't connected to, so nmcli is only the fallback: when `iw` fails, the chart shows nmcli's data under a warning that says so |

A link opens `http://<address>/` (`[…]` around IPv6) or `http://<name>/` in the
default browser through `xdg-open`, in the info colour, underlined on hover.
Link-local IPv6 addresses are no links, since browsers take no zone. Links are
for the mouse; the keyboard cursor skips them. A failure shows below the link.

## Keyboard

Navigation keys are taken out of egui's input before egui sees them
(`form::take_keys`), and `Form` moves the focus itself, so egui's own focus
handling never fights with ours.

| Key | Effect |
|---|---|
| Tab / Shift+Tab | next or previous widget, wrapping around |
| ↑ / ↓ | move inside a list or open select; elsewhere, previous or next widget |
| ← / → | on tabs, switch tab; elsewhere, previous or next widget, except inside a text field |
| PgUp / PgDn / Home / End | jump inside lists |
| ← / → on a slider | 5 % down or up (Home and End: 0 and 100 %) |
| Enter / Space | press a button, toggle a checkbox, open a select, flip the selected row's switch |
| Esc | close a popup or prompt, otherwise go back to the list, otherwise quit (also from the menu) |

A text field keeps ←, →, Home, End and Space for editing. Keys pressed with
Ctrl, Alt or Cmd go to egui untouched, so copy and paste work.

The focused widget has an accent outline. The list, and the editor pane while any of its fields or buttons has the focus, get an accent border. The selected row in a focused list
is filled with the accent colour and uses dark text.

## Theme

The default is Gruvbox Dark Hard. It is defined in `theme.rs`, and the same
colours are shipped in `themes/Gruvbox Dark Hard.json`.

| Role | Colour | Role | Colour |
|---|---|---|---|
| window | `#1d2021` | text | `#ebdbb2` |
| panel | `#282828` | secondary | `#bdae93` |
| surface | `#3c3836` | dim | `#928374` |
| surface_hover / outline | `#504945` | accent | `#fabd2f` |
| surface_active | `#665c54` | accent_hover | `#d79921` |
| danger | `#fb4934` | success | `#b8bb26` |
| warning | `#fe8019` | info (list headers) | `#83a598` |

To use another theme, put a fastframe palette file at
`$XDG_CONFIG_HOME/settings-dialog/theme.json`. Fonts and text rendering come from
`fastframe-fonts` and `fastframe-text`.

## NixOS modules

The save button on the Sound, Bluetooth and Networks pages writes
`/etc/nixos/settings-dialog/<page>.nix` (`SETTINGS_DIALOG_NIX_DIR` moves the directory)
together with a `default.nix` that imports the files that exist, so one line in
`configuration.nix` picks all of them up:

```nix
imports = [ ./settings-dialog ];
```

Nothing happens until the next `nixos-rebuild`, and nothing is written unless the
button is pressed. A module never carries a secret: no Wi-Fi password, no static
address. What each one holds:

| File | Holds | Leaves out |
| --- | --- | --- |
| sound.nix | the default output and input, pinned with `priority.session = 3000` in `services.pipewire.wireplumber.extraConfig` | volumes, mutes, links; a node that is neither an ALSA nor a Bluetooth device, which gets a comment instead because WirePlumber has no monitor rule for it |
| bluetooth.nix | `hardware.bluetooth.powerOnBoot` and `settings.Policy.AutoEnable`, both following the switch | per-device pairings and trust, which live in BlueZ's own state |
| network.nix | one `networking.networkmanager.ensureProfiles.profiles` entry per profile that needs no secret: name, type, autoconnect, SSID and mode, with `ipv4.method`/`ipv6.method` of `auto` | a profile with a password or an address of its own, each named in a comment saying why. `ensureProfiles` overwrites the keyfile it names, so writing such a profile without its secrets would replace a working connection with a broken one |

A written file that nothing imports changes nothing, so the page says so
(NIX-4) when no `.nix` file under `/etc/nixos` mentions `settings-dialog`.

## Architecture

```
main.rs     logging, fonts, theme, icons, eframe window
app.rs      the Settings menu, the current page + current prompt; routes keys; applies Actions; handles Replies of both backends
screens/    networks (list + editor pane), editor (the pane), bluetooth (device list + device pane), sound (device list + device pane), displays (arrange map band, monitor list and monitor pane), graph (the node graph canvas), overlay (prompts), radar (the channel chart); each `show` draws and returns an Action
form.rs     keyboard/focus model and the widgets (button, check, switch, slider, tabs, entry, select, list with row switches, icon buttons), icons
nm.rs       Request → nmcli (or iw, for the radar) → Reply on one worker thread; the finder's Wi-Fi signal (iw link / scan dump) and hotspot devices (iw station dump); data types and parsing
bt.rs       Request → busctl / bluetoothctl → Reply on its own worker thread, which also owns the discovery child process; the finder's Bluetooth signal (hcitool / busctl); BlueZ types and parsing
hypr.rs     Request → hyprctl → Reply on its own worker thread; the monitor list, and writing and parsing the layout file
nix.rs      builds and writes the NixOS modules the Sound, Bluetooth and Network pages save
tablet.rs   the tablet screen: the headless output and the Sunshine child, whose stdout a reader thread turns into streams, PIN requests and errors
pw.rs       Request → pw-dump / pw-metadata / wpctl / pw-cli / pw-link → Reply on its own worker thread; PipeWire graph types and parsing
lan.rs      the devices on a network: knocks on the subnet, reads the neighbour table, then looks up names and pings, on a thread per scan
finder.rs   find by sound: reads a Bluetooth, Wi-Fi or hotspot device's signal and beeps into pw-cat on its own thread
theme.rs    Palette, Gruvbox defaults, user theme loading
```

- `App` owns all pages and an optional prompt. Screens return `Action::{Pop, Quit, Send, Bluetooth, Overlay}`; sound requests are pushed to `Cx.sound` and sent after the frame. The editor shares the Networks `Form`, so Tab moves from the list into the editor fields.
- `Backend` is a single `nmcli` worker thread with two mpsc channels. Requests run one at a time, in order. After every change the worker sends a fresh `Snapshot`, so screens only read `Snapshot` and never patch their own state.
- It is a Wayland app (eframe glow + winit; X11 is only a fallback). Every frame calls winit's `pre_present_notify`, so the next redraw waits for the compositor's frame callback. Without it, a repaint while the window is on a hidden workspace blocks in `eglSwapBuffers` (vsync waits for a frame callback that never comes), pings go unanswered and Hyprland reports the app as not responding. Hidden windows therefore don't paint at all.
- "Use now" writes WirePlumber's `default.audio.sink|source` metadata directly; it holds until a device is added or removed, when WirePlumber picks again from the saved default. "Make default" is `wpctl set-default`, which saves `default.configured.*` and so also switches now.
- Volumes are on wpctl's cubic scale (what pavucontrol shows); `pw-dump` gives the linear `channelVolumes`, so the shown volume is the cube root of the loudest channel. `wpctl set-volume` sets every channel to the same value, so volumes are set per channel with `pw-cli set-param` the way wpctl does it underneath: on a device node through its device's `Route` (with `save: true`, so WirePlumber keeps it, as wpctl does), on any other node through its `Props`. pw-cli exits 0 on errors, so its stderr is checked. A dragged slider sends a request every frame; the worker keeps only the newest volume per node from what queued up while pw-cli ran, and the screen shows the value it set for a second so a lagging snapshot doesn't make the slider jump back.
- Levels come from one `pw-record` per open graph (`pw::Meter`): `--target 0` so nothing links it, raw f32 at 12 kHz, one AUX channel per linked audio output port, each linked with `pw-link`; a reader thread sends the peak per channel every 50 ms. It is passive (it never wakes a device), only meters ports that already have links, is restarted when that set changes and stopped when the graph closes. The snapshot hides nodes named `settings-dialog-meter-*`, their ports and links.
- The finder (`finder::Finder`, for Bluetooth devices and Wi-Fi networks) is one thread: it reads the signal, writes one beep to a `pw-cat --raw` stream, and repeats, so the playback paces it; it writes 20 ms at a time and checks its stop flag in between, so stopping kills pw-cat at once. A connected device's signal comes from `hcitool rssi` (BlueZ doesn't update `RSSI` while connected; the HCI Read RSSI command needs no root). That value is how far the link is from the controller's ideal receive range, 0 inside it, not dBm, so it is mapped from -20…+5 onto the pitch range. Otherwise it is BlueZ's `RSSI` property, which discovery keeps fresh while the page is open. The stream has `state.restore-props = false`, so WirePlumber saves nothing for it.
- The node graph is a `form.custom` widget: it gets the navigation keys while focused and Tab still leaves it.
- The Screens page talks to Hyprland only. The monitors come from `hyprctl -j monitors all`; the live refresh rate is rounded onto the monitor's own mode list, so a monitor doesn't look edited the moment the page draws it. A save applies the layout with one `hyprctl eval` of `hl.monitor({ … })` calls (`hyprctl keyword` does not work with the Lua config parser) and writes the same calls to `monitors.lua` beside the Hyprland config, which `hyprland.lua` loads with `dofile`, so the layout survives a restart. Apart from that file the app only writes the NixOS modules (below). `hyprctl eval` exits 0 even when Hyprland rejects the chunk, so its answer is checked for `ok`. Monitors in the file that aren't plugged in right now are parsed back out of it and written again, so an unplugged monitor keeps its settings.
- `nmcli` and `iw` run with `LC_ALL=C` and terse output (`-t`). Passwords are only passed when the user changed them, and they are redacted in logs.

## Errors and logging

The log is `$XDG_STATE_HOME/settings-dialog/settings-dialog.log`. The previous run's
log is kept beside it, and panics go to `panics.log`. The default filter is
`warn,settings_dialog=debug`; `RUST_LOG` overrides it. Every `nmcli`, `iw`, `ip`, `getent`, `ping`, `busctl`, `bluetoothctl`, `hyprctl`, `sunshine` and PipeWire tool call is
logged with its arguments (secrets redacted), exit status and duration.

Each error site has its own fixed code. The code appears in the UI as
`[CODE] message` and in the log line. Codes are never reused or renumbered.

| Code | Site |
|---|---|
| NM-101 | refresh the snapshot |
| NM-102 | load a connection for editing |
| NM-103 | save a connection |
| NM-104 | delete a connection |
| NM-105 | activate a saved connection |
| NM-106 | deactivate a connection |
| NM-107 | connect to a new Wi-Fi network |
| NM-108 | *retired* (set the hostname) |
| NM-109 | switch Wi-Fi on or off |
| NM-110 | refresh after a change |
| NM-111 | list the channels with nmcli (radar fallback) |
| IW-1 | read the scan results with `iw` (radar) |
| IW-2 | read the Wi-Fi link with `iw` (connection details; logged only, nmcli takes over) |
| NM-112 | read the connection details |
| NM-113 | ask for a rescan while finding a network (logged only) |
| IW-3 | read the Wi-Fi link while finding a network |
| IW-4 | read the scan results while finding a network |
| IW-5 | list the Wi-Fi interfaces for hotspot devices (logged only) |
| IW-6 | list a hotspot's devices (logged only) |
| IW-7 | read a hotspot device's signal while finding it |
| NET-1 | ping the hotspot device being found (logged only) |
| LAN-1 | read the device's addresses (devices on a network) |
| LAN-2 | read the neighbour table (devices on a network) |
| LAN-3 | knock on the subnet (logged only; the table then has only what the kernel already knew) |
| LAN-4 | read the default route (logged only; no device is marked as the router by IPv4) |
| BT-1 | read the devices from BlueZ |
| BT-2 | switch Bluetooth on or off |
| BT-3 | start discovery |
| BT-4 | pair a device |
| BT-5 | connect a device |
| BT-6 | disconnect a device |
| BT-7 | trust or untrust a device |
| BT-8 | block or unblock a device |
| BT-9 | remove a device |
| BT-10 | refresh after a Bluetooth change |
| BT-11 | start the find sound (pw-cat; Wi-Fi's finder too) |
| BT-12 | play the find sound (pw-cat stopped; Wi-Fi's finder too) |
| BT-13 | find the device's sink for the find sound (logged only; it then plays on the default output) |
| SND-1 | read the PipeWire graph |
| SND-2 | use a device now |
| SND-3 | make a device the default |
| SND-4 | link ports |
| SND-5 | unlink ports |
| SND-6 | refresh after a sound change |
| SND-7 | set the volume |
| SND-8 | mute or unmute |
| SND-9 | start the level meter |
| SND-10 | link the level meter |
| SND-11 | switch a Bluetooth codec |
| HY-1 | read the monitors after a change |
| HY-2 | read the monitors |
| HY-3 | apply the layout (hyprctl failed) |
| HY-4 | apply the layout (Hyprland rejected it) |
| HY-5 | find the Hyprland config |
| HY-6 | write the layout file |
| HY-7 | saving with every screen off |
| HY-8 | focus the settings window (hyprctl failed) |
| HY-9 | focus the settings window (Hyprland rejected it) |
| NIX-1 | make the NixOS module directory |
| NIX-2 | write the NixOS module |
| NIX-3 | write the NixOS module list |
| NIX-4 | /etc/nixos does not import the module directory |
| NM-114 | load a connection for the NixOS module |
| NM-115 | refresh before a rescan |
| TAB-1 | list the monitors before making the tablet output |
| TAB-2 | create the tablet output |
| TAB-3 | apply the tablet output's saved settings, or its size (logged only) |
| TAB-5 | write the Sunshine config |
| TAB-6 | start Sunshine (also: Sunshine is not installed) |
| TAB-7 | read this computer's addresses (logged only) |
| TAB-8 | Sunshine stopped |
| TAB-9 | a fatal line from Sunshine |
| TAB-10 | remove the tablet output (logged only) |
| TAB-11 | pass the PIN to Sunshine |
| TAB-12 | stop the tablet's output copying a screen before removing it (logged only) |
| TAB-13 | forget a Moonlight's repeated pairings in Sunshine's state (logged only) |
| URL-1 | open an address in the browser (`xdg-open`; shown below the link) |
| EDIT-1 | profile name empty |
| EDIT-2 | SSID empty |
| EDIT-3 | WPA password not 8–63 characters |

## Development

```
direnv allow           # once; or `nix develop` — Rust 1.98, runtime libraries, iw, watchexec
watch                  # rebuild and restart the app on every change in src/, themes/ or Cargo.toml
cargo test
cargo clippy --all-targets
```
