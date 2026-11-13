"use strict";

const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];
const BRIDGE = window.__TAURI__;
const POSITIONS = ["Links", "Mitte", "Rechts"];
const GESTURES = { tap: "Tippen", hold: "Halten", doubleTap: "Doppeltipp" };
const TYPES = {
  none: "Keine Aktion", discordToggle: "Discord · Mikrofon umschalten",
  discordHold: "Discord · Push-to-Talk", shortcut: "Tastenkürzel",
  obs: "OBS Studio", streamDeck: "Elgato Stream Deck", macro: "Makro", pause: "Wartezeit",
};
const APP_NAMES = {
  "com.hnc.Discord": "Discord",
  "com.obsproject.obs-studio": "OBS Studio",
  "com.valvesoftware.steam": "Steam",
  "com.elgato.StreamDeck": "Stream Deck",
  "de.nevio.pedalsteuerung.v2": "Pedalsteuerung",
};
const STATUS = { queued: "Wartet", running: "Läuft", success: "Ausgeführt", error: "Fehler" };
const EASTER_SEQUENCE = [0, 1, 2, 0, 2, 1];
const EASTER_TIMEOUT = 8_000;
const EASTER_VIDEO = "./media/nevio-mcfly.mp4";
const EMPTY_PEDALS = () => POSITIONS.map(() => ({ tap: null, hold: null, doubleTap: null }));
const EMPTY = {
  config: {
    version: 2, profiles: [{ id: "default", name: "Standard", applications: [], buttons: EMPTY_PEDALS() }],
    defaultProfileId: "default", buttonMap: [1, 3, 2], holdMs: 450, doubleTapMs: 280, exclusive: true,
  },
  connected: false, connectionMessage: "", paused: false, preview: false,
  manualProfileId: null, activeProfileId: "default", frontmostApp: null,
  buttons: [false, false, false], rawButtons: [false, false, false], lastAction: null,
};

let snapshot = EMPTY;
let backendReady = false;
let view = "pedal";
let configKey = "";
let currentDialog = null;
let calibration = null;
let lastExternalApp = "";
let notificationTimer;
let timingEdit = false;
let mutationPending = false;
let startupMessage = "";
let updateStatus = null;
let updatePending = false;
let easter = { started: 0, steps: 0, down: null, ready: false };

function node(tag, attrs = {}, children = []) {
  const element = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") element.className = value;
    else if (key === "text") element.textContent = value;
    else if (key === "click") element.addEventListener("click", value);
    else if (typeof value === "boolean") {
      if (value) element.setAttribute(key, "");
    } else element.setAttribute(key, value);
  }
  element.append(...children);
  return element;
}

function button(text, click, variant = "quiet", attrs = {}) {
  return node("button", { type: "button", class: `button ${variant}`, text, click, ...attrs });
}

function option(value, text) {
  return node("option", { value, text });
}

function field(title, input, hint = "") {
  return node("label", { class: "field" }, [
    node("span", { text: title }),
    input,
    ...(hint ? [node("p", { text: hint })] : []),
  ]);
}

function input(type, value = "", attrs = {}) {
  const element = node("input", { type, ...attrs });
  element.value = value;
  return element;
}

function select(items, value) {
  const element = node("select");
  element.replaceChildren(...items.map(([id, label]) => option(id, label)));
  element.value = value;
  return element;
}

function message(error) {
  return typeof error === "string" ? error : error?.message ?? String(error);
}

function notify(text, error = false) {
  const box = $("#notification");
  box.textContent = text;
  box.classList.toggle("error", error);
  box.hidden = false;
  clearTimeout(notificationTimer);
  notificationTimer = setTimeout(() => { box.hidden = true; }, error ? 6500 : 2800);
}

async function invoke(command, args) {
  if (!BRIDGE?.core?.invoke) throw new Error("Die macOS-App ist für diese Funktion erforderlich.");
  return BRIDGE.core.invoke(command, args);
}

function activeProfile() {
  return snapshot.config.profiles.find(({ id }) => id === snapshot.activeProfileId)
    ?? snapshot.config.profiles.find(({ id }) => id === snapshot.config.defaultProfileId)
    ?? snapshot.config.profiles[0];
}

function displayApp(id) {
  return id ? (APP_NAMES[id] ?? id) : "Keine Vordergrund-App erkannt";
}

function actionName(action) {
  if (!action) return "Belegen";
  if (action.type === "shortcut") return action.shortcut;
  if (action.type === "macro") return `Makro · ${action.steps.length} Schritte`;
  if (action.type === "obs") return `OBS · ${OBS_PRESETS[action.requestType]?.label || action.requestType}`;
  if (action.type === "streamDeck") return action.actionName || "Elgato Stream Deck";
  return TYPES[action.type] ?? "Aktion";
}

function update(snapshotFromBackend) {
  if (!snapshotFromBackend?.config) return;
  snapshot = snapshotFromBackend;
  backendReady = true;
  if (snapshot.startupMessage && snapshot.startupMessage !== startupMessage) {
    startupMessage = snapshot.startupMessage;
    notify(startupMessage, true);
  }
  if (snapshot.frontmostApp && !snapshot.frontmostApp.startsWith("de.nevio.pedalsteuerung")) {
    lastExternalApp = snapshot.frontmostApp;
  }
  renderRuntime();
  const nextKey = JSON.stringify([snapshot.config, snapshot.activeProfileId, snapshot.manualProfileId]);
  if (configKey !== nextKey) {
    configKey = nextKey;
    renderAssignments();
    renderProfiles();
    renderSettings();
  }
  if (calibration) onCalibrationState();
  onEasterState();
}

function renderRuntime() {
  const connection = $("#connection");
  connection.classList.toggle("connected", snapshot.connected);
  connection.querySelector("strong").textContent = snapshot.connected ? "Philips-Pedal verbunden" : "Philips-Pedal getrennt";
  connection.title = snapshot.connectionMessage || "";
  $("#connection-mode").textContent = snapshot.connected
    ? (snapshot.config.exclusive ? "Exklusiver Zugriff" : "Geteilter Zugriff")
    : (snapshot.connectionMessage || "USB-Verbindung prüfen");
  const pause = $("#pause");
  pause.setAttribute("aria-pressed", snapshot.paused);
  pause.textContent = snapshot.paused ? "Aktionen fortsetzen" : "Aktionen pausieren";
  const preview = $("#preview");
  preview.setAttribute("aria-pressed", snapshot.preview);
  preview.textContent = snapshot.preview ? "Vorschau beenden" : "Vorschau";
  $("#frontmost").textContent = snapshot.manualProfileId
    ? "Profil fest gewählt"
    : `Automatisches Profil · ${displayApp(snapshot.frontmostApp)}`;
  $("#live-note").textContent = snapshot.preview
    ? "Vorschau aktiv. Pedalbewegungen lösen keine Aktionen aus."
    : snapshot.paused ? "Aktionen pausiert. Die Tastenanzeige bleibt aktiv."
      : "Drücke eine Taste, um ihren Status zu sehen.";
  snapshot.buttons.forEach((pressed, index) => {
    $(`[data-pedal="${index}"]`).classList.toggle("pressed", pressed);
    const label = $(`[data-pedal-label="${index}"]`);
    label.classList.toggle("pressed", pressed);
    label.setAttribute("aria-label", `${POSITIONS[index]}${pressed ? ", gedrückt" : ", losgelassen"}`);
  });
  const last = snapshot.lastAction;
  $("#activity").textContent = last ? [last.label, last.message].filter(Boolean).join(" · ") : "Noch keine Aktion ausgelöst.";
  const badge = $("#activity-badge");
  badge.hidden = !last;
  badge.textContent = last ? STATUS[last.state] ?? last.state : "";
  badge.classList.toggle("error", last?.state === "error");
  badge.classList.toggle("pending", ["queued", "running"].includes(last?.state));
  $("#calibrate").disabled = !snapshot.connected || mutationPending;
  for (const selector of ["#pause", "#preview", "#active-profile", "#access-mode", "#save-timings"]) {
    $(selector).disabled = mutationPending || !backendReady;
  }
  if (currentDialog?.kind === "action") syncTestButton();
}

function renderAssignments() {
  const profile = activeProfile();
  const chooser = $("#active-profile");
  chooser.replaceChildren(
    option("", `Automatisch · ${profile.name}`),
    ...snapshot.config.profiles.map(({ id, name }) => option(id, name)),
  );
  chooser.value = snapshot.manualProfileId || "";
  $("#editing-profile").textContent = profile.name;
  $("#assignment-grid").replaceChildren(...POSITIONS.map((position, index) => {
    const card = node("article", { class: "panel assignment-card" }, [node("h3", { text: position })]);
    for (const [gesture, title] of Object.entries(GESTURES)) {
      const action = profile.buttons[index][gesture];
      const name = actionName(action);
      card.append(node("button", {
        type: "button", class: `assignment-row${action ? " filled" : ""}`,
        title: name, "aria-label": `${position}, ${title}, ${name}`,
        click: () => openAction(profile.id, index, gesture),
      }, [node("span", { text: title }), node("span", { text: name })]));
    }
    return card;
  }));
}

function renderProfiles() {
  $("#profile-list").replaceChildren(...snapshot.config.profiles.map((profile) => {
    const isDefault = profile.id === snapshot.config.defaultProfileId;
    const isActive = profile.id === snapshot.activeProfileId;
    const heading = node("div", {}, [
      node("h2", { text: profile.name }),
      node("p", { text: profile.applications.length ? profile.applications.map(displayApp).join(", ") : "Ohne App-Zuordnung" }),
    ]);
    const tools = node("div", { class: "profile-tools" });
    if (isActive) tools.append(node("span", { class: "badge", text: "Aktiv" }));
    if (isDefault) tools.append(node("span", { class: "badge", text: "Standard" }));
    tools.append(button("Bearbeiten", () => openProfile(profile.id), "quiet", { "aria-label": `Profil ${profile.name} bearbeiten` }));
    tools.append(button("Fest wählen", () => mutate("set_manual_profile", { profileId: profile.id }, `Profil „${profile.name}“ fest gewählt.`), "quiet", {
      disabled: snapshot.manualProfileId === profile.id, "aria-label": `Profil ${profile.name} fest wählen`,
    }));
    return node("article", { class: "panel profile-card" }, [heading, tools]);
  }));
}

function renderSettings() {
  $("#access-mode").value = snapshot.config.exclusive ? "exclusive" : "shared";
  if (!timingEdit) {
    $("#hold-ms").value = snapshot.config.holdMs;
    $("#double-ms").value = snapshot.config.doubleTapMs;
  }
  $("#mapping-description").textContent = snapshot.config.buttonMap
    .map((number, index) => `${POSITIONS[index]} · Taste ${number}`).join("   /   ");
}

async function mutate(command, args, success = "", inDialog = false) {
  if (mutationPending) return false;
  mutationPending = true;
  renderRuntime();
  try {
    update(await invoke(command, args));
    if (success) notify(success);
    return true;
  } catch (error) {
    if (inDialog) dialogMessage(message(error), true);
    else notify(message(error), true);
    return false;
  } finally {
    mutationPending = false;
    renderRuntime();
    renderSettings();
  }
}

function saveConfig(config, success = "", inDialog = false) {
  return mutate("save_config", { config }, success, inDialog);
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function navigate(next) {
  resetEaster();
  view = next;
  for (const item of $$(".view")) item.hidden = item.id !== `view-${view}`;
  for (const control of $$("[data-view]")) {
    if (control.dataset.view === view) control.setAttribute("aria-current", "page");
    else control.removeAttribute("aria-current");
  }
  if (view === "settings") checkPermission();
}

function dialogMessage(text, error = false) {
  const element = $("#dialog-message");
  element.textContent = text;
  element.hidden = !text;
  element.classList.toggle("error", error);
}

function dialog(context, title, body, footer, data) {
  currentDialog = data;
  $("#dialog-context").textContent = context;
  $("#dialog-title").textContent = title;
  $("#dialog-body").replaceChildren(...body);
  $("#dialog-footer").replaceChildren(...footer);
  dialogMessage("");
  const element = $("#edit-dialog");
  if (!element.open) element.showModal();
}

function closeDialog() {
  $("#edit-dialog").close();
}

function submitButton(text, click) {
  return button(text, async (event) => {
    const control = event.currentTarget;
    control.disabled = true;
    try { await click(); } catch (error) { dialogMessage(message(error), true); }
    finally { control.disabled = false; }
  }, "primary");
}

function openProfile(id = null) {
  const original = id ? snapshot.config.profiles.find((profile) => profile.id === id) : null;
  const name = input("text", original?.name || "", { maxlength: 64, placeholder: "z. B. Gaming" });
  const applications = node("textarea", { placeholder: "z. B. com.hnc.Discord\ncom.obsproject.obs-studio" });
  applications.value = (original?.applications || []).join("\n");
  const addApp = button("Zuletzt aktive App hinzufügen", () => {
    if (!lastExternalApp) return dialogMessage("Wechsle kurz in die Ziel-App und zurück.", true);
    const ids = applicationIds(applications.value);
    if (!ids.includes(lastExternalApp)) ids.push(lastExternalApp);
    applications.value = ids.join("\n");
    dialogMessage("");
  });
  const isDefault = id === snapshot.config.defaultProfileId;
  const defaultInput = node("input", { type: "checkbox", checked: isDefault, disabled: isDefault });
  const defaultField = node("label", { class: "inline-check" }, [
    defaultInput, document.createTextNode(isDefault ? " Aktuelles Standardprofil" : " Als Standard verwenden"),
  ]);
  const save = submitButton("Speichern", async () => {
    const title = name.value.trim();
    if (!title) throw new Error("Gib einen Profilnamen ein.");
    const ids = applicationIds(applications.value);
    if (ids.some((value) => !/^[\w-]+(?:\.[\w-]+)+$/u.test(value))) {
      throw new Error("Gib gültige App-Kennungen an, jeweils eine pro Zeile.");
    }
    const config = clone(snapshot.config);
    const key = id ?? (globalThis.crypto?.randomUUID?.() ?? `profil-${Date.now()}`);
    const index = config.profiles.findIndex((profile) => profile.id === key);
    const profile = { id: key, name: title, applications: ids, buttons: EMPTY_PEDALS() };
    if (index >= 0) Object.assign(config.profiles[index], { name: title, applications: ids });
    else config.profiles.push(profile);
    if (defaultInput.checked) config.defaultProfileId = key;
    if (await saveConfig(config, `Profil „${title}“ gespeichert.`, true)) closeDialog();
  });
  const footer = [button("Abbrechen", closeDialog), save];
  if (original && snapshot.config.profiles.length > 1) {
    footer.unshift(button("Löschen", () => confirmDelete(original), "danger leading"));
  }
  dialog("Profile", original ? "Profil bearbeiten" : "Profil hinzufügen", [
    field("Profilname", name),
    field("Apps für den automatischen Wechsel", applications, "Eine App-Kennung pro Zeile."),
    node("div", {}, [addApp]),
    defaultField,
  ], footer, { kind: "profile" });
}

function applicationIds(value) {
  return [...new Set(value.split(/[\n,]/).map((id) => id.trim()).filter(Boolean))];
}

function confirmDelete(profile) {
  dialog("Profile", "Profil löschen?", [
    node("p", { text: `Die Belegung von „${profile.name}“ wird gelöscht.` }),
  ], [button("Abbrechen", () => openProfile(profile.id)), submitButton("Profil löschen", async () => {
    const config = clone(snapshot.config);
    config.profiles = config.profiles.filter(({ id }) => id !== profile.id);
    if (config.defaultProfileId === profile.id) config.defaultProfileId = config.profiles[0].id;
    if (await saveConfig(config, `Profil „${profile.name}“ gelöscht.`, true)) closeDialog();
  })], { kind: "profile" });
}

function defaultAction(type) {
  switch (type) {
    case "discordToggle": return { type, shortcut: "Ctrl+Shift+F9" };
    case "discordHold": return { type, shortcut: "Ctrl+Shift+F10" };
    case "shortcut": return { type, shortcut: "" };
    case "obs": return { type, port: 4455, password: "", requestType: "ToggleRecord", requestData: {} };
    case "streamDeck": return { type, port: 29001, actionId: "", actionName: "" };
    case "pause": return { type, milliseconds: 250 };
    case "macro": return { type, steps: [defaultAction("shortcut")] };
    default: return null;
  }
}

function openAction(profileId, index, gesture) {
  const profile = snapshot.config.profiles.find(({ id }) => id === profileId);
  if (!profile) return;
  const original = profile.buttons[index][gesture];
  const editor = node("div", { class: "steps" });
  const state = { kind: "action", profileId, index, gesture, draft: clone(original), editor, read: null };
  const save = submitButton("Speichern", async () => {
    const action = state.read();
    const config = clone(snapshot.config);
    const destination = config.profiles.find(({ id }) => id === profileId);
    if (!destination) throw new Error("Das Profil ist nicht mehr vorhanden.");
    destination.buttons[index][gesture] = action;
    if (await saveConfig(config, "Tastenbelegung gespeichert.", true)) closeDialog();
  });
  const test = button("Aktion testen", async () => {
    try {
      const action = state.read();
      if (action) await mutate("test_action", { action }, "", true);
    } catch (error) { dialogMessage(message(error), true); }
  }, "quiet leading");
  state.test = test;
  dialog(`${profile.name} · ${POSITIONS[index]}`, GESTURES[gesture], [editor], [
    test, button("Abbrechen", closeDialog), save,
  ], state);
  const types = ["none", "discordToggle", "shortcut", "obs", "streamDeck", "macro"];
  if (gesture === "hold") types.splice(2, 0, "discordHold");
  state.read = actionEditor(editor, state.draft, types, () => {
    state.currentType = editor.querySelector("select").value;
    syncTestButton();
  });
  state.currentType = original?.type ?? "none";
  syncTestButton();
}

function syncTestButton() {
  const state = currentDialog;
  if (!state || state.kind !== "action") return;
  state.test.disabled = mutationPending || snapshot.preview || snapshot.paused
    || !state.currentType || ["none", "discordHold"].includes(state.currentType);
  state.test.title = state.currentType === "discordHold"
    ? "Push-to-Talk über das gehaltene Pedal testen."
    : snapshot.preview || snapshot.paused ? "Beende Vorschau oder Pause vor dem Test." : "";
}

function actionEditor(container, value, types, onTypeChange = () => {}) {
  const type = select(types.map((name) => [name, TYPES[name]]), value?.type ?? types[0]);
  const body = node("div", { class: "steps" });
  container.replaceChildren(field("Aktion", type), body);
  let readFields = renderActionFields(body, value ?? defaultAction(type.value));
  type.addEventListener("change", () => {
    readFields = renderActionFields(body, defaultAction(type.value));
    onTypeChange();
  });
  return (strict = true) => readFields(strict);
}

function renderActionFields(body, action) {
  body.replaceChildren();
  if (!action) return () => null;
  const { type } = action;

  if (["shortcut", "discordToggle", "discordHold"].includes(type)) {
    const shortcut = input("text", action.shortcut, { placeholder: "Ctrl+Shift+F9", maxlength: 80 });
    const help = type === "discordHold"
      ? "Verwende dasselbe Kürzel für Push-to-Talk in der Discord-Desktop-App."
      : type === "discordToggle" ? "Verwende dasselbe Kürzel für „Mute umschalten“ in Discord."
        : "Beispiel: Ctrl+Alt+F10. Ohne globales Kürzel empfängt das aktive Fenster die Eingabe.";
    body.append(field("Tastenkürzel", shortcut, help));
    return (strict) => {
      const value = shortcut.value.trim();
      if (strict && !value) throw new Error("Gib ein Tastenkürzel ein.");
      return { type, shortcut: value };
    };
  }

  if (type === "pause") {
    const duration = input("number", action.milliseconds, { min: 1, max: 10000, step: 1 });
    body.append(field("Wartezeit in Millisekunden", duration));
    return (strict) => ({ type, milliseconds: number(duration, 1, 10000, "Wartezeit", strict) });
  }

  if (type === "obs") return obsFields(body, action);
  if (type === "streamDeck") return streamDeckFields(body, action);
  return macroFields(body, action.steps);
}

function number(control, minimum, maximum, name, strict = true) {
  const result = Number(control.value);
  if (strict && (!Number.isInteger(result) || result < minimum || result > maximum)) {
    throw new Error(`${name} muss zwischen ${minimum} und ${maximum} liegen.`);
  }
  return result;
}

const OBS_PRESETS = {
  ToggleRecord: { label: "Aufnahme umschalten" },
  ToggleStream: { label: "Livestream umschalten" },
  ToggleReplayBuffer: { label: "Replay-Puffer umschalten" },
  SaveReplayBuffer: { label: "Replay speichern" },
  SetCurrentProgramScene: { label: "Szene wechseln", key: "sceneName", field: "Szenenname" },
  ToggleInputMute: { label: "Mikrofon oder Audioquelle umschalten", key: "inputName", field: "Name der Audioquelle" },
  TriggerHotkeyByName: { label: "OBS-Hotkey auslösen", key: "hotkeyName", field: "Name des OBS-Hotkeys" },
};

function obsFields(body, action) {
  const port = input("number", action.port, { min: 1, max: 65535 });
  const password = input("password", action.password, { autocomplete: "off", placeholder: "Optional" });
  const requestType = input("text", action.requestType, { maxlength: 96, placeholder: "ToggleRecord" });
  const requestData = node("textarea", { spellcheck: "false", placeholder: '{"sceneName":"Gameplay"}' });
  requestData.value = typeof action.requestData === "string" ? action.requestData : JSON.stringify(action.requestData, null, 2);

  const current = OBS_PRESETS[action.requestType];
  const keys = Object.keys(action.requestData || {});
  const simple = current && (!keys.length || (keys.length === 1 && keys[0] === current.key && typeof action.requestData[current.key] === "string"));
  const choices = Object.entries(OBS_PRESETS).map(([id, preset]) => [id, preset.label]);
  const preset = select([...choices, ["custom", "Eigener OBS-Befehl"]], simple ? action.requestType : "custom");
  const parameter = input("text", simple && current.key ? action.requestData[current.key] || "" : "");
  const parameterField = field("", parameter, "Verwende den Namen genau wie in OBS.");
  const custom = node("div", { class: "steps" }, [
    field("OBS-Befehl", requestType),
    field("Parameter als JSON", requestData, "Beispiel für SetCurrentProgramScene: {\"sceneName\":\"Gameplay\"}"),
  ]);

  const updatePreset = () => {
    const selected = OBS_PRESETS[preset.value];
    custom.hidden = Boolean(selected);
    parameterField.hidden = !selected?.key;
    if (selected?.key) parameterField.querySelector("span").textContent = selected.field;
  };
  preset.addEventListener("change", () => {
    if (preset.value !== "custom") parameter.value = "";
    updatePreset();
  });
  body.append(
    field("OBS-Aktion", preset),
    parameterField,
    custom,
    node("div", { class: "field-row" }, [field("OBS-Port", port), field("WebSocket-Kennwort", password)]),
  );
  updatePreset();

  return (strict) => {
    const selected = OBS_PRESETS[preset.value];
    let request = preset.value;
    let data = {};
    if (selected?.key) {
      const value = parameter.value.trim();
      if (strict && !value) throw new Error(`Gib „${selected.field}“ ein.`);
      data[selected.key] = value;
    } else if (!selected) {
      request = requestType.value.trim();
      data = requestData.value || "{}";
      try {
        data = JSON.parse(data);
        if (!data || Array.isArray(data) || typeof data !== "object") throw new Error();
      } catch {
        if (strict) throw new Error("Die OBS-Parameter müssen ein JSON-Objekt sein.");
      }
      if (strict && !/^[A-Za-z0-9_]+$/.test(request)) {
        throw new Error("Gib einen gültigen OBS-Befehl ein.");
      }
    }
    return { type: "obs", port: number(port, 1, 65535, "Port", strict), password: password.value, requestType: request, requestData: data };
  };
}

function streamDeckFields(body, action) {
  const port = input("number", action.port, { min: 1, max: 65535 });
  const actionId = input("text", action.actionId, { maxlength: 256, placeholder: "Aktionen laden oder ID eingeben" });
  let knownName = action.actionName || "";
  let nameId = action.actionId;
  const actions = select([["", "Aktion aus MCP Deck wählen"]], "");
  actions.hidden = true;
  actions.setAttribute("aria-label", "Stream-Deck-Aktion");
  const load = button("Aktionen laden", async () => {
    load.disabled = true;
    try {
      const usedPort = number(port, 1, 65535, "Port");
      const list = await invoke("list_elgato_actions", { port: usedPort });
      actions.replaceChildren(option("", "Aktion aus MCP Deck wählen"),
        ...list.map(({ id, name, description }) => node("option", { value: id, text: name, title: description || name })));
      actions.value = actionId.value;
      actions.hidden = list.length === 0;
      dialogMessage(list.length ? `${list.length} Aktionen gefunden.` : "Auf dem MCP Deck sind keine Aktionen verfügbar.");
    } catch (error) { dialogMessage(message(error), true); }
    finally { load.disabled = false; }
  });
  actions.addEventListener("change", () => {
    if (!actions.value) return;
    actionId.value = actions.value;
    nameId = actions.value;
    knownName = actions.selectedOptions[0].textContent;
  });
  body.append(node("div", { class: "inline-field" }, [field("Lokaler Port", port), load]));
  body.append(actions, field("Aktions-ID", actionId, "Port 29001 für den integrierten HTTP-Server, Port 9090 für Elgatos separaten Server."));
  return (strict) => {
    const id = actionId.value.trim();
    if (strict && !id) throw new Error("Wähle eine Stream-Deck-Aktion oder gib ihre ID ein.");
    return {
      type: "streamDeck", port: number(port, 1, 65535, "Port", strict), actionId: id,
      actionName: id === nameId ? knownName : "",
    };
  };
}

function macroFields(body, initial) {
  const steps = node("div", { class: "steps" });
  const types = ["shortcut", "discordToggle", "obs", "streamDeck", "pause"];
  let readers = [];
  const capture = () => readers.map((read) => read(false));
  const add = button("Schritt hinzufügen", () => {
    const values = capture();
    values.push(defaultAction("shortcut"));
    show(values);
  });

  function show(values) {
    readers = [];
    steps.replaceChildren();
    values.forEach((value, index) => {
      const controls = node("div");
      for (const [offset, glyph, text] of [[-1, "↑", "nach oben"], [1, "↓", "nach unten"]]) {
        controls.append(button(glyph, () => {
          const current = capture();
          [current[index], current[index + offset]] = [current[index + offset], current[index]];
          show(current);
        }, "quiet", { "aria-label": `Schritt ${index + 1} ${text}`, disabled: index + offset < 0 || index + offset >= values.length }));
      }
      controls.append(button("Entfernen", () => {
        const current = capture();
        current.splice(index, 1);
        show(current);
      }, "quiet", { "aria-label": `Schritt ${index + 1} entfernen` }));
      const editor = node("div", { class: "steps" });
      const heading = node("div", { class: "step-head" }, [node("strong", { text: `Schritt ${index + 1}` }), controls]);
      steps.append(node("div", { class: "step" }, [heading, editor]));
      readers.push(actionEditor(editor, value, types));
    });
    add.disabled = values.length >= 24;
  }

  body.append(steps, node("div", {}, [add]));
  show(initial);
  return (strict) => {
    if (strict && readers.length === 0) throw new Error("Füge mindestens einen Makroschritt hinzu.");
    return { type: "macro", steps: readers.map((read) => read(strict)) };
  };
}

function startCalibration() {
  if (!snapshot.connected || mutationPending) return;
  const progress = node("div", { class: "calibration-keys" });
  const instruction = node("p");
  const apply = submitButton("Zuordnung speichern", async () => {
    const current = calibration;
    if (!current || current.order.length !== 3 || snapshot.rawButtons.some(Boolean)) return;
    const config = clone(snapshot.config);
    config.buttonMap = current.order;
    if (await saveConfig(config, "Pedaltasten zugeordnet.", true)) closeDialog();
  });
  const current = {
    order: [], ready: false, candidate: null, previousPreview: snapshot.preview, pending: true,
    progress, instruction, apply, warning: "",
  };
  calibration = current;
  dialog("Einstellungen", "Pedaltasten kalibrieren", [instruction, progress], [
    button("Abbrechen", closeDialog), apply,
  ], { kind: "calibration" });
  drawCalibration();
  mutate("set_preview", { preview: true }, "", true).then((ok) => {
    if (calibration !== current) return;
    current.pending = false;
    if (!ok) current.warning = "Die Vorschau konnte nicht gestartet werden.";
    onCalibrationState();
  });
}

function onCalibrationState() {
  const current = calibration;
  if (!current) return;
  if (!snapshot.connected) {
    current.warning = "Das Pedal wurde getrennt. Starte die Kalibrierung nach dem Verbinden erneut.";
    current.order = [];
    drawCalibration();
    return;
  }
  if (current.pending || current.warning.startsWith("Das Pedal") || !snapshot.preview) return;
  const raw = snapshot.rawButtons;
  const down = raw.flatMap((pressed, index) => pressed ? [index + 1] : []);
  if (down.length === 0) {
    if (current.candidate !== null) current.order.push(current.candidate);
    current.candidate = null;
    current.ready = true;
    current.warning = "";
  } else if (down.length > 1) {
    current.candidate = null;
    current.ready = false;
    current.warning = "Drücke jeweils nur eine Pedaltaste.";
  } else if (current.ready && current.order.length < 3) {
    current.ready = false;
    if (current.order.includes(down[0])) current.warning = "Diese Taste ist bereits erfasst.";
    else current.candidate = down[0];
  }
  drawCalibration();
}

function drawCalibration() {
  const current = calibration;
  if (!current) return;
  const count = current.order.length;
  current.instruction.textContent = current.pending ? "Vorschau wird aktiviert …"
    : current.warning || (current.ready
      ? (count === 3 ? "Alle drei Tasten erkannt. Speichere die Zuordnung." : `Drücke das Pedal ${POSITIONS[count].toLowerCase()}.`)
      : "Lass alle Tasten vollständig los.");
  current.progress.replaceChildren(...POSITIONS.map((position, index) => node("span", {
    class: index < count ? "done" : index === count ? "active" : "",
    text: index < count ? `✓ ${position}` : position,
  })));
  current.apply.disabled = !snapshot.connected || !current.ready || count !== 3 || mutationPending;
}

async function checkPermission(request = false) {
  const feedback = $("#keyboard-permission");
  try {
    const granted = await invoke(request ? "request_shortcut_permission" : "get_shortcut_permission");
    feedback.textContent = granted ? "Tastaturzugriff erlaubt." : "Tastaturzugriff fehlt.";
  } catch (error) {
    feedback.textContent = "Tastaturzugriff konnte nicht geprüft werden.";
    if (request) notify(message(error), true);
  }
}

function renderUpdate(status = updateStatus) {
  if (status) updateStatus = status;
  const current = updateStatus;
  $("#installed-version").textContent = current?.currentVersion ? `Installiert: ${current.currentVersion}` : "";
  const latest = $("#latest-version");
  latest.textContent = current?.latestVersion ? `Neueste Version: ${current.latestVersion}` : "";
  latest.hidden = !current?.latestVersion;
  const statusText = $("#update-status");
  if (current) statusText.textContent = current.message;
  statusText.classList.toggle("error", current?.state === "error");
  const checked = $("#update-checked");
  const checkedDate = current?.checkedAt ? new Date(current.checkedAt) : null;
  checked.hidden = !checkedDate || Number.isNaN(checkedDate.valueOf());
  if (!checked.hidden) {
    checked.dateTime = current.checkedAt;
    checked.textContent = `Zuletzt geprüft: ${checkedDate.toLocaleString("de-DE", { dateStyle: "short", timeStyle: "short" })}`;
  }
  const notes = $("#update-notes");
  notes.hidden = !current?.notes;
  notes.querySelector("p").textContent = current?.notes ?? "";
  const busy = updatePending || ["checking", "installing"].includes(current?.state);
  $("#check-update").disabled = busy || !BRIDGE?.core?.invoke;
  const install = $("#install-update");
  install.hidden = current?.state !== "available";
  install.disabled = busy || !BRIDGE?.core?.invoke;
  $("#feedback").disabled = !BRIDGE?.core?.invoke;
}

async function updateCommand(command) {
  if (updatePending) return;
  updatePending = true;
  renderUpdate();
  try {
    renderUpdate(await invoke(command));
  } catch (error) {
    renderUpdate({ ...updateStatus, state: "error", message: message(error) });
  } finally {
    updatePending = false;
    renderUpdate();
  }
}

function resetEaster() {
  easter = { started: 0, steps: 0, down: null, ready: false };
}

function onEasterState() {
  if (!snapshot.connected || !snapshot.preview || view !== "pedal" || calibration
      || currentDialog || $("#edit-dialog").open || $("#easter-dialog").open
      || !document.hasFocus() || document.visibilityState !== "visible") {
    resetEaster();
    return;
  }
  const down = snapshot.buttons.flatMap((pressed, index) => pressed ? [index] : []);
  const now = performance.now();
  if (easter.started && now - easter.started > EASTER_TIMEOUT) resetEaster();
  if (down.length > 1) {
    resetEaster();
    return;
  }
  if (down.length === 1) {
    if (!easter.ready || easter.down !== null) return;
    easter.down = down[0];
    if (!easter.started) easter.started = now;
    return;
  }
  if (easter.down !== null) {
    if (easter.down === EASTER_SEQUENCE[easter.steps]) easter.steps++;
    else {
      easter.steps = easter.down === EASTER_SEQUENCE[0] ? 1 : 0;
      easter.started = easter.steps ? now : 0;
    }
    easter.down = null;
    if (easter.steps === EASTER_SEQUENCE.length) {
      resetEaster();
      showEaster();
      return;
    }
  }
  easter.ready = true;
}

function showEaster() {
  const dialog = $("#easter-dialog");
  const video = $("#easter-video");
  const feedback = $("#easter-message");
  feedback.hidden = true;
  dialog.classList.add("speed-flash");
  dialog.showModal();
  video.src = EASTER_VIDEO;
  video.play().catch(() => {
    if (!dialog.open) return;
    feedback.textContent = video.error
      ? "Das Video konnte nicht geladen werden."
      : "Drücke Play, um den Clip zu starten.";
    feedback.hidden = false;
  });
}

function stopEaster() {
  const dialog = $("#easter-dialog");
  const video = $("#easter-video");
  video.pause();
  video.removeAttribute("src");
  video.load();
  dialog.classList.remove("speed-flash");
  resetEaster();
}

function wire() {
  for (const control of $$("[data-view]")) control.addEventListener("click", () => navigate(control.dataset.view));
  $("#pause").addEventListener("click", () => mutate("set_paused", { paused: !snapshot.paused }));
  $("#preview").addEventListener("click", () => mutate("set_preview", { preview: !snapshot.preview }));
  $("#active-profile").addEventListener("change", (event) => {
    const profileId = event.currentTarget.value || null;
    mutate("set_manual_profile", { profileId }).then(renderAssignments);
  });
  $("#new-profile").addEventListener("click", () => openProfile());
  $("#access-mode").addEventListener("change", (event) => {
    const exclusive = event.currentTarget.value === "exclusive";
    saveConfig({ ...snapshot.config, exclusive }, "Zugriffsmodus gespeichert.");
  });
  $("#hold-ms").addEventListener("input", () => { timingEdit = true; });
  $("#double-ms").addEventListener("input", () => { timingEdit = true; });
  $("#save-timings").addEventListener("click", async () => {
    try {
      const holdMs = number($("#hold-ms"), 200, 2000, "Haltezeit");
      const doubleTapMs = number($("#double-ms"), 120, 800, "Doppeltippzeit");
      if (await saveConfig({ ...snapshot.config, holdMs, doubleTapMs }, "Erkennungszeiten gespeichert.")) {
        timingEdit = false;
        renderSettings();
      }
    } catch (error) { notify(message(error), true); }
  });
  $("#calibrate").addEventListener("click", startCalibration);
  $("#grant-permission").addEventListener("click", () => checkPermission(true));
  window.addEventListener("focus", () => { if (view === "settings") checkPermission(); });
  $("#feedback").addEventListener("click", async () => {
    try { await invoke("open_feedback"); }
    catch (error) { notify(message(error), true); }
  });
  $("#check-update").addEventListener("click", () => updateCommand("check_for_update"));
  $("#install-update").addEventListener("click", () => updateCommand("install_update"));
  $("#close-easter").addEventListener("click", () => $("#easter-dialog").close());
  $("#easter-dialog").addEventListener("close", stopEaster);
  $("#easter-video").addEventListener("playing", () => { $("#easter-message").hidden = true; });
  $("#easter-video").addEventListener("error", () => {
    if (!$("#easter-dialog").open) return;
    const feedback = $("#easter-message");
    feedback.textContent = "Das Video konnte nicht geladen werden.";
    feedback.hidden = false;
  });
  window.addEventListener("blur", resetEaster);
  document.addEventListener("visibilitychange", resetEaster);
  $("#close-dialog").addEventListener("click", closeDialog);
  $("#dialog-form").addEventListener("submit", (event) => event.preventDefault());
  $("#edit-dialog").addEventListener("close", () => {
    const previous = calibration;
    calibration = null;
    currentDialog = null;
    if (!previous || previous.previousPreview) return;
    const restore = () => {
      if (mutationPending) setTimeout(restore, 40);
      else if (snapshot.preview) mutate("set_preview", { preview: false });
    };
    restore();
  });
}

async function init() {
  wire();
  renderRuntime();
  renderAssignments();
  renderProfiles();
  renderSettings();
  renderUpdate();
  if (!BRIDGE?.core?.invoke) {
    $("#connection").querySelector("strong").textContent = "App-Vorschau";
    $("#connection-mode").textContent = "Philips USB-Pedal";
    $("#update-status").textContent = "Updates sind in der macOS-App verfügbar.";
    return;
  }
  try {
    if (BRIDGE.event?.listen) await BRIDGE.event.listen("pedal-state", (event) => update(event.payload));
  } catch (error) {
    notify(`Live-Anzeige konnte nicht gestartet werden. ${message(error)}`, true);
  }
  try { update(await invoke("get_state")); }
  catch (error) { notify(`Pedalsteuerung konnte nicht gestartet werden. ${message(error)}`, true); }
  try {
    if (BRIDGE.event?.listen) await BRIDGE.event.listen("pedal-update", (event) => renderUpdate(event.payload));
    renderUpdate(await invoke("get_update_status"));
  } catch (error) {
    renderUpdate({ state: "error", message: message(error) });
  }
}

init();
