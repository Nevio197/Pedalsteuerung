use serde::Serialize;
use std::{path::Path, sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error, Update, UpdaterExt};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const FEEDBACK_URL: &str = "https://github.com/Nevio197/Pedalsteuerung/issues/new/choose";

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Installing,
    Error,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(self, Self::Checking | Self::Installing)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    current_version: String,
    latest_version: Option<String>,
    state: Phase,
    message: String,
    checked_at: Option<String>,
    notes: Option<String>,
}

struct Inner {
    status: UpdateStatus,
    pending: Option<Update>,
}

impl Inner {
    fn complete_check(&mut self, result: tauri_plugin_updater::Result<Option<Update>>) {
        self.status.checked_at = OffsetDateTime::now_utc().format(&Rfc3339).ok();
        self.status.notes = None;
        self.status.latest_version = None;
        self.pending = None;
        match result {
            Ok(Some(mut update)) => {
                update.timeout = Some(DOWNLOAD_TIMEOUT);
                self.status.latest_version = Some(update.version.clone());
                self.status.notes = update
                    .body
                    .as_ref()
                    .map(|notes| notes.chars().take(24_000).collect());
                self.status.state = Phase::Available;
                self.status.message = format!("Version {} ist verfügbar.", update.version);
                self.pending = Some(update);
            }
            Ok(None) => {
                self.status.latest_version = Some(self.status.current_version.clone());
                self.status.state = Phase::UpToDate;
                self.status.message = "Pedalsteuerung ist aktuell.".into();
            }
            Err(error) => {
                self.status.state = Phase::Error;
                self.status.message = check_error(&error).into();
            }
        }
    }
}

pub struct Updates(Mutex<Inner>);

impl Updates {
    pub fn new(version: String) -> Self {
        Self(Mutex::new(Inner {
            status: UpdateStatus {
                current_version: version,
                latest_version: None,
                state: Phase::Idle,
                message: "Noch keine Update-Prüfung durchgeführt.".into(),
                checked_at: None,
                notes: None,
            },
            pending: None,
        }))
    }
}

fn publish(app: &AppHandle, status: &UpdateStatus) {
    let _ = app.emit("pedal-update", status);
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            check(&app).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

async fn check(app: &AppHandle) -> UpdateStatus {
    let state = app.state::<Updates>();
    {
        let mut data = state.0.lock().unwrap();
        if data.status.state.busy() {
            return data.status.clone();
        }
        data.status.state = Phase::Checking;
        data.status.message = "Suche nach Updates …".into();
        publish(app, &data.status);
    }

    let result = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    let mut data = state.0.lock().unwrap();
    data.complete_check(result);
    publish(app, &data.status);
    data.status.clone()
}

#[tauri::command]
pub fn get_update_status(updates: State<'_, Updates>) -> UpdateStatus {
    updates.0.lock().unwrap().status.clone()
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> UpdateStatus {
    check(&app).await
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<UpdateStatus, String> {
    let state = app.state::<Updates>();
    let update = {
        let mut data = state.0.lock().unwrap();
        if data.status.state.busy() {
            return Ok(data.status.clone());
        }
        let update = data
            .pending
            .clone()
            .ok_or("Suche zuerst nach einem verfügbaren Update.")?;
        data.status.state = Phase::Installing;
        data.status.message = "Update wird geladen und geprüft …".into();
        publish(&app, &data.status);
        update
    };

    if !inside_macos_app(&std::env::current_exe().unwrap_or_default()) {
        let mut data = state.0.lock().unwrap();
        data.status.state = Phase::Error;
        data.status.message =
            "Öffne die installierte Pedalsteuerung-App, um das Update einzuspielen.".into();
        publish(&app, &data.status);
        return Ok(data.status.clone());
    }

    let result = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => {
            {
                let mut data = state.0.lock().unwrap();
                data.status.message = "Update wird installiert …".into();
                publish(&app, &data.status);
            }
            match tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await {
                Ok(result) => result.map_err(|error| install_error(&error)),
                Err(_) => Err("Die Update-Installation konnte nicht abgeschlossen werden.".into()),
            }
        }
        Err(error) => Err(install_error(&error)),
    };

    let status = {
        let mut data = state.0.lock().unwrap();
        match &result {
            Ok(()) => {
                data.pending = None;
                data.status.message = "Update installiert. Pedalsteuerung startet neu …".into();
            }
            Err(message) => {
                data.status.state = Phase::Error;
                data.status.message = message.clone();
            }
        }
        publish(&app, &data.status);
        data.status.clone()
    };
    if result.is_ok() {
        // The normal exit handler releases the pedal and any held shortcut before relaunch.
        app.request_restart();
    }
    Ok(status)
}

fn inside_macos_app(executable: &Path) -> bool {
    executable.parent().is_some_and(|directory| {
        directory.file_name().is_some_and(|name| name == "MacOS")
            && directory.parent().is_some_and(|contents| {
                contents.file_name().is_some_and(|name| name == "Contents")
                    && contents
                        .parent()
                        .is_some_and(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
            })
    })
}

fn check_error(error: &Error) -> &'static str {
    match error {
        Error::TargetNotFound(_) | Error::TargetsNotFound(_) => {
            "Für diesen Mac ist kein passendes Update hinterlegt."
        }
        Error::ReleaseNotFound => {
            "Die Update-Informationen sind auf GitHub gerade nicht verfügbar."
        }
        _ => {
            "Die Update-Prüfung ist fehlgeschlagen. Prüfe die Internetverbindung und versuche es erneut."
        }
    }
}

fn install_error(error: &Error) -> String {
    match error {
        Error::Minisign(_) | Error::Base64(_) | Error::SignatureUtf8(_) | Error::SignedVersionMismatch { .. } | Error::MissingSignedVersion =>
            "Die Update-Signatur konnte nicht bestätigt werden. Die installierte App wurde nicht ersetzt.".into(),
        Error::PreviousAppNotRestored(path) => format!("Das Update konnte nicht abgeschlossen werden. Die bisherige App liegt hier: {}", path.display()),
        Error::Reqwest(_) | Error::Network(_) => "Das Update konnte nicht heruntergeladen werden. Versuche es erneut.".into(),
        Error::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied =>
            "Pedalsteuerung darf die App an diesem Ort nicht ersetzen. Verschiebe sie in einen beschreibbaren Programme-Ordner.".into(),
        _ => "Das Update konnte nicht installiert werden. Prüfe, ob die App in einem beschreibbaren Programme-Ordner liegt.".into(),
    }
}

#[tauri::command]
pub fn open_feedback() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if std::process::Command::new("/usr/bin/open")
            .arg(FEEDBACK_URL)
            .status()
            .is_ok_and(|status| status.success())
        {
            return Ok(());
        }
    }
    Err("Die GitHub-Feedbackseite konnte nicht geöffnet werden.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_install_is_limited_to_an_app_bundle() {
        assert!(inside_macos_app(Path::new(
            "/Applications/Pedalsteuerung 2.app/Contents/MacOS/pedalsteuerung"
        )));
        assert!(!inside_macos_app(Path::new(
            "/tmp/target/debug/pedalsteuerung"
        )));
    }

    #[test]
    fn startup_status_and_check_outcomes_serialize_for_the_frontend() {
        let updates = Updates::new("2.1.0".into());
        let mut data = updates.0.lock().unwrap();
        let status = serde_json::to_value(&data.status).unwrap();
        assert_eq!(status["state"], "idle");
        assert_eq!(status["currentVersion"], "2.1.0");
        assert!(status["latestVersion"].is_null());
        data.complete_check(Ok(None));
        let status = serde_json::to_value(&data.status).unwrap();
        assert_eq!(status["state"], "upToDate");
        assert_eq!(status["latestVersion"], "2.1.0");
        assert!(status["checkedAt"].as_str().unwrap().ends_with('Z'));
        data.complete_check(Err(Error::ReleaseNotFound));
        let status = serde_json::to_value(&data.status).unwrap();
        assert_eq!(status["state"], "error");
        assert!(status["latestVersion"].is_null());
    }
}
