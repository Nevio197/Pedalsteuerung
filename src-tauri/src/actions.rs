use crate::model::{Action, ActionPhase};
use serde::Serialize;

#[cfg(target_os = "macos")]
#[path = "actions_macos.rs"]
mod macos;
#[path = "actions_network.rs"]
mod network;

#[derive(Debug, Serialize)]
pub struct StreamDeckAction {
    pub id: String,
    pub name: String,
    pub description: String,
}

pub fn list_stream_deck_actions(port: u16) -> Result<Vec<StreamDeckAction>, String> {
    network::list_stream_deck_actions(port)
}

pub fn run_action(action: &Action, phase: ActionPhase) -> Result<(), String> {
    match action {
        Action::Shortcut { shortcut }
        | Action::DiscordToggle { shortcut }
        | Action::DiscordHold { shortcut } => keyboard(shortcut, phase),
        Action::Obs {
            port,
            password,
            request_type,
            request_data,
        } => network::run_obs(*port, password, request_type, request_data),
        Action::StreamDeck {
            port, action_id, ..
        } => network::run_stream_deck(*port, action_id),
        _ => Err("Diese Aktion muss als Teil eines Makros ausgeführt werden.".into()),
    }
}

#[cfg(target_os = "macos")]
pub fn frontmost_bundle_identifier() -> Option<String> {
    macos::frontmost_bundle_identifier()
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_bundle_identifier() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
pub fn shortcut_permission() -> bool {
    macos::shortcut_permission()
}

#[cfg(not(target_os = "macos"))]
pub fn shortcut_permission() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub fn request_shortcut_permission() -> bool {
    macos::request_shortcut_permission()
}

#[cfg(not(target_os = "macos"))]
pub fn request_shortcut_permission() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn keyboard(shortcut: &str, phase: ActionPhase) -> Result<(), String> {
    macos::keyboard(shortcut, phase)
}

#[cfg(not(target_os = "macos"))]
fn keyboard(_shortcut: &str, _phase: ActionPhase) -> Result<(), String> {
    Err("Tastenkürzel sind in dieser Version für macOS verfügbar.".into())
}
