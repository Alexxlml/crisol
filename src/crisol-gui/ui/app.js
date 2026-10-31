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
  for (const panel of document.querySelectorAll(".panel")) {
    panel.hidden = panel.id !== "section-" + name;
  }
  for (const item of document.querySelectorAll(".nav-item")) {
    if (item.dataset.section === name) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  }
  document.getElementById("title").textContent = t("section." + name + ".title");
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
      item.append(chip(row.state, t(STATE_KEYS[row.state] || row.state)));
      // What the daemon says about a failure is shown as it comes.
      if (row.detail) item.append(el("span", "detail", row.detail));
      return item;
    })
  );
  const connected = state.connection.kind === "connected";
  empty.hidden = summary.rows.length > 0;
  empty.textContent = connected ? t("services.none") : t("services.no_daemon");
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
  document.getElementById("banners").replaceChildren(...nodes);
}

// A state from the app, unless an equal or newer one was already drawn: `bootstrap` and the
// `state` event can arrive in either order.
function take(payload) {
  if (current && payload.revision <= current.revision) return;
  current = { revision: payload.revision, state: payload.state, summary: payload.summary };
  render();
}

function render() {
  if (!current || !ready) return;
  renderHeader(current);
  renderServices(current);
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
  await listen("exit_notice", () => setExitNotice(true));
  await listen("exit_notice_dismiss", () => setExitNotice(false));

  const boot = await invoke("bootstrap");
  words = boot.words.words;
  document.documentElement.lang = boot.words.language === "spanish" ? "es" : "en";
  translatePage();
  showSection(section);
  ready = true;
  take(boot);
  render();

  for (const item of document.querySelectorAll(".nav-item")) {
    item.addEventListener("click", () => showSection(item.dataset.section));
  }
  document.getElementById("exit-cancel").addEventListener("click", () => setExitNotice(false));
  document.getElementById("exit-confirm").addEventListener("click", () => invoke("quit_confirmed"));
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") setExitNotice(false);
  });
}

start();
