// The page of the GUI. It draws what the app gives it and turns clicks into calls: nothing here
// decides anything about Crisol (ADR-0026). The words come from the dictionary of the app.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let words = {};
let ready = false; // the words are in and the page is translated
let current = null; // { revision, state, summary }, the newest the app sent
let section = "services";
let failure = null; // the last call that failed, until it is dismissed
let notice = null; // what the last call that worked said: { message, warnings }
let pending = new Set(); // the clicks in flight, so that none is made twice
let sitesView = null; // the last answer of `sites`
let terminalOptions = null; // the last answer of `terminal_options`
let wasReloading = false;
let picked = null; // the folder waiting for a name in the link dialog
let versionsView = null; // the last answer of `versions`
let installProgress = {}; // how far each installation in flight is, by what it installs
let confirming = null; // the version whose removal waits for a second click
let doctorView = null; // the last answer of `doctor`
let doctorArmed = null; // the finding whose button waits for its second click
let trustPreview = null; // what the trust button shows before its second click

// The dictionary key of what the daemon says; a state this page does not know shows as itself.
const STATE_KEYS = {
  running: "state.running",
  starting: "state.starting",
  stopping: "state.stopping",
  stopped: "state.stopped",
  failed: "state.failed",
};
const OVERALL_KEYS = {
  running: "overall.running",
  starting: "overall.starting",
  stopped: "overall.stopped",
  failed: "overall.failed",
  no_daemon: "overall.no_daemon",
};
// The look of a state: the chip class and the sign that does not depend on color (ADR-0027).
// The signs are drawn by CSS (`.sign-*`, prefixed so they never meet a class such as the
// panels' `.empty`), not with characters: those come from different
// fallback fonts and do not sit on the same line.
const CHIPS = {
  running: ["running", "full"],
  starting: ["busy", "half"],
  stopping: ["busy", "half"],
  stopped: ["stopped", "empty"],
  failed: ["failed", "cross"],
  no_daemon: ["stopped", "dash"],
};

function t(key, values) {
  let text = Object.prototype.hasOwnProperty.call(words, key) ? words[key] : key;
  for (const [name, value] of Object.entries(values || {})) {
    text = text.split("{" + name + "}").join(value);
  }
  return text;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function translatePage() {
  for (const node of document.querySelectorAll("[data-i18n]")) {
    node.textContent = t(node.dataset.i18n);
  }
  for (const node of document.querySelectorAll("[data-i18n-label]")) {
    node.setAttribute("aria-label", t(node.dataset.i18nLabel));
  }
}

function chip(kind, label) {
  const [className, shape] = CHIPS[kind] || CHIPS.stopped;
  const node = el("span", "chip " + className);
  const sign = el("span", "sign sign-" + shape);
  sign.setAttribute("aria-hidden", "true"); // the label says it in words
  node.append(sign, el("span", null, label));
  return node;
}

function showSection(name) {
  section = name;
  notice = null; // what the last action said belongs to the screen it was done on
  for (const panel of document.querySelectorAll(".panel")) {
    panel.hidden = panel.id !== "section-" + name;
  }
  for (const item of document.querySelectorAll(".nav-item")) {
    if (item.dataset.section === name) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  }
  document.getElementById("title").textContent = t("section." + name + ".title");
  // What a section draws is asked when it is opened; the app owns the truth.
  if (name === "sites") refreshSites();
  if (name === "versions") refreshVersions();
  if (name === "doctor") refreshDoctor();
  if (name === "setup") refreshSetup();
  if (name === "settings") refreshTerminalOptions();
  if (name === "logs") openLogs();
  else stopLogs();
  if (name === "credentials") openCredentials();
  // The banners belong to the screen: the offer to set up is not shown on its own screen.
  render();
}

// ---- calls ------------------------------------------------------------------------------

// The app answers a failed call with `{ message, fix }`, the words of the core.
function asFailure(error) {
  if (error && typeof error === "object" && "message" in error) return error;
  return { message: String(error), fix: null };
}

// Runs `work`, shows what went wrong as it comes, and draws again. `key` stops the same click
// from being made twice while it is in flight.
async function run(key, work) {
  if (pending.has(key)) return undefined;
  pending.add(key);
  render();
  try {
    const result = await work();
    failure = null;
    return result;
  } catch (error) {
    failure = asFailure(error);
    notice = null;
    return undefined;
  } finally {
    pending.delete(key);
    render();
  }
}

const SITE_ACTS = ["site", "link", "park", "unpark"];

// A click of the services or sites screens. The app turns it into the call the command line
// would make.
async function perform(act) {
  const done = await run(JSON.stringify(act), async () => {
    notice = null;
    const done = await invoke("act", { act });
    notice = done.message || (done.warnings && done.warnings.length) ? done : null;
    if (SITE_ACTS.includes(act.do)) await loadSites();
    // The new password is on screen as soon as it exists.
    if (act.do === "reset_password") {
      await loadCredentials();
      // The projects keep their own copy of the old password in their .env.
      const said = notice || { message: "", warnings: [] };
      notice = { message: said.message, warnings: [...(said.warnings || []), t("credentials.reset_reminder")] };
    }
    return done;
  });
  return done;
}

async function loadSites() {
  sitesView = await invoke("sites");
}

function refreshSites() {
  return run("sites", loadSites);
}

async function loadVersions() {
  versionsView = await invoke("versions");
}

function refreshVersions() {
  return run("versions", loadVersions);
}

// 12 345 678 → "11.8 MB". Only how a number is drawn.
function formatBytes(bytes) {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return (unit === 0 ? String(value) : value.toFixed(1)) + " " + units[unit];
}

// What the daemon answered to a click of the versions screen, in the words of this page; what the
// core says about a version (its vulnerabilities) comes as it said it.
function versionNotice(reply) {
  if (reply.type === "installed") {
    const name = reply.service + " " + reply.version;
    const warnings = [];
    if (reply.unverified) warnings.push(t("versions.warn_unverified", { name }));
    if (reply.end_of_life) warnings.push(t("versions.warn_end_of_life", { name }));
    for (const advisory of reply.advisories || []) {
      const remedy = advisory.fixed_in
        ? t("versions.fixed_in", { version: advisory.fixed_in })
        : t("versions.no_fix");
      // The summary is the core's sentence, with or without its full stop.
      const summary = advisory.summary.replace(/[.\s]+$/, "");
      warnings.push(
        t("versions.warn_advisory", { name, id: advisory.id, summary }) + ". " + remedy
      );
    }
    return {
      message: reply.already
        ? t("versions.done_already", { name })
        : t("versions.done_installed", { name, size: formatBytes(reply.downloaded) }),
      warnings,
    };
  }
  if (reply.type === "removed") {
    return {
      message: t("versions.done_removed", { name: reply.service + " " + reply.version }),
      warnings: [],
    };
  }
  if (reply.type === "used") {
    return {
      // `selects` already names the tool (`php 7.4.33`).
      message: t("versions.done_used", { name: reply.selects }),
      warnings: reply.warnings || [],
    };
  }
  return null;
}

// A click of the versions screen. The app turns it into the call the command line would make.
async function performVersion(act) {
  confirming = null;
  const what = act.service + " " + act.version;
  await run(JSON.stringify(act), async () => {
    notice = null;
    try {
      notice = versionNotice(await invoke("version_act", { act }));
    } finally {
      delete installProgress[what];
    }
    await loadVersions();
  });
}

// ---- doctor -----------------------------------------------------------------------------

// The core says what is wrong and, beside the fix, what a button can do for it; the app decides
// which of those become a button. Nothing is done without a click, and what removes something
// or asks Windows for a permission takes two.
async function loadDoctor() {
  doctorArmed = null;
  trustPreview = null;
  doctorView = await invoke("doctor");
}

function refreshDoctor() {
  return run("doctor", loadDoctor);
}

async function performDoctor(row) {
  const action = row.button.action;
  doctorArmed = null;
  trustPreview = null;
  await run("doctor:" + row.name, async () => {
    notice = null;
    const done = await invoke("doctor_act", { action });
    notice = { message: t(done.words.key, done.words.args), warnings: [] };
    await loadDoctor();
  });
}

async function pressDoctor(row) {
  const b = row.button;
  if (b.kind === "copy") {
    copyText(b.text, t("doctor.copied_what"));
    return;
  }
  if (b.kind === "show_log") {
    logs.service = b.text;
    showSection("logs");
    return;
  }
  if (b.confirm && doctorArmed !== row.name) {
    doctorArmed = row.name;
    trustPreview = null;
    if (b.action.kind === "local" && b.action.op === "trust") {
      // What Windows will ask about, before it asks.
      await run("doctor-preview", async () => {
        trustPreview = await invoke("doctor_trust_preview");
      });
      if (!trustPreview) doctorArmed = null;
    }
    render();
    return;
  }
  await performDoctor(row);
}

const DOCTOR_CHIPS = { ok: "running", warning: "starting", problem: "failed" };

function trustBox(p) {
  const box = el("div", "doctor-preview");
  if (p.already) {
    box.append(el("p", null, t("doctor.trust_already")));
    return box;
  }
  box.append(
    el("p", null, p.name + (p.created ? t("doctor.trust_made") : "")),
    el("p", "detail", t("doctor.trust_vouches", { what: p.vouches_for })),
    el("p", "detail", t("doctor.trust_until", { date: p.valid_until })),
    el("p", "detail", "SHA-256 " + p.sha256)
  );
  if (p.sha1) {
    box.append(el("p", null, t("doctor.trust_check", { sha1: p.sha1 })));
  }
  return box;
}

function doctorActions(row) {
  const b = row.button;
  const actions = el("span", "actions");
  const key = "doctor:" + row.name;
  const label = pending.has(key) ? t("doctor.working") : t(b.label.key, b.label.args);
  if (b.confirm && doctorArmed === row.name) {
    actions.append(
      button(t("doctor.confirm", { label }), () => pressDoctor(row), key, "danger"),
      button(t("doctor.cancel"), () => {
        doctorArmed = null;
        trustPreview = null;
        render();
      })
    );
  } else {
    actions.append(button(label, () => pressDoctor(row), key));
  }
  return actions;
}

function renderDoctor() {
  const box = document.getElementById("doctor-view");
  const empty = document.getElementById("doctor-empty");
  const summary = document.getElementById("doctor-summary");
  // Checking again only asks `doctor` again: what is found and what each row offers is still
  // the app's to say. What was fixed from a terminal shows without leaving the screen.
  // It is only there while nothing is being checked: neither a check of its own nor the one
  // that follows a fix (a button of a row asks `doctor` again when it is done).
  const again = document.getElementById("doctor-again");
  again.textContent = t("doctor.check_again");
  again.hidden = [...pending].some((key) => key === "doctor" || key.startsWith("doctor:"));
  if (!doctorView) {
    box.replaceChildren();
    empty.hidden = false;
    empty.textContent = t("doctor.loading");
    summary.textContent = "";
    return;
  }
  empty.hidden = true;
  summary.textContent =
    doctorView.problems || doctorView.warnings
      ? t("doctor.summary", { problems: doctorView.problems, warnings: doctorView.warnings })
      : t("doctor.all_ok");
  box.replaceChildren(
    ...doctorView.rows.map((row) => {
      const item = el("li", "row doctor");
      item.append(chip(DOCTOR_CHIPS[row.status], t("doctor.status." + row.status)));
      const grow = el("span", "grow");
      grow.append(el("strong", null, row.label || row.name), el("span", "detail", row.detail));
      // The exact thing to do: what the command line would say, in the core's words, shown
      // under the button so that nothing is done without being seen first.
      if (row.fix) {
        const fix = el("span", "detail doctor-fix");
        fix.append(t("doctor.will_do") + " ", el("code", null, row.fix));
        grow.append(fix);
      }
      if (row.button && doctorArmed === row.name && trustPreview) {
        grow.append(trustBox(trustPreview));
      }
      item.append(grow);
      if (row.button) item.append(doctorActions(row));
      return item;
    })
  );
}

// ---- first run and uninstall ------------------------------------------------------------

// The plan of `crisol setup` and of `crisol uninstall --all`, in the core's words. Looking
// changes nothing; a run plans again in the app, so what this page holds is only what it draws.
let setupView = null; // the last answer of `setup_plan`
let setupSwitches = {}; // the optional steps the person has on, by id
let setupResult = null; // how the last run went: { outcomes, failed }
let uninstallView = null; // the last answer of `uninstall_plan`, once the person asked for it
let uninstallPurge = false; // also delete the home, with the data and the projects
let uninstallBackups = false; // the person says they have a backup
let uninstallArmed = false; // the button waits for its second click
let uninstallResult = null; // how the last run went: { outcomes, done, close_app }

const OUTCOME_CHIPS = { done: "running", nothing: "running", declined: "starting", failed: "failed", skipped: "stopped" };

function applySetupView(view) {
  setupView = view;
  // A switch keeps what the person chose; a step seen for the first time starts as the app says.
  const next = {};
  for (const step of view.steps) {
    if (!step.optional) continue;
    next[step.id] = Object.prototype.hasOwnProperty.call(setupSwitches, step.id) ? setupSwitches[step.id] : step.on_by_default;
  }
  setupSwitches = next;
}

async function loadSetup() {
  const screen = await invoke("setup_plan", { first: false });
  applySetupView(screen.view);
}

function refreshSetup() {
  return run("setup", loadSetup);
}

async function performSetup() {
  const accepted = Object.keys(setupSwitches).filter((id) => setupSwitches[id]);
  await run("setup:run", async () => {
    notice = null;
    setupResult = await invoke("setup_run", { accepted });
    await loadSetup();
  });
}

function outcomesList(outcomes) {
  const list = el("ul", "rows");
  for (const outcome of outcomes) {
    const item = el("li", "row doctor");
    item.append(chip(OUTCOME_CHIPS[outcome.result], t("outcome." + outcome.result)));
    const grow = el("span", "grow");
    grow.append(el("strong", null, t("step." + outcome.step)), el("span", "detail", outcome.message));
    item.append(grow);
    list.append(item);
  }
  return list;
}

function renderSetup() {
  const box = document.getElementById("setup-view");
  const empty = document.getElementById("setup-empty");
  if (!setupView) {
    box.replaceChildren();
    empty.hidden = false;
    empty.textContent = t("setup.loading");
    return;
  }
  empty.hidden = true;
  const parts = [];
  if (setupResult) {
    parts.push(el("h2", null, t(setupResult.failed ? "setup.result_failed" : "setup.result")), outcomesList(setupResult.outcomes));
  }
  if (setupView.problems.length) {
    parts.push(el("h2", null, t("setup.blocked")));
    const list = el("ul", "rows");
    for (const problem of setupView.problems) {
      const item = el("li", "row doctor");
      item.append(chip("failed", t("doctor.status.problem")));
      const grow = el("span", "grow");
      grow.append(el("span", "detail", problem.message));
      if (problem.fix) {
        const fix = el("span", "detail doctor-fix");
        fix.append(t("doctor.will_do") + " ", el("code", null, problem.fix));
        grow.append(fix);
      }
      item.append(grow);
      list.append(item);
    }
    parts.push(list);
  } else if (setupView.steps.length) {
    parts.push(el("h2", null, t("setup.todo")), el("p", "hint small", t("setup.todo_hint")));
    const list = el("ul", "rows");
    for (const step of setupView.steps) {
      const item = el("li", "row doctor");
      const grow = el("span", "grow");
      if (step.optional) {
        const label = el("label", "check");
        const switchBox = el("input");
        switchBox.type = "checkbox";
        switchBox.checked = !!setupSwitches[step.id];
        switchBox.addEventListener("change", () => {
          setupSwitches[step.id] = switchBox.checked;
        });
        label.append(switchBox, el("strong", null, t("step." + step.id)));
        grow.append(label);
      } else {
        grow.append(el("strong", null, t("step." + step.id)));
      }
      grow.append(el("span", "detail", step.description));
      item.append(grow);
      list.append(item);
    }
    parts.push(list);
    const actions = el("div", "toolbar");
    const label = pending.has("setup:run") ? t("doctor.working") : t("setup.run");
    actions.append(button(label, performSetup, "setup:run", "primary"));
    parts.push(actions);
  } else {
    parts.push(el("p", "hint", t("setup.complete")));
  }
  if (setupView.done.length) {
    parts.push(...linesSection(t("setup.in_place"), setupView.done));
  }
  for (const note of setupView.notes) parts.push(el("p", "hint small", note));
  box.replaceChildren(...parts);
}

async function loadUninstall() {
  uninstallArmed = false;
  uninstallView = await invoke("uninstall_plan", { purge: uninstallPurge });
  if (!uninstallView.deletes_home) uninstallBackups = false;
}

function openUninstall() {
  uninstallResult = null;
  return run("uninstall:plan", loadUninstall);
}

async function performUninstall() {
  const purge = uninstallPurge;
  const backups = uninstallBackups;
  uninstallArmed = false;
  await run("uninstall:run", async () => {
    notice = null;
    uninstallResult = await invoke("uninstall_run", { purge, backups });
    // After a run that went through there is no daemon, no CA and no entry left to plan.
    await loadUninstall();
  });
}

function linesSection(title, lines) {
  if (!lines.length) return [];
  const list = el("ul", "plain");
  for (const line of lines) list.append(el("li", null, line));
  return [el("h3", null, title), list];
}

function renderUninstall() {
  const box = document.getElementById("uninstall-view");
  if (!uninstallView) {
    box.replaceChildren(button(t("uninstall.review"), openUninstall, "uninstall:plan"));
    return;
  }
  const plan = uninstallView;
  const parts = [];
  const purge = el("label", "check");
  const purgeBox = el("input");
  purgeBox.type = "checkbox";
  purgeBox.checked = uninstallPurge;
  purgeBox.disabled = pending.has("uninstall:run");
  purgeBox.addEventListener("change", async () => {
    uninstallPurge = purgeBox.checked;
    uninstallBackups = false;
    uninstallResult = null;
    await run("uninstall:plan", loadUninstall);
  });
  purge.append(purgeBox, el("span", null, t("uninstall.purge")));
  parts.push(purge, el("p", "hint small", t("uninstall.purge_hint")));

  if (uninstallResult) {
    parts.push(
      el("h3", null, t(uninstallResult.done ? "uninstall.result_done" : "uninstall.result_left")),
      outcomesList(uninstallResult.outcomes)
    );
    if (uninstallResult.close_app) parts.push(el("p", "hint", t("uninstall.closing")));
  }

  if (!plan.steps.length) {
    parts.push(el("p", "hint", t("uninstall.nothing")));
  } else {
    parts.push(el("h3", null, t("uninstall.will")));
    const list = el("ol", "plain");
    for (const step of plan.steps) list.append(el("li", null, step.description));
    parts.push(list);
  }
  if (plan.purge) {
    const purgeNotice = plan.purge;
    if (purgeNotice.foreign.length) {
      parts.push(...linesSection(t("uninstall.foreign"), purgeNotice.foreign));
    } else {
      parts.push(el("p", "hint", t("uninstall.purge_deletes", { home: purgeNotice.home })));
      parts.push(...linesSection(t("uninstall.databases"), purgeNotice.databases));
      parts.push(...linesSection(t("uninstall.projects"), purgeNotice.projects));
    }
  }
  parts.push(...linesSection(t("uninstall.by_hand"), plan.by_hand));
  parts.push(...linesSection(t("uninstall.problems"), plan.problems));
  parts.push(...linesSection(t("uninstall.kept"), plan.kept));

  if (plan.steps.length && !(uninstallResult && uninstallResult.close_app)) {
    if (plan.deletes_home) {
      const label = el("label", "check");
      const backupBox = el("input");
      backupBox.type = "checkbox";
      backupBox.checked = uninstallBackups;
      backupBox.addEventListener("change", () => {
        uninstallBackups = backupBox.checked;
        uninstallArmed = false;
        render();
      });
      label.append(backupBox, el("span", null, t("uninstall.backups")));
      parts.push(label);
    }
    const actions = el("div", "toolbar");
    const key = "uninstall:run";
    const label = pending.has(key) ? t("doctor.working") : t("uninstall.run");
    if (uninstallArmed) {
      actions.append(
        button(t("doctor.confirm", { label }), performUninstall, key, "danger"),
        button(t("doctor.cancel"), () => {
          uninstallArmed = false;
          render();
        })
      );
    } else {
      const go = button(
        label,
        () => {
          uninstallArmed = true;
          render();
        },
        key,
        "danger"
      );
      go.disabled = go.disabled || (plan.deletes_home && !uninstallBackups);
      actions.append(go);
    }
    parts.push(actions);
  }
  box.replaceChildren(...parts);
}

// ---- logs -------------------------------------------------------------------------------

const LOG_EVERY = 1000; // ms between two reads of the log being followed

// Follows one log by asking again from where the last read ended. A read that answers after the
// person chose another log (or left the screen) is dropped: `logs.token` changes with each.
const logs = { service: null, next: null, lines: [], timer: null, token: 0, choices: [] };

function logView() {
  return document.getElementById("log-view");
}

function stopLogs() {
  logs.token += 1;
  clearTimeout(logs.timer);
  logs.timer = null;
}

function showLogMessage(text) {
  const empty = document.getElementById("log-empty");
  empty.hidden = !text;
  empty.textContent = text || "";
  logView().hidden = !!text;
}

function drawLogLines(update) {
  const view = logView();
  if (update.replace) {
    logs.lines = update.lines.slice();
    view.textContent = logs.lines.join("\n");
  } else if (update.lines.length) {
    const had = logs.lines.length > 0;
    logs.lines.push(...update.lines);
    if (logs.lines.length > update.keep) {
      logs.lines.splice(0, logs.lines.length - update.keep);
      view.textContent = logs.lines.join("\n");
    } else {
      view.append((had ? "\n" : "") + update.lines.join("\n"));
    }
  }
  if (update.replace || update.lines.length) {
    if (document.getElementById("log-follow").checked) view.scrollTop = view.scrollHeight;
  }
}

async function readLog(token) {
  const service = logs.service;
  try {
    const update = await invoke("log_read", { service, from: logs.next });
    if (token !== logs.token) return;
    logs.next = update.next;
    document.getElementById("log-path").textContent = update.path;
    showLogMessage("");
    drawLogLines(update);
    if (!logs.lines.length) showLogMessage(t("logs.empty"));
  } catch (error) {
    if (token !== logs.token) return;
    // A service that has not written anything yet has no log: say so and keep looking.
    logs.next = null;
    logs.lines = [];
    document.getElementById("log-path").textContent = "";
    showLogMessage(asFailure(error).message);
  }
  if (token === logs.token) logs.timer = setTimeout(() => readLog(token), LOG_EVERY);
}

function followLog(service) {
  stopLogs();
  logs.service = service;
  logs.next = null;
  logs.lines = [];
  logView().textContent = "";
  document.getElementById("log-path").textContent = "";
  showLogMessage(t("logs.loading"));
  const token = logs.token;
  readLog(token);
}

async function openLogs() {
  let choices;
  try {
    choices = await invoke("log_choices");
  } catch (error) {
    failure = asFailure(error);
    render();
    return;
  }
  if (section !== "logs") return;
  logs.choices = choices;
  const select = document.getElementById("log-select");
  select.replaceChildren(...choices.map((name) => new Option(name, name)));
  const service = choices.includes(logs.service) ? logs.service : choices[0];
  select.value = service;
  followLog(service);
}

// ---- credentials ------------------------------------------------------------------------

// What the credentials screen shows: the service chosen, its answer or why there is none, which
// secrets are on view, and whether the reset waits for its second click.
const creds = { service: null, view: null, error: null, revealed: new Set(), confirming: false, serviceState: null };

async function loadCredentials() {
  const service = creds.service;
  if (!service) return;
  try {
    const view = await invoke("credentials", { service });
    if (service !== creds.service) return;
    creds.view = view;
    creds.error = null;
  } catch (error) {
    if (service !== creds.service) return;
    creds.view = null;
    creds.error = asFailure(error);
  }
  render();
}

function chooseCredentials(service) {
  creds.service = service;
  creds.view = null;
  creds.error = null;
  creds.revealed = new Set();
  creds.confirming = false;
  creds.serviceState = serviceStateOf(service);
  render();
  return loadCredentials();
}

function serviceStateOf(name) {
  const found = current && current.state.services.find((s) => s.name === name);
  return found ? found.state : null;
}

async function openCredentials() {
  let choices;
  try {
    choices = await invoke("credential_choices");
  } catch (error) {
    failure = asFailure(error);
    render();
    return;
  }
  const select = document.getElementById("cred-select");
  select.replaceChildren(...choices.map((name) => new Option(name, name)));
  // The first one that is running, else the first: that is the one a person came to look at.
  const running = choices.find((name) => serviceStateOf(name) === "running");
  const service = choices.includes(creds.service) ? creds.service : running || choices[0];
  select.value = service;
  chooseCredentials(service);
}

// Puts `text` on the clipboard. The page is not a secure context in every WebView, so the old way
// is the fallback.
async function copyText(text, what) {
  let copied = false;
  try {
    await navigator.clipboard.writeText(text);
    copied = true;
  } catch (_) {
    const area = el("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    try {
      // It answers false, it does not throw, when the browser refuses.
      copied = document.execCommand("copy");
    } finally {
      area.remove();
    }
  }
  if (copied) {
    failure = null;
    notice = { message: t("credentials.copied", { what }), warnings: [] };
  } else {
    notice = null;
    failure = { message: t("credentials.copy_failed"), fix: null };
  }
  render();
}

function resetBox(view) {
  const box = el("div", "cred-actions");
  const reason = {
    needs_daemon: "credentials.reset_needs_daemon",
    stop_first: "credentials.reset_stop_first",
    start_first: "credentials.reset_start_first",
    busy: "credentials.reset_busy",
  }[view.reset];
  if (view.reset === "no_password") {
    box.append(el("span", "hint small", t("credentials.no_password", { name: view.service })));
  } else if (reason) {
    box.append(el("span", "hint small", t(reason, { name: view.service })));
  } else {
    const act = { do: "reset_password", service: view.service };
    if (creds.confirming) {
      box.append(
        el("span", "hint small warn", t("credentials.reset_warning", { name: view.service })),
        button(t("credentials.reset_confirm", { name: view.service }), async () => {
          creds.confirming = false;
          await perform(act);
        }, JSON.stringify(act), "danger"),
        button(t("action.keep"), () => {
          creds.confirming = false;
          render();
        })
      );
    } else {
      box.append(
        button(t("credentials.reset"), () => {
          creds.confirming = true;
          render();
        })
      );
    }
  }
  return box;
}

function renderCredentials() {
  const empty = document.getElementById("cred-empty");
  const box = document.getElementById("cred-view");
  if (creds.error) {
    empty.hidden = false;
    empty.textContent = creds.error.message;
    box.replaceChildren();
    return;
  }
  if (!creds.view) {
    empty.hidden = !creds.service;
    empty.textContent = t("credentials.loading");
    box.replaceChildren();
    return;
  }
  empty.hidden = true;
  const view = creds.view;
  const rows = el("ul", "rows");
  for (const line of view.lines) {
    const row = el("li", "row cred-line");
    const hidden = line.secret && !creds.revealed.has(line.key);
    row.append(
      el("span", "key", line.key),
      el("span", "value", hidden ? "••••••••••••" : line.value)
    );
    const actions = el("span", "actions");
    if (line.secret) {
      actions.append(
        button(hidden ? t("credentials.show") : t("credentials.hide"), () => {
          if (creds.revealed.has(line.key)) creds.revealed.delete(line.key);
          else creds.revealed.add(line.key);
          render();
        })
      );
    }
    actions.append(button(t("credentials.copy"), () => copyText(line.value, line.key)));
    row.append(actions);
    rows.append(row);
  }
  const all = el("div", "cred-actions");
  all.append(button(t("credentials.copy_env"), () => copyText(view.env, ".env")));
  const notes = view.notes.map((note) => el("p", "hint small", note));
  box.replaceChildren(all, rows, ...notes, resetBox(view));
}

function refreshTerminalOptions() {
  return run("terminal_options", async () => {
    terminalOptions = await invoke("terminal_options");
  });
}

async function openTerminal(place) {
  await run("terminal:" + JSON.stringify(place), async () => {
    notice = null;
    const opened = await invoke("open_terminal", { place });
    notice = {
      message: t("terminal.opened", { terminal: opened.terminal, folder: opened.folder }),
      warnings: opened.note ? [opened.note] : [],
    };
  });
}

function openSite(name) {
  return run("open:" + name, () => invoke("open_site", { name }));
}

async function chooseFolder() {
  return run("pick_folder", () => invoke("pick_folder"));
}

async function pickAndPark() {
  const folder = await chooseFolder();
  if (folder) await perform({ do: "park", path: folder.path });
}

async function pickAndLink() {
  const folder = await chooseFolder();
  if (!folder) return;
  picked = folder;
  document.getElementById("link-path").textContent = folder.path;
  document.getElementById("link-name").value = folder.name;
  document.getElementById("link-suffix").textContent = "." + (sitesView ? sitesView.tld : "test");
  setLinkDialog(true);
}

function setLinkDialog(visible) {
  document.getElementById("link-dialog").hidden = !visible;
  if (visible) {
    const field = document.getElementById("link-name");
    field.focus();
    field.select();
  } else {
    picked = null;
  }
}

function submitLink(event) {
  event.preventDefault();
  if (!picked) return;
  const name = document.getElementById("link-name").value.trim();
  const path = picked.path;
  setLinkDialog(false);
  // The core has the last word on the name, and its answer is shown as it comes.
  perform({ do: "link", name, path });
}

// ---- pieces -----------------------------------------------------------------------------

// A button that makes `onClick`, disabled while the same click is in flight.
function button(label, onClick, key, className) {
  const node = el("button", "button small" + (className ? " " + className : ""), label);
  node.type = "button";
  node.disabled = !!key && pending.has(key);
  node.addEventListener("click", onClick);
  return node;
}

function actButton(label, act, className) {
  return button(label, () => perform(act), JSON.stringify(act), className);
}

function tag(text, className) {
  return el("span", "tag" + (className ? " " + className : ""), text);
}

function renderHeader({ state, summary }) {
  const overall = document.getElementById("overall");
  const fresh = chip(summary.overall, t(OVERALL_KEYS[summary.overall] || "overall.no_daemon"));
  overall.className = fresh.className;
  overall.replaceChildren(...fresh.childNodes);

  const connection = document.getElementById("connection");
  if (state.connection.kind === "connected" && summary.daemon_version) {
    connection.textContent = t("conn.connected", { version: summary.daemon_version });
  } else {
    connection.textContent = "";
  }
}

function renderServices({ state, summary }) {
  const list = document.getElementById("services");
  const empty = document.getElementById("services-empty");
  list.replaceChildren(
    ...summary.rows.map((row) => {
      const item = el("li", "service");
      item.append(el("span", "dot " + row.metal), el("span", "name", row.name));
      const side = el("span", "side-by-side");
      // A data service that `Start all` leaves out says so; the button beside it changes it.
      if (row.enabled === false) side.append(tag(t("service.left_out")));
      // Nginx and PHP go together: their buttons are Start all and Stop all.
      if (row.web_stack) side.append(tag(t("service.web_stack")));
      side.append(chip(row.state, t(STATE_KEYS[row.state] || row.state)));
      item.append(side);
      // The buttons are the app's decision (`row.actions`): this page only names them.
      const actions = el("span", "actions");
      for (const action of row.actions) {
        actions.append(actButton(t("action." + action), { do: "service", action, name: row.name }));
      }
      item.append(actions);
      // What the daemon says about a failure is shown as it comes.
      if (row.detail) item.append(el("span", "detail", row.detail));
      return item;
    })
  );
  const connected = state.connection.kind === "connected";
  empty.hidden = summary.rows.length > 0;
  empty.textContent = connected ? t("services.none") : t("services.no_daemon");

  const toggle = document.getElementById("toggle-all");
  const act = { do: "all", toggle: summary.toggle };
  toggle.textContent = t("action." + summary.toggle);
  toggle.disabled = pending.has(JSON.stringify(act));
  toggle.onclick = () => perform(act);
}

function renderReloading({ state }) {
  const node = document.getElementById("reloading");
  node.hidden = !state.reloading;
  if (state.reloading) {
    const fresh = chip("starting", t("reloading"));
    node.replaceChildren(...fresh.childNodes);
  }
}

function renderSites() {
  const parked = document.getElementById("parked");
  const list = document.getElementById("sites-list");
  const empty = document.getElementById("sites-empty");
  if (!sitesView) {
    parked.replaceChildren();
    list.replaceChildren();
    empty.hidden = false;
    empty.textContent = t("sites.loading");
    return;
  }
  parked.replaceChildren(
    ...sitesView.parked.map((folder) => {
      const item = el("li", "row");
      item.append(el("span", "grow mono path", folder.path));
      if (!folder.exists) item.append(tag(t("sites.missing"), "warn"));
      if (folder.removable) {
        item.append(actButton(t("action.unpark"), { do: "unpark", entry: folder.entry }));
      }
      return item;
    })
  );
  list.replaceChildren(
    ...sitesView.sites.map((site) => {
      const item = el("li", "row site");
      const head = el("span", "grow");
      head.append(el("strong", null, site.domain));
      head.append(el("span", "path mono", site.docroot));
      item.append(head);
      item.append(tag(t("sites.kind." + site.kind)));
      if (site.secure) item.append(tag("HTTPS", "secure"));
      const actions = el("span", "actions");
      actions.append(button(t("action.open"), () => openSite(site.name), "open:" + site.name));
      actions.append(
        button(
          t("action.terminal_here"),
          () => openTerminal({ where: "site", site: site.name }),
          "terminal:" + JSON.stringify({ where: "site", site: site.name })
        )
      );
      for (const action of site.actions) {
        actions.append(actButton(t("action." + action), { do: "site", action, name: site.name }));
      }
      item.append(actions);
      return item;
    })
  );
  empty.hidden = sitesView.sites.length > 0;
  empty.textContent = t("sites.none");
  // What the daemon left out is shown as it said it.
  for (const warning of sitesView.warnings) {
    const item = el("li", "row");
    item.append(tag(t("sites.warning"), "warn"), el("span", "grow detail", warning));
    list.append(item);
  }
}

// The bar of an installation in flight: how many bytes of how many, or only the bytes when the
// server did not say the total.
function progressBox(row) {
  const what = row.service + " " + row.version;
  const progress = installProgress[what];
  const box = el("span", "progress");
  if (!progress) {
    box.append(el("span", "detail", t("versions.waiting")));
    return box;
  }
  const bar = el("progress");
  bar.setAttribute("aria-label", t("versions.progress_label", { name: what }));
  bar.max = 100;
  if (progress.percent !== null) bar.value = progress.percent;
  box.append(bar);
  box.append(
    el(
      "span",
      "detail",
      progress.total
        ? t("versions.progress", { done: formatBytes(progress.done), total: formatBytes(progress.total) })
        : t("versions.progress_unknown", { done: formatBytes(progress.done) })
    )
  );
  return box;
}

function versionButtons(row) {
  const actions = el("span", "actions");
  const key = row.service + " " + row.version;
  for (const action of row.actions) {
    const act = { do: action, service: row.service, version: row.version };
    if (action === "uninstall") {
      // Removing deletes the files of that version: it takes a second click.
      if (confirming === key) {
        actions.append(
          button(t("action.confirm_remove", { version: row.version }), () => performVersion(act), JSON.stringify(act), "danger"),
          button(t("action.keep"), () => {
            confirming = null;
            render();
          })
        );
      } else {
        actions.append(
          button(t("action.uninstall"), () => {
            confirming = key;
            render();
          })
        );
      }
    } else {
      actions.append(button(t("action." + action), () => performVersion(act), JSON.stringify(act)));
    }
  }
  return actions;
}

function renderVersions() {
  const box = document.getElementById("versions");
  const empty = document.getElementById("versions-empty");
  if (!versionsView) {
    box.replaceChildren();
    empty.hidden = false;
    empty.textContent = t("versions.loading");
    return;
  }
  empty.hidden = versionsView.services.length > 0;
  empty.textContent = t("versions.none");
  box.replaceChildren(
    ...versionsView.services.map((group) => {
      const section = el("div", "version-group");
      const title = el("h2", "version-title");
      title.append(el("span", "dot " + group.service), el("span", null, group.service));
      section.append(title);
      const inUse = group.rows.find((row) => row.default);
      if (group.missing_default && inUse) {
        section.append(
          el(
            "p",
            "hint small warn",
            t("versions.missing_default", {
              tool: group.service,
              asked: group.missing_default,
              version: inUse.version,
            })
          )
        );
      }
      const list = el("ul", "rows");
      for (const row of group.rows) {
        const item = el("li", "row");
        item.append(el("span", "version-name mono", row.version));
        // The marks wrap among themselves, so the buttons stay on the first line of the row.
        const marks = el("span", "tags");
        if (row.default) marks.append(tag(t("versions.default"), "secure"));
        if (row.installed && !row.default) marks.append(tag(t("versions.installed")));
        if (row.recommended) marks.append(tag(t("versions.recommended")));
        if (row.end_of_life) marks.append(tag(t("versions.end_of_life"), "warn"));
        if (row.advisories.length) {
          marks.append(tag(t("versions.vulnerable", { ids: row.advisories.join(", ") }), "warn"));
        }
        if (row.unverified) marks.append(tag(t("versions.unverified"), "warn"));
        item.append(marks);
        const installing = pending.has(
          JSON.stringify({ do: "install", service: row.service, version: row.version })
        );
        item.append(installing ? progressBox(row) : versionButtons(row));
        list.append(item);
      }
      section.append(list);
      return section;
    })
  );
}

function renderSettings() {
  const select = document.getElementById("terminal-select");
  const effective = document.getElementById("terminal-effective");
  const warning = document.getElementById("terminal-warning");
  if (!terminalOptions) {
    select.replaceChildren();
    effective.textContent = "";
    warning.hidden = true;
    return;
  }
  const options = [new Option(t("settings.terminal_auto"), "")];
  for (const option of terminalOptions.options) {
    const label = option.found ? option.label : option.label + " (" + t("settings.not_found") + ")";
    const entry = new Option(label, option.name);
    // A terminal that is gone cannot be chosen, but the one chosen stays listed.
    entry.disabled = !option.found && option.name !== terminalOptions.chosen;
    options.push(entry);
  }
  // A name crisol.toml has and Crisol does not know: shown as it is, so that choosing
  // «automatic» (or another one) is a change that takes it out.
  const UNKNOWN = "(unknown)";
  if (terminalOptions.unknown) {
    const entry = new Option(terminalOptions.unknown + " (" + t("settings.not_recognized") + ")", UNKNOWN);
    entry.disabled = true;
    options.push(entry);
  }
  select.replaceChildren(...options);
  select.value = terminalOptions.unknown ? UNKNOWN : terminalOptions.chosen || "";
  const open = terminalOptions.options.find((o) => o.name === terminalOptions.effective);
  effective.textContent = open
    ? t("settings.terminal_opens", { terminal: open.label })
    : t("settings.terminal_none");
  warning.hidden = !terminalOptions.warning;
  warning.textContent = terminalOptions.warning || "";
}

function banner(kind, title, text, fix) {
  const node = el("div", "banner " + kind);
  node.append(el("span", "banner-title", title));
  if (text) node.append(el("span", "banner-text", text));
  if (fix) {
    const line = el("span", "banner-fix");
    line.append(t("banner.fix") + ": ", el("code", null, fix));
    node.append(line);
  }
  return node;
}

function renderBanners({ state, summary }) {
  const nodes = [];
  if (summary.refusal) {
    nodes.push(banner("failed", t("conn.refused"), summary.refusal));
  }
  if (state.version_warning) {
    // `detail` and `fix` are the core's own words.
    nodes.push(banner("", t("banner.version"), state.version_warning.detail, state.version_warning.fix));
  }
  if (state.reload_error) {
    nodes.push(banner("failed", t("banner.reload_error"), state.reload_error));
  }
  if (state.blocked) {
    nodes.push(banner("", t("banner.blocked"), state.blocked));
  }
  // What the daemon has to say about the services (a default version that was removed, say), in
  // its own words.
  for (const warning of state.warnings || []) {
    nodes.push(banner("", t("banner.warning"), warning));
  }
  // The setup opened by itself once; while Crisol still lacks a step it needs, it stays here as
  // an offer (`incomplete` is decided by the app, not here).
  if (setupView && setupView.incomplete && section !== "setup") {
    const node = banner("", t("banner.setup"), t("banner.setup_text"));
    const go = el("button", "button small", t("banner.setup_go"));
    go.type = "button";
    go.addEventListener("click", () => showSection("setup"));
    node.append(go);
    nodes.push(node);
  }
  if (failure) {
    const node = banner("failed", t("banner.call_failed"), failure.message, failure.fix);
    const close = el("button", "button small", t("banner.dismiss"));
    close.type = "button";
    close.addEventListener("click", () => {
      failure = null;
      render();
    });
    node.append(close);
    nodes.push(node);
  }
  if (notice) {
    // What the core said about what it did; anything it warned about comes with it.
    const node = banner("ok", notice.message || t("banner.done"));
    for (const warning of notice.warnings || []) node.append(el("span", "banner-text", warning));
    const close = el("button", "button small", t("banner.dismiss"));
    close.type = "button";
    close.addEventListener("click", () => {
      notice = null;
      render();
    });
    node.append(close);
    nodes.push(node);
  }
  document.getElementById("banners").replaceChildren(...nodes);
}

// A state from the app, unless an equal or newer one was already drawn: `bootstrap` and the
// `state` event can arrive in either order.
function take(payload) {
  if (current && payload.revision <= current.revision) return;
  current = { revision: payload.revision, state: payload.state, summary: payload.summary };
  // When the daemon finishes applying the web configuration the sites may have changed.
  if (wasReloading && !current.state.reloading && section === "sites") refreshSites();
  wasReloading = current.state.reloading;
  // Whether the password of a service can be reset depends on its state.
  if (section === "credentials" && creds.service) {
    const now = serviceStateOf(creds.service);
    if (now !== creds.serviceState) {
      creds.serviceState = now;
      // A second click armed in the old state does not carry over to the new one.
      creds.confirming = false;
      loadCredentials();
    }
  }
  render();
}

function render() {
  if (!current || !ready) return;
  renderHeader(current);
  renderReloading(current);
  renderServices(current);
  renderSites();
  renderVersions();
  renderDoctor();
  renderSetup();
  renderUninstall();
  renderCredentials();
  renderSettings();
  renderBanners(current);
}

function setExitNotice(visible) {
  document.getElementById("exit-notice").hidden = !visible;
  if (visible) document.getElementById("exit-cancel").focus();
}

async function start() {
  // Listening first: a change between `bootstrap` and `listen` would otherwise not be drawn
  // until the next one.
  await listen("state", (event) => take(event.payload));
  await listen("call_failed", (event) => {
    failure = event.payload;
    render();
  });
  await listen("install_progress", (event) => {
    installProgress[event.payload.what] = event.payload;
    if (section === "versions") render();
  });
  await listen("exit_notice", () => setExitNotice(true));
  await listen("exit_notice_dismiss", () => setExitNotice(false));

  const boot = await invoke("bootstrap");
  words = boot.words.words;
  document.documentElement.lang = boot.words.language;
  translatePage();
  showSection(section);
  ready = true;
  take(boot);
  render();

  // What is left to set up: the screen opens by itself the first time, and after that the
  // banner offers it. A failure to look is not a reason to stop the app from opening.
  invoke("setup_plan", { first: true }).then(
    (screen) => {
      applySetupView(screen.view);
      if (screen.open) showSection("setup");
      render();
    },
    () => {}
  );

  for (const item of document.querySelectorAll(".nav-item")) {
    item.addEventListener("click", () => showSection(item.dataset.section));
  }
  document.getElementById("exit-cancel").addEventListener("click", () => setExitNotice(false));
  document.getElementById("exit-confirm").addEventListener("click", () => invoke("quit_confirmed"));
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      setExitNotice(false);
      setLinkDialog(false);
    }
  });

  // The terminal of Crisol: in the folder of a project, or where the projects are.
  for (const id of ["terminal-general", "terminal-general-sites"]) {
    document.getElementById(id).addEventListener("click", () => openTerminal({ where: "projects" }));
  }
  document.getElementById("log-select").addEventListener("change", (event) => followLog(event.target.value));
  document.getElementById("cred-select").addEventListener("change", (event) => chooseCredentials(event.target.value));
  // Scrolling away from the end stops following; coming back to the end follows again.
  logView().addEventListener("scroll", () => {
    const view = logView();
    document.getElementById("log-follow").checked = view.scrollHeight - view.scrollTop - view.clientHeight < 8;
  });
  document.getElementById("log-follow").addEventListener("change", (event) => {
    if (event.target.checked) logView().scrollTop = logView().scrollHeight;
  });
  document.getElementById("doctor-again").addEventListener("click", refreshDoctor);
  document.getElementById("link-folder").addEventListener("click", pickAndLink);
  document.getElementById("park-folder").addEventListener("click", pickAndPark);
  document.getElementById("link-form").addEventListener("submit", submitLink);
  document.getElementById("link-cancel").addEventListener("click", () => setLinkDialog(false));
  document.getElementById("terminal-select").addEventListener("change", async (event) => {
    const name = event.target.value || null;
    await run("choose_terminal", () => invoke("choose_terminal", { name }));
    // Drawn again from what the app wrote, not from what was clicked; a failure of the save
    // stays on screen.
    const kept = failure;
    await refreshTerminalOptions();
    failure = kept;
    render();
  });
}

start();
