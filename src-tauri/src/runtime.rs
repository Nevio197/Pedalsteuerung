use crate::actions;
use crate::gesture::{Dispatch, Gestures};
use crate::hardware::{AccessMode, HardwareEvent, HardwareReader};
use crate::model::{self, Action, ActionPhase, Config};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

const TICK: Duration = Duration::from_millis(10);
const ACTION_QUEUE_LIMIT: usize = 8;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionStatus {
    pub label: String,
    pub state: &'static str,
    pub message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub config: Config,
    pub connected: bool,
    pub connection_message: String,
    pub paused: bool,
    pub preview: bool,
    pub manual_profile_id: Option<String>,
    pub active_profile_id: String,
    pub frontmost_app: Option<String>,
    pub buttons: [bool; 3],
    pub raw_buttons: [bool; 3],
    pub input_revision: u64,
    pub last_action: Option<ActionStatus>,
    pub startup_message: Option<String>,
}

type Reply = oneshot::Sender<Result<AppSnapshot, String>>;

struct PublishedState {
    snapshot: AppSnapshot,
    event_queued: bool,
}

pub enum Message {
    Save(Config, Reply),
    Pause(bool, Reply),
    TogglePause,
    Preview(bool, Reply),
    Pin(Option<String>, Reply),
    Test(Action, Reply),
    Foreground(Option<String>),
    Hardware {
        generation: u64,
        event: HardwareEvent,
        at: Instant,
    },
    ActionUpdate {
        id: u64,
        token: Arc<AtomicBool>,
        status: ActionStatus,
    },
    Stop(SyncSender<()>),
}

pub struct Controller {
    tx: Sender<Message>,
    published: Arc<Mutex<PublishedState>>,
}

impl Controller {
    pub fn start(app: AppHandle, path: PathBuf, legacy_path: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        let control_tx = tx.clone();
        let action_tx = tx.clone();
        let (jobs, queue) = mpsc::sync_channel(ACTION_QUEUE_LIMIT);
        let runtime = Runtime::new(app, path, legacy_path.as_deref(), control_tx, jobs);
        let published = Arc::clone(&runtime.published);
        thread::spawn(move || run_actions(queue, action_tx));
        thread::spawn(move || runtime.run(rx));
        Self { tx, published }
    }

    pub fn sender(&self) -> Sender<Message> {
        self.tx.clone()
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.published.lock().unwrap().snapshot.clone()
    }

    pub async fn request(
        &self,
        message: impl FnOnce(Reply) -> Message,
    ) -> Result<AppSnapshot, String> {
        let (reply, result) = oneshot::channel();
        let unavailable = || "Pedalsteuerung konnte die Anfrage nicht abschließen.".to_string();
        self.tx.send(message(reply)).map_err(|_| unavailable())?;
        tokio::time::timeout(Duration::from_secs(4), result)
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
    }

    pub fn stop(&self) {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.tx.send(Message::Stop(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_millis(750));
        }
    }
}

struct Job {
    id: u64,
    action: Action,
    token: Arc<AtomicBool>,
}

fn run_actions(queue: Receiver<Job>, messages: Sender<Message>) {
    while let Ok(job) = queue.recv() {
        if job.token.load(Ordering::Acquire) {
            continue;
        }
        let report = |state, message: String| {
            let _ = messages.send(Message::ActionUpdate {
                id: job.id,
                token: job.token.clone(),
                status: ActionStatus {
                    label: job.action.label().into(),
                    state,
                    message,
                },
            });
        };
        report("running", "Aktion läuft …".into());
        let result = match &job.action {
            Action::Macro { steps } => run_steps(steps, &job.token),
            action => actions::run_action(action, ActionPhase::Pulse),
        };
        if !job.token.load(Ordering::Acquire) {
            match result {
                Ok(()) => report("success", "Aktion ausgeführt.".into()),
                Err(message) => report("error", message),
            }
        }
    }
}

fn run_steps(steps: &[Action], token: &AtomicBool) -> Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        if token.load(Ordering::Acquire) {
            break;
        }
        if let Action::Pause { milliseconds } = step {
            let end = Instant::now() + Duration::from_millis(u64::from(*milliseconds));
            while !token.load(Ordering::Acquire) {
                let remaining = end.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                thread::sleep(remaining.min(TICK));
            }
        } else {
            actions::run_action(step, ActionPhase::Pulse)
                .map_err(|error| format!("Schritt {} · {}: {error}", index + 1, step.label()))?;
        }
    }
    Ok(())
}

struct Runtime {
    app: AppHandle,
    path: PathBuf,
    snapshot: AppSnapshot,
    published: Arc<Mutex<PublishedState>>,
    gestures: Gestures,
    held: [Option<Action>; 3],
    hardware: Option<HardwareReader>,
    hardware_generation: u64,
    messages: Sender<Message>,
    jobs: SyncSender<Job>,
    token: Arc<AtomicBool>,
    latest_action_id: u64,
}

impl Runtime {
    fn new(
        app: AppHandle,
        path: PathBuf,
        legacy_path: Option<&Path>,
        messages: Sender<Message>,
        jobs: SyncSender<Job>,
    ) -> Self {
        let initial = model::load_config(&path, legacy_path);
        let active_profile_id = initial
            .config
            .active_profile(initial.manual_profile_id.as_deref(), None)
            .id
            .clone();
        let snapshot = AppSnapshot {
            config: initial.config,
            connected: false,
            connection_message: "Suche Philips-Pedal …".into(),
            paused: initial.paused,
            preview: false,
            manual_profile_id: initial.manual_profile_id,
            active_profile_id,
            frontmost_app: None,
            buttons: [false; 3],
            raw_buttons: [false; 3],
            input_revision: 0,
            last_action: None,
            startup_message: initial.message,
        };
        let published = Arc::new(Mutex::new(PublishedState {
            snapshot: snapshot.clone(),
            event_queued: false,
        }));
        Self {
            app,
            path,
            snapshot,
            published,
            gestures: Gestures::default(),
            held: Default::default(),
            hardware: None,
            hardware_generation: 0,
            messages,
            jobs,
            token: Arc::new(AtomicBool::new(false)),
            latest_action_id: 0,
        }
    }

    fn run(mut self, rx: Receiver<Message>) {
        self.start_hardware();
        self.emit();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Message::Stop(reply)) => {
                    self.reset_actions();
                    self.stop_hardware();
                    let _ = reply.send(());
                    break;
                }
                Ok(message) => self.handle(message),
                Err(RecvTimeoutError::Timeout) => self.tick(Instant::now()),
                Err(RecvTimeoutError::Disconnected) => {
                    self.reset_actions();
                    self.stop_hardware();
                    break;
                }
            }
        }
    }

    fn handle(&mut self, message: Message) {
        let result = match message {
            Message::Save(config, reply) => (reply, self.save(config)),
            Message::Pause(paused, reply) => (reply, self.set_paused(paused)),
            Message::TogglePause => {
                if let Err(message) = self.set_paused(!self.snapshot.paused) {
                    self.set_action("Pedal", "error", message);
                }
                self.emit();
                return;
            }
            Message::Preview(preview, reply) => {
                if self.snapshot.preview != preview {
                    self.reset_actions();
                    self.snapshot.preview = preview;
                }
                (reply, Ok(()))
            }
            Message::Pin(id, reply) => (reply, self.pin(id)),
            Message::Test(action, reply) => {
                let result = if self.snapshot.preview || self.snapshot.paused {
                    Err("Beende die Vorschau oder hebe die Pause für den Aktionstest auf.".into())
                } else {
                    action
                        .validate(false, false)
                        .and_then(|_| self.enqueue(action))
                };
                (reply, result)
            }
            Message::Foreground(bundle) => {
                if self.snapshot.frontmost_app != bundle {
                    self.snapshot.frontmost_app = bundle;
                    self.refresh_profile();
                    self.emit();
                }
                return;
            }
            Message::Hardware {
                generation,
                event,
                at,
            } => {
                if generation == self.hardware_generation {
                    self.hardware_event(event, at);
                }
                return;
            }
            Message::ActionUpdate { id, token, status } => {
                if id == self.latest_action_id && !token.load(Ordering::Acquire) {
                    self.snapshot.last_action = Some(status);
                    self.emit();
                }
                return;
            }
            Message::Stop(_) => unreachable!(),
        };
        let (reply, result) = result;
        if result.is_ok() {
            self.emit();
        }
        let _ = reply.send(result.map(|()| self.snapshot.clone()));
    }

    fn emit(&self) {
        let mut state = self.published.lock().unwrap();
        state.snapshot = self.snapshot.clone();
        if state.event_queued {
            return;
        }
        state.event_queued = true;
        drop(state);

        let app = self.app.clone();
        let published = Arc::clone(&self.published);
        if self
            .app
            .run_on_main_thread(move || {
                let snapshot = {
                    let mut state = published.lock().unwrap();
                    state.event_queued = false;
                    state.snapshot.clone()
                };
                let _ = app.emit("pedal-state", snapshot);
            })
            .is_err()
        {
            self.published.lock().unwrap().event_queued = false;
        }
    }

    fn save(&mut self, config: Config) -> Result<(), String> {
        let manual = self
            .snapshot
            .manual_profile_id
            .clone()
            .filter(|id| config.profiles.iter().any(|profile| profile.id == *id));
        model::save_config(&self.path, &config, self.snapshot.paused, manual.as_deref())?;
        let mode_changed = self.snapshot.config.exclusive != config.exclusive;
        if self.snapshot.config != config {
            self.reset_actions();
        }
        self.snapshot.config = config;
        self.snapshot.manual_profile_id = manual;
        self.snapshot.startup_message = None;
        self.remap_buttons();
        self.refresh_profile();
        if mode_changed {
            self.snapshot.connected = false;
            self.snapshot.connection_message = "Pedal wird neu verbunden …".into();
            self.clear_buttons();
            self.start_hardware();
        }
        Ok(())
    }

    fn pin(&mut self, id: Option<String>) -> Result<(), String> {
        if id.as_ref().is_some_and(|id| {
            !self
                .snapshot
                .config
                .profiles
                .iter()
                .any(|profile| profile.id == *id)
        }) {
            return Err("Das gewählte Profil existiert nicht mehr.".into());
        }
        if self.snapshot.manual_profile_id != id {
            model::save_config(
                &self.path,
                &self.snapshot.config,
                self.snapshot.paused,
                id.as_deref(),
            )?;
            self.snapshot.manual_profile_id = id;
            self.snapshot.startup_message = None;
            self.refresh_profile();
        }
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), String> {
        if self.snapshot.paused != paused {
            if paused {
                self.reset_actions();
                self.snapshot.paused = true;
            }
            model::save_config(
                &self.path,
                &self.snapshot.config,
                paused,
                self.snapshot.manual_profile_id.as_deref(),
            )?;
            self.snapshot.paused = paused;
            self.snapshot.startup_message = None;
        }
        Ok(())
    }

    fn refresh_profile(&mut self) {
        let active = self.snapshot.config.active_profile(
            self.snapshot.manual_profile_id.as_deref(),
            self.snapshot.frontmost_app.as_deref(),
        );
        if self.snapshot.active_profile_id != active.id {
            self.snapshot.active_profile_id = active.id.clone();
            self.reset_actions();
        }
    }

    fn start_hardware(&mut self) {
        self.stop_hardware();
        self.hardware_generation += 1;
        let generation = self.hardware_generation;
        let mode = if self.snapshot.config.exclusive {
            AccessMode::Exclusive
        } else {
            AccessMode::Shared
        };
        let tx = self.messages.clone();
        self.hardware = Some(HardwareReader::spawn(mode, move |event| {
            let _ = tx.send(Message::Hardware {
                generation,
                event,
                at: Instant::now(),
            });
        }));
    }

    fn stop_hardware(&mut self) {
        if let Some(mut reader) = self.hardware.take() {
            reader.stop();
        }
    }

    fn hardware_event(&mut self, event: HardwareEvent, at: Instant) {
        match event {
            HardwareEvent::Connected => {
                self.snapshot.connected = true;
                self.snapshot.connection_message = if self.snapshot.config.exclusive {
                    "Philips-Pedal exklusiv verbunden"
                } else {
                    "Philips-Pedal im gemeinsamen Modus verbunden"
                }
                .into();
            }
            HardwareEvent::Disconnected { reason } => {
                self.snapshot.connected = false;
                self.snapshot.connection_message = reason;
                self.clear_buttons();
                self.reset_actions();
            }
            HardwareEvent::Button { button, pressed } => {
                if !self.snapshot.connected
                    || !(1..=3).contains(&button)
                    || self.snapshot.raw_buttons[usize::from(button) - 1] == pressed
                {
                    return;
                }
                self.snapshot.input_revision += 1;
                self.snapshot.raw_buttons[usize::from(button) - 1] = pressed;
                self.remap_buttons();
                if self.gestures_enabled() {
                    let position = self
                        .snapshot
                        .config
                        .button_map
                        .iter()
                        .position(|id| *id == button)
                        .expect("validated button map");
                    let (hold, double) = self.timings();
                    let profile = self.snapshot.config.active_profile(
                        self.snapshot.manual_profile_id.as_deref(),
                        self.snapshot.frontmost_app.as_deref(),
                    );
                    let dispatch = self.gestures.input(
                        position,
                        pressed,
                        at,
                        &profile.buttons[position],
                        hold,
                        double,
                    );
                    self.dispatch(dispatch);
                }
            }
        }
        self.emit();
    }

    fn tick(&mut self, now: Instant) {
        if !self.gestures_enabled() {
            return;
        }
        let (hold, double) = self.timings();
        let profile = self.snapshot.config.active_profile(
            self.snapshot.manual_profile_id.as_deref(),
            self.snapshot.frontmost_app.as_deref(),
        );
        let dispatch = self.gestures.tick(now, &profile.buttons, hold, double);
        if !dispatch.is_empty() {
            self.dispatch(dispatch);
            self.emit();
        }
    }

    fn gestures_enabled(&self) -> bool {
        self.snapshot.connected && !self.snapshot.paused && !self.snapshot.preview
    }

    fn timings(&self) -> (Duration, Duration) {
        (
            Duration::from_millis(self.snapshot.config.hold_ms),
            Duration::from_millis(self.snapshot.config.double_tap_ms),
        )
    }

    fn clear_buttons(&mut self) {
        self.snapshot.raw_buttons = [false; 3];
        self.remap_buttons();
    }

    fn remap_buttons(&mut self) {
        self.snapshot.buttons = self
            .snapshot
            .config
            .button_map
            .map(|button| self.snapshot.raw_buttons[usize::from(button) - 1]);
    }

    fn reset_actions(&mut self) {
        self.gestures.reset();
        self.token.store(true, Ordering::Release);
        self.token = Arc::new(AtomicBool::new(false));
        if self
            .snapshot
            .last_action
            .as_ref()
            .is_some_and(|status| matches!(status.state, "queued" | "running"))
        {
            self.set_action(
                "Pedal",
                "success",
                "Ausstehende Makros und Aktionen abgebrochen.".into(),
            );
        }
        for position in 0..self.held.len() {
            self.release_hold(position);
        }
    }

    fn enqueue(&mut self, action: Action) -> Result<(), String> {
        let id = self.latest_action_id + 1;
        let label = action.label().to_string();
        match self.jobs.try_send(Job {
            id,
            action,
            token: self.token.clone(),
        }) {
            Ok(()) => {
                self.latest_action_id = id;
                self.snapshot.last_action = Some(ActionStatus {
                    label,
                    state: "queued",
                    message: "Aktion wartet …".into(),
                });
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                Err("Zu viele Aktionen warten. Versuche es gleich noch einmal.".into())
            }
            Err(TrySendError::Disconnected(_)) => {
                Err("Die Aktionsausführung ist nicht verfügbar.".into())
            }
        }
    }

    fn dispatch(&mut self, dispatch: Vec<Dispatch>) {
        for event in dispatch {
            match event.phase {
                ActionPhase::Pulse => {
                    let label = event.action.label().to_string();
                    if let Err(message) = self.enqueue(event.action) {
                        self.set_action(&label, "error", message);
                    }
                }
                ActionPhase::Press => self.press_hold(event.position, event.action),
                ActionPhase::Release => self.release_hold(event.position),
            }
        }
    }

    fn press_hold(&mut self, position: usize, action: Action) {
        if self.held.iter().any(|held| held.as_ref() == Some(&action)) {
            self.held[position] = Some(action);
            return;
        }
        match actions::run_action(&action, ActionPhase::Press) {
            Ok(()) => {
                self.set_action(
                    action.label(),
                    "success",
                    "Tastenkürzel wird gehalten.".into(),
                );
                self.held[position] = Some(action);
            }
            Err(message) => self.set_action(action.label(), "error", message),
        }
    }

    fn release_hold(&mut self, position: usize) {
        let Some(action) = self.held[position].take() else {
            return;
        };
        if self.held.iter().any(|held| held.as_ref() == Some(&action)) {
            return;
        }
        match actions::run_action(&action, ActionPhase::Release) {
            Ok(()) => self.set_action(
                action.label(),
                "success",
                "Tastenkürzel losgelassen.".into(),
            ),
            Err(message) => self.set_action(action.label(), "error", message),
        }
    }

    fn set_action(&mut self, label: &str, state: &'static str, message: String) {
        self.latest_action_id += 1;
        self.snapshot.last_action = Some(ActionStatus {
            label: label.into(),
            state,
            message,
        });
    }
}
