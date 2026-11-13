use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    Shortcut {
        shortcut: String,
    },
    DiscordToggle {
        shortcut: String,
    },
    DiscordHold {
        shortcut: String,
    },
    Obs {
        port: u16,
        #[serde(default)]
        password: String,
        request_type: String,
        #[serde(default = "empty_object")]
        request_data: Value,
    },
    StreamDeck {
        port: u16,
        action_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_name: Option<String>,
    },
    Pause {
        milliseconds: u32,
    },
    Macro {
        steps: Vec<Action>,
    },
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionPhase {
    Pulse,
    Press,
    Release,
}

impl Action {
    pub fn label(&self) -> &str {
        match self {
            Self::Shortcut { shortcut } => shortcut,
            Self::DiscordToggle { .. } => "Discord · Mikrofon umschalten",
            Self::DiscordHold { .. } => "Discord · Gedrückt halten",
            Self::Obs { request_type, .. } => request_type,
            Self::StreamDeck { action_name, .. } => action_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Elgato Stream Deck"),
            Self::Pause { .. } => "Pause",
            Self::Macro { .. } => "Makro",
        }
    }

    pub fn validate(&self, hold_slot: bool, macro_step: bool) -> Result<(), String> {
        match self {
            Self::Shortcut { shortcut }
            | Self::DiscordToggle { shortcut }
            | Self::DiscordHold { shortcut } => {
                if matches!(self, Self::DiscordHold { .. }) && !hold_slot {
                    return Err("Discord · Gedrückt halten gehört in die Halten-Belegung.".into());
                }
                if shortcut.trim().is_empty() || shortcut.len() > 100 {
                    return Err("Gib ein Tastenkürzel ein.".into());
                }
            }
            Self::Obs {
                port,
                password,
                request_type,
                request_data,
            } => {
                if *port == 0
                    || password.len() > 1024
                    || request_type.is_empty()
                    || request_type.len() > 96
                    || !request_type
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    || !request_data.is_object()
                    || request_data.to_string().len() > 32_768
                {
                    return Err("Prüfe Port, Anfrage und Daten der OBS-Aktion.".into());
                }
            }
            Self::StreamDeck {
                port,
                action_id,
                action_name,
            } => {
                if *port == 0
                    || action_id.trim().is_empty()
                    || action_id.len() > 256
                    || action_name.as_ref().is_some_and(|name| name.len() > 500)
                {
                    return Err(
                        "Gib einen gültigen Stream-Deck-Port an und wähle eine Aktion.".into(),
                    );
                }
            }
            Self::Pause { milliseconds } => {
                if !macro_step || !(1..=10_000).contains(milliseconds) {
                    return Err(
                        "Eine Makropause muss zwischen 1 und 10.000 Millisekunden liegen.".into(),
                    );
                }
            }
            Self::Macro { steps } => {
                if macro_step || !(1..=24).contains(&steps.len()) {
                    return Err("Ein Makro benötigt 1 bis 24 Schritte. Verschachtelte Makros sind nicht möglich.".into());
                }
                let mut wait_ms = 0;
                for step in steps {
                    step.validate(false, true)?;
                    if let Self::Pause { milliseconds } = step {
                        wait_ms += milliseconds;
                    }
                }
                if wait_ms > 60_000 {
                    return Err(
                        "Die Pausen eines Makros dürfen insgesamt höchstens eine Minute dauern."
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bindings {
    pub tap: Option<Action>,
    pub hold: Option<Action>,
    pub double_tap: Option<Action>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub applications: Vec<String>,
    pub buttons: [Bindings; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u8,
    pub profiles: Vec<Profile>,
    pub default_profile_id: String,
    pub button_map: [u8; 3],
    pub hold_ms: u64,
    pub double_tap_ms: u64,
    pub exclusive: bool,
}

impl Default for Config {
    fn default() -> Self {
        let id = "default".to_string();
        Self {
            version: 2,
            profiles: vec![Profile {
                id: id.clone(),
                name: "Standard".into(),
                applications: vec![],
                buttons: Default::default(),
            }],
            default_profile_id: id,
            button_map: [1, 3, 2],
            hold_ms: 450,
            double_tap_ms: 280,
            exclusive: true,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 2 {
            return Err("Die Einstellungsversion wird nicht unterstützt.".into());
        }
        let mut buttons = self.button_map;
        buttons.sort_unstable();
        if buttons != [1, 2, 3] {
            return Err("Ordne jede der drei Pedaltasten genau einmal zu.".into());
        }
        if !(200..=2_000).contains(&self.hold_ms) || !(120..=800).contains(&self.double_tap_ms) {
            return Err("Halten: 200–2.000 ms. Doppeltippen: 120–800 ms.".into());
        }
        if !(1..=40).contains(&self.profiles.len()) {
            return Err("Speichere zwischen 1 und 40 Profile.".into());
        }

        let mut ids = HashSet::new();
        for profile in &self.profiles {
            if profile.id.trim().is_empty()
                || profile.id.len() > 80
                || !ids.insert(&profile.id)
                || profile.name.trim().is_empty()
                || profile.name.len() > 100
            {
                return Err("Jedes Profil braucht einen Namen und eine eigene ID.".into());
            }
            if profile.applications.len() > 30
                || profile.applications.iter().any(|app| {
                    let app = app.trim();
                    app.is_empty()
                        || app.len() > 255
                        || !app
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
                })
            {
                return Err("Prüfe die App-IDs im Profil.".into());
            }
            for button in &profile.buttons {
                for (action, hold_slot) in [
                    (&button.tap, false),
                    (&button.hold, true),
                    (&button.double_tap, false),
                ] {
                    if let Some(action) = action {
                        action.validate(hold_slot, false)?;
                    }
                }
            }
        }
        if !ids.contains(&self.default_profile_id) {
            return Err("Wähle ein vorhandenes Standardprofil.".into());
        }
        Ok(())
    }

    pub fn active_profile(&self, manual: Option<&str>, frontmost: Option<&str>) -> &Profile {
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| Some(profile.id.as_str()) == manual)
        {
            return profile;
        }
        if let Some(bundle_id) = frontmost {
            if let Some(profile) = self.profiles.iter().find(|profile| {
                profile
                    .applications
                    .iter()
                    .any(|app| app.trim().eq_ignore_ascii_case(bundle_id))
            }) {
                return profile;
            }
        }
        self.profiles
            .iter()
            .find(|profile| profile.id == self.default_profile_id)
            .unwrap_or(&self.profiles[0])
    }
}

#[derive(Deserialize)]
struct LegacyConfig {
    version: u8,
    mapping: Option<[u8; 3]>,
    enabled: bool,
    assignments: [LegacyAssignment; 3],
}

#[derive(Deserialize)]
struct LegacyAssignment {
    kind: String,
    #[serde(default)]
    shortcut: String,
    #[serde(default)]
    stream_id: String,
    #[serde(default)]
    stream_name: Option<String>,
    #[serde(default)]
    port: u16,
}

pub struct InitialConfig {
    pub config: Config,
    pub paused: bool,
    pub manual_profile_id: Option<String>,
    pub message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedConfig {
    #[serde(flatten)]
    config: Config,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    manual_profile_id: Option<String>,
}

pub fn load_config(path: &Path, legacy_path: Option<&Path>) -> InitialConfig {
    let loaded = match fs::read(path) {
        Ok(data) => serde_json::from_slice::<SavedConfig>(&data)
            .map_err(|_| {
                "Die gespeicherten Einstellungen konnten nicht gelesen werden.".to_string()
            })
            .and_then(|saved| {
                saved.config.validate()?;
                let manual = saved.manual_profile_id.filter(|id| {
                    saved
                        .config
                        .profiles
                        .iter()
                        .any(|profile| profile.id == *id)
                });
                Ok((saved.config, saved.paused, manual))
            }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match legacy_path.map(fs::read).transpose() {
                Ok(Some(data)) => {
                    import_legacy(&data).map(|(config, paused)| (config, paused, None))
                }
                Ok(None) => Ok((Config::default(), true, None)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Ok((Config::default(), true, None))
                }
                Err(error) => Err(format!(
                    "Die Einstellungen der ersten Version sind nicht lesbar. {error}"
                )),
            }
        }
        Err(error) => Err(format!("Die Einstellungen sind nicht lesbar. {error}")),
    };
    match loaded {
        Ok((config, paused, manual_profile_id)) => InitialConfig {
            config,
            paused,
            manual_profile_id,
            message: None,
        },
        Err(message) => InitialConfig {
            config: Config::default(),
            paused: true,
            manual_profile_id: None,
            message: Some(message),
        },
    }
}

fn import_legacy(data: &[u8]) -> Result<(Config, bool), String> {
    let legacy: LegacyConfig = serde_json::from_slice(data).map_err(|_| {
        "Die Einstellungen der ersten Version konnten nicht übernommen werden.".to_string()
    })?;
    if legacy.version != 1 {
        return Err("Das ältere Einstellungsformat wird nicht unterstützt.".into());
    }
    let mut config = Config::default();
    if let Some(mapping) = legacy.mapping {
        config.button_map = mapping;
    }
    for (button, assignment) in config.profiles[0]
        .buttons
        .iter_mut()
        .zip(legacy.assignments)
    {
        button.tap = match assignment.kind.as_str() {
            "none" => None,
            "shortcut" => Some(Action::Shortcut {
                shortcut: assignment.shortcut,
            }),
            "discord" => Some(Action::DiscordToggle {
                shortcut: assignment.shortcut,
            }),
            "stream_deck" => Some(Action::StreamDeck {
                port: assignment.port,
                action_id: assignment.stream_id,
                action_name: assignment.stream_name,
            }),
            _ => {
                return Err(
                    "Eine Belegung der ersten Version konnte nicht übernommen werden.".into(),
                );
            }
        };
    }
    config.validate()?;
    Ok((config, !legacy.enabled || legacy.mapping.is_none()))
}

pub fn save_config(
    path: &Path,
    config: &Config,
    paused: bool,
    manual_profile_id: Option<&str>,
) -> Result<(), String> {
    config.validate()?;
    write_config(path, config, paused, manual_profile_id)
        .map_err(|error| format!("Die Einstellungen konnten nicht gespeichert werden. {error}"))
}

fn write_config(
    path: &Path,
    config: &Config,
    paused: bool,
    manual_profile_id: Option<&str>,
) -> io::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Settings<'a> {
        #[serde(flatten)]
        config: &'a Config,
        paused: bool,
        manual_profile_id: Option<&'a str>,
    }
    let folder = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(folder)?;
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    serde_json::to_writer_pretty(
        file.as_file_mut(),
        &Settings {
            config,
            paused,
            manual_profile_id,
        },
    )?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_import_preserves_mapping_action_labels_and_pause_without_writing_original() {
        let folder = tempfile::tempdir().unwrap();
        let old = folder.path().join("v1.json");
        let new = folder.path().join("v2.json");
        let data = json!({
            "version": 1, "mapping": [3, 2, 1], "enabled": false, "assignments": [
                {"kind":"discord","shortcut":"Ctrl+Shift+F9"},
                {"kind":"none"},
                {"kind":"stream_deck","port":9090,"stream_id":"one","stream_name":"Licht"}
            ]
        });
        let original = serde_json::to_vec(&data).unwrap();
        fs::write(&old, &original).unwrap();
        let result = load_config(&new, Some(&old));
        assert_eq!(result.config.button_map, [3, 2, 1]);
        assert!(result.paused);
        assert_eq!(
            result.config.profiles[0].buttons[2]
                .tap
                .as_ref()
                .unwrap()
                .label(),
            "Licht"
        );
        assert_eq!(fs::read(&old).unwrap(), original);
        assert!(!new.exists());
        save_config(&new, &result.config, true, Some("default")).unwrap();
        let saved = load_config(&new, Some(&old));
        assert_eq!(saved.config, result.config);
        assert_eq!(saved.manual_profile_id.as_deref(), Some("default"));
        assert!(saved.paused);
    }

    #[test]
    fn invalid_candidate_cannot_replace_existing_config() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        let mut config = Config::default();
        save_config(&path, &config, false, None).unwrap();
        let previous = fs::read(&path).unwrap();
        config.button_map = [1, 1, 3];
        assert!(save_config(&path, &config, false, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
    }

    #[test]
    fn manual_profile_takes_priority_over_foreground_and_default() {
        let mut config = Config::default();
        let mut profile = config.profiles[0].clone();
        profile.id = "obs".into();
        profile
            .applications
            .push("com.obsproject.obs-studio".into());
        config.profiles.push(profile);
        assert_eq!(
            config
                .active_profile(None, Some("com.OBSProject.OBS-Studio"))
                .id,
            "obs"
        );
        assert_eq!(
            config
                .active_profile(Some("default"), Some("com.obsproject.obs-studio"))
                .id,
            "default"
        );
        assert_eq!(config.active_profile(None, None).id, "default");
    }
}
