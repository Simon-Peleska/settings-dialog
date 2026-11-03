pub mod bluetooth;
pub mod displays;
pub mod editor;
pub mod graph;
pub mod networks;
pub mod overlay;
pub mod radar;
pub mod sound;
pub mod tasks;

use crate::bt;
use crate::form::Key;
use crate::hypr;
use crate::nm::{Request, Snapshot};
use crate::pw;
use crate::theme::Palette;

pub struct Cx<'a> {
    pub ctx: &'a egui::Context,
    pub pal: &'a Palette,
    pub keys: &'a [Key],
    pub snap: &'a Snapshot,
    pub entry: bool,
    pub wifi: bool,
    pub send: Vec<Request>,
    /// Bluetooth requests that need no "please wait".
    pub bt: Vec<bt::Request>,
    pub sound: Vec<pw::Request>,
    pub screens: Vec<hypr::Request>,
}

pub enum Action {
    Pop,
    Quit,
    Send(Request),
    Bluetooth(bt::Request),
    Overlay(overlay::Overlay),
}

/// Shown after writing a module the NixOS config doesn't import, because then
/// the file changes nothing.
pub fn not_imported() -> Option<String> {
    (!crate::nix::imported()).then(|| {
        let message = format!(
            "[NIX-4] /etc/nixos does not import {}",
            crate::nix::dir().display()
        );
        log::warn!("{message}");
        message
    })
}
