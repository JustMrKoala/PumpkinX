"use strict";

const $ = (sel) => document.querySelector(sel);
const el = (tag, attrs = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null) node.setAttribute(k, v);
  }
  for (const c of children.flat()) node.append(c instanceof Node ? c : document.createTextNode(String(c)));
  return node;
};

let token = "";
let timers = [];
try { token = localStorage.getItem("pumpkinx-session") || ""; } catch (_) { /* storage blocked */ }

const state = { status: null, tickHistory: [], logLines: [], logSeq: 0, settings: null, page: "overview" };

function toast(message, isError = false) {
  const t = el("div", { class: "toast" + (isError ? " error" : "") }, message);
  $("#toasts").append(t);
  setTimeout(() => t.remove(), 4200);
}

async function api(path, options = {}) {
  const res = await fetch("/api" + path, {
    ...options,
    headers: { "Content-Type": "application/json", Authorization: "Bearer " + token, ...(options.headers || {}) },
  });
  if (res.status === 401) { setSession(""); showLogin("Please sign in again."); throw new Error("unauthorized"); }
  if (!res.ok) throw new Error((await res.text()) || res.statusText);
  return res.json();
}

/* Login */
let setupMode = false;

async function showLogin(message = "") {
  $("#app").classList.add("hidden");
  $("#login").classList.remove("hidden");
  $("#login-error").textContent = message;
  timers.forEach(clearInterval);
  try {
    setupMode = (await (await fetch("/api/auth/state")).json()).needs_setup;
  } catch (_) { /* keep the current mode */ }
  $("#login-token").classList.toggle("hidden", !setupMode);
  $("#login-token").required = setupMode;
  $("#login-note").textContent = setupMode
    ? "Create the first account. The setup token is in pumpkinx.toml and is only needed this once."
    : "Sign in to your server.";
  $("#login-submit").textContent = setupMode ? "Create account" : "Sign in";
  $("#login-password").autocomplete = setupMode ? "new-password" : "current-password";
}

function setSession(session) {
  token = session;
  try {
    if (session) localStorage.setItem("pumpkinx-session", session);
    else localStorage.removeItem("pumpkinx-session");
  } catch (_) { /* storage blocked */ }
}

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const body = { username: $("#login-user").value.trim(), password: $("#login-password").value };
  if (setupMode) body.token = $("#login-token").value.trim();
  try {
    const res = await fetch(setupMode ? "/api/auth/setup" : "/api/auth/login", {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await res.text());
    setSession((await res.json()).session);
    $("#login-password").value = "";
    $("#login-token").value = "";
    start();
  } catch (err) {
    $("#login-error").textContent = err.message;
  }
});
$("#logout").addEventListener("click", async () => {
  try { await api("/auth/logout", { method: "POST" }); } catch (_) { /* the session may be gone already */ }
  setSession("");
  showLogin();
});

/* Navigation */
function showPage(page) {
  state.page = page;
  document.querySelectorAll("#nav a").forEach((a) => a.classList.toggle("active", a.dataset.page === page));
  document.querySelectorAll(".page").forEach((p) => p.classList.toggle("hidden", p.id !== "page-" + page));
  if (page === "players") loadPlayers();
  if (page === "settings") { if (!state.settings) loadSettings(); loadIcon(); }
  if (page === "features") { loadFeatures(); loadUsers(); }
  if (page === "files") loadFiles(files.path);
  if (page === "plugins") loadPlugins();
  if (page === "console") scrollLog(true);
  try { localStorage.setItem("pumpkinx-page", page); } catch (_) { /* storage blocked */ }
}
document.querySelectorAll("#nav a").forEach((a) => a.addEventListener("click", () => showPage(a.dataset.page)));

/* Formatting */
const fmtBytes = (b) => {
  if (!b) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(b) / Math.log(1024)));
  return (b / 1024 ** i).toFixed(i ? 1 : 0) + " " + u[i];
};
const fmtUptime = (s) => {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m ${s % 60}s`;
};
const pretty = (key) => key.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());

/* Overview */
async function refreshStatus() {
  try {
    const s = await api("/status");
    state.status = s;
    setOnline(true);
    $("#motd").textContent = s.motd || "Overview";
    $("#version-line").textContent = `PumpkinX ${s.pumpkinx_version} · Minecraft ${s.minecraft_version} (protocol ${s.protocol})`;
    $("#stat-players").textContent = `${s.players_online} / ${s.max_players}`;
    const tps = $("#stat-tps");
    tps.textContent = s.tps.toFixed(1);
    tps.className = "value " + (s.tps >= 19 ? "good" : s.tps >= 15 ? "warnc" : "badc");
    $("#stat-mspt").textContent = s.mspt.toFixed(2) + " ms per tick";
    $("#stat-mem").textContent = fmtBytes(s.memory_bytes);
    $("#stat-mem-sub").textContent = "of " + fmtBytes(s.total_memory_bytes) + " system";
    $("#stat-cpu").textContent = s.cpu_percent.toFixed(0) + "%";
    $("#stat-uptime").textContent = fmtUptime(s.uptime_secs);

    state.tickHistory.push(s.mspt);
    if (state.tickHistory.length > 150) state.tickHistory.shift();
    drawTickChart();

    $("#world-list").replaceChildren(...s.worlds.map((w) =>
      el("li", {}, el("span", {}, w.name.replace("minecraft:", "")), el("span", { class: "tag" }, `${w.players} online`))));
    const mv = s.multiversion, pb = s.patchbukkit;
    $("#feature-list").replaceChildren(
      el("li", {}, el("span", {}, "Multiversion"), el("span", { class: "tag " + (mv.running ? "on" : "off") }, mv.running ? mv.range : "off")),
      el("li", {}, el("span", {}, "Keystone"), el("span", { class: "tag " + (s.keystone.running ? "on" : "off") }, s.keystone.running ? "on" : s.keystone.embedded ? "off" : "not in this build")),
      el("li", {}, el("span", {}, "PatchBukkit"), el("span", { class: "tag " + (pb.running ? "on" : "off") },
        pb.running ? `${pb.jars.length} plugin jar(s)` : pb.compiled_in ? "off" : "not in this build")),
    );
    const ks = s.keystone;
    const pending = mv.saved !== mv.running || pb.saved !== pb.running || (ks.embedded && ks.saved !== ks.running);
    $("#restart-banner").classList.toggle("hidden", !pending && !state.restartPending);
  } catch (err) {
    if (err.message !== "unauthorized") setOnline(false);
  }
}

function setOnline(online) {
  const pill = $("#server-pill");
  pill.className = "pill " + (online ? "online" : "offline");
  $("#server-pill-text").textContent = online ? "server online" : "server unreachable";
}

function drawTickChart() {
  const canvas = $("#tick-chart");
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w) return;
  canvas.width = w * dpr; canvas.height = h * dpr;
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const data = state.tickHistory;
  const max = Math.max(60, ...data) * 1.1;
  const y = (v) => h - 18 - (v / max) * (h - 28);
  ctx.font = "11px system-ui";
  ctx.fillStyle = "#a8917e";
  ctx.strokeStyle = "#3a2a1f";
  ctx.lineWidth = 1;
  for (const v of [0, 25, 50]) {
    ctx.beginPath(); ctx.moveTo(30, y(v)); ctx.lineTo(w, y(v)); ctx.stroke();
    ctx.fillText(String(v), 4, y(v) + 4);
  }
  ctx.setLineDash([4, 4]); ctx.strokeStyle = "#ff5a4f88";
  ctx.beginPath(); ctx.moveTo(30, y(50)); ctx.lineTo(w, y(50)); ctx.stroke();
  ctx.setLineDash([]);
  if (data.length < 2) return;
  const step = (w - 34) / 149;
  const x0 = w - (data.length - 1) * step;
  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, "#ff8a1f66"); grad.addColorStop(1, "#ff8a1f00");
  ctx.beginPath();
  data.forEach((v, i) => (i ? ctx.lineTo(x0 + i * step, y(v)) : ctx.moveTo(x0, y(v))));
  ctx.strokeStyle = "#ff8a1f"; ctx.lineWidth = 2; ctx.stroke();
  ctx.lineTo(x0 + (data.length - 1) * step, y(0)); ctx.lineTo(x0, y(0)); ctx.closePath();
  ctx.fillStyle = grad; ctx.fill();
}
window.addEventListener("resize", drawTickChart);

/* Console */
const LEVELS = ["trace", "debug", "info", "warn", "error"];
function logVisible(line) {
  const min = LEVELS.indexOf($("#log-level").value);
  if (LEVELS.indexOf(line.level) < min) return false;
  const f = $("#log-filter").value.trim().toLowerCase();
  return !f || line.message.toLowerCase().includes(f) || line.target.toLowerCase().includes(f);
}
function renderLogLine(line) {
  const t = new Date(line.time).toLocaleTimeString();
  return el("div", { class: "log-line" + (line.message.startsWith("[dashboard] >") ? " cmd" : "") },
    el("span", { class: "t" }, t), el("span", { class: "lv lv-" + line.level }, line.level.toUpperCase()), line.message);
}
function addLogLines(lines) {
  const log = $("#log");
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  for (const line of lines) {
    if (line.seq <= state.logSeq) continue;
    state.logSeq = line.seq;
    state.logLines.push(line);
    if (logVisible(line)) log.append(renderLogLine(line));
  }
  if (state.logLines.length > 3000) {
    state.logLines.splice(0, state.logLines.length - 3000);
    while (log.childElementCount > 3000) log.firstChild.remove();
  }
  scrollLog(atBottom);
}
function rerenderLog() {
  $("#log").replaceChildren(...state.logLines.filter(logVisible).map(renderLogLine));
  scrollLog(true);
}
function scrollLog(force) { const log = $("#log"); if (force) log.scrollTop = log.scrollHeight; }
$("#log-level").addEventListener("change", rerenderLog);
$("#log-filter").addEventListener("input", rerenderLog);

// Polling, not a stream: Cloudflare quick tunnels don't pass server-sent events.
async function pollLogs() {
  try { addLogLines(await api("/logs?after=" + state.logSeq)); } catch (_) { /* retried next tick */ }
}

const history = [];
let historyPos = 0;
async function runCommand(command) {
  try {
    await api("/command", { method: "POST", body: JSON.stringify({ command }) });
  } catch (err) { toast(err.message, true); }
}
$("#command-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#command-input");
  const command = input.value.trim();
  if (!command) return;
  history.push(command); historyPos = history.length;
  input.value = "";
  await runCommand(command);
});
$("#command-input").addEventListener("keydown", (e) => {
  if (e.key === "ArrowUp" && historyPos > 0) { historyPos--; e.target.value = history[historyPos]; e.preventDefault(); }
  if (e.key === "ArrowDown") { historyPos = Math.min(history.length, historyPos + 1); e.target.value = history[historyPos] || ""; e.preventDefault(); }
});

/* Players */
async function loadPlayers() {
  let players;
  try { players = await api("/players"); } catch (_) { return; }
  $("#players-sub").textContent = `${players.length} online`;
  $("#players-empty").classList.toggle("hidden", players.length > 0);
  const action = (label, cmd, cls = "") => el("button", { class: "btn small " + cls, onclick: async () => {
    await runCommand(cmd); toast(`Ran: ${cmd}`); setTimeout(loadPlayers, 600);
  } }, label);
  $("#players-body").replaceChildren(...players.map((p) => el("tr", {},
    el("td", {}, el("div", { class: "player" }, el("span", { class: "tag on" }, p.name.slice(0, 1).toUpperCase()), p.name)),
    el("td", {}, el("span", { class: "tag" }, p.version)),
    el("td", {}, p.world.replace("minecraft:", "")),
    el("td", {}, p.gamemode),
    el("td", { class: "muted" }, p.position.join(", ")),
    el("td", {}, p.ping + " ms"),
    el("td", { class: "actions" },
      action("Op", `op ${p.name}`),
      action("Creative", `gamemode creative ${p.name}`),
      action("Survival", `gamemode survival ${p.name}`),
      action("Kick", `kick ${p.name} Kicked from the dashboard`, "danger"),
      action("Ban", `ban ${p.name} Banned from the dashboard`, "danger")),
  )));
}
$("#whitelist-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const name = $("#whitelist-name").value.trim();
  if (!name) return;
  await runCommand(`whitelist add ${name}`);
  toast(`Asked the server to whitelist ${name}`);
  $("#whitelist-name").value = "";
});

/* Server settings: a form built from pumpkin.toml */
async function loadSettings() {
  try { state.settings = await api("/config/pumpkin"); } catch (err) { toast(err.message, true); return; }
  fillProfile();
  const form = $("#settings-form");
  form.replaceChildren();
  const scalars = {};
  const tables = [];
  for (const [k, v] of Object.entries(state.settings)) {
    if (v && typeof v === "object" && !Array.isArray(v)) tables.push([k, v]); else scalars[k] = v;
  }
  if (Object.keys(scalars).length) form.append(renderSection("General", scalars, [], true));
  for (const [k, v] of tables) form.append(renderSection(pretty(k), v, [k], false));
}

function renderSection(title, obj, path, open) {
  const body = el("div", { class: "section-body" });
  for (const [k, v] of Object.entries(obj)) {
    const p = [...path, k];
    if (v && typeof v === "object" && !Array.isArray(v)) body.append(renderSection(pretty(k), v, p, false));
    else body.append(renderField(k, v, p));
  }
  const details = el("details", { class: "section" }, el("summary", {}, title), body);
  if (open) details.open = true;
  return details;
}

function renderField(key, value, path) {
  const id = "cfg-" + path.join("-");
  let input;
  if (typeof value === "boolean") {
    input = el("label", { class: "switch" }, el("input", { type: "checkbox", id }), el("span"));
    input.querySelector("input").checked = value;
  } else if (typeof value === "number") {
    input = el("input", { type: "number", id, step: Number.isInteger(value) ? "1" : "any", value });
  } else if (Array.isArray(value)) {
    const simple = value.every((x) => typeof x !== "object");
    input = el("textarea", { id, rows: Math.min(8, Math.max(2, value.length + 1)), "data-kind": simple ? "lines" : "json",
      placeholder: simple ? "One entry per line" : "JSON" });
    input.value = simple ? value.join("\n") : JSON.stringify(value, null, 2);
  } else {
    input = el("input", { id, value: value ?? "" });
  }
  (input.matches("input, textarea") ? input : input.querySelector("input")).dataset.path = JSON.stringify(path);
  const row = el("label", { class: "field", "data-search": path.join(".").toLowerCase() },
    el("span", {}, pretty(key), el("small", {}, path.join("."))), input);
  return row;
}

function collectSettings() {
  const out = structuredClone(state.settings);
  document.querySelectorAll("#settings-form [data-path]").forEach((node) => {
    const keys = JSON.parse(node.dataset.path);
    let target = out;
    for (const k of keys.slice(0, -1)) target = target[k];
    const last = keys[keys.length - 1];
    const old = target[last];
    if (typeof old === "boolean") target[last] = node.checked;
    else if (typeof old === "number") target[last] = node.value === "" ? old : Number(node.value);
    else if (Array.isArray(old)) {
      if (node.dataset.kind === "json") target[last] = JSON.parse(node.value || "[]");
      else {
        const items = node.value.split("\n").map((s) => s.trim()).filter(Boolean);
        target[last] = old.length && typeof old[0] === "number" ? items.map(Number) : items;
      }
    } else target[last] = node.value;
  });
  return out;
}

$("#settings-save").addEventListener("click", async () => {
  let body;
  try { body = collectSettings(); } catch (err) { toast("Couldn't read the form: " + err.message, true); return; }
  try {
    await api("/config/pumpkin", { method: "PUT", body: JSON.stringify(body) });
    state.settings = body;
    state.restartPending = true;
    $("#restart-banner").classList.remove("hidden");
    fillProfile();
    toast("pumpkin.toml saved");
  } catch (err) { toast(err.message, true); }
});
$("#settings-search").addEventListener("input", (e) => {
  const q = e.target.value.trim().toLowerCase();
  document.querySelectorAll("#settings-form .field").forEach((f) => f.classList.toggle("hidden", q && !f.dataset.search.includes(q)));
  document.querySelectorAll("#settings-form details").forEach((d) => {
    if (q) d.open = d.querySelector(".field:not(.hidden)") !== null;
    d.classList.toggle("hidden", q && !d.querySelector(".field:not(.hidden)"));
  });
});

/* Server profile: the common settings, stored in the same pumpkin.toml */
function fillProfile() {
  const c = state.settings;
  const java = c.networking.java;
  $("#profile-motd").value = java.motd;
  $("#profile-motd-preview").textContent = java.motd;
  $("#profile-max").value = java.max_players;
  $("#profile-difficulty").value = c.default_difficulty;
  $("#profile-gamemode").value = c.default_gamemode;
  $("#profile-view").value = java.view_distance;
  $("#profile-whitelist").checked = c.white_list;
}

$("#profile-motd").addEventListener("input", (e) => { $("#profile-motd-preview").textContent = e.target.value; });
$("#profile-save").addEventListener("click", async () => {
  if (!state.settings) return;
  const body = structuredClone(state.settings);
  body.networking.java.motd = $("#profile-motd").value;
  if (body.networking.bedrock) body.networking.bedrock.motd = $("#profile-motd").value;
  body.networking.java.max_players = Number($("#profile-max").value);
  body.default_difficulty = $("#profile-difficulty").value;
  body.default_gamemode = $("#profile-gamemode").value;
  body.networking.java.view_distance = Number($("#profile-view").value);
  body.white_list = $("#profile-whitelist").checked;
  try {
    await api("/config/pumpkin", { method: "PUT", body: JSON.stringify(body) });
    state.settings = null;
    await loadSettings();
    state.restartPending = true;
    $("#restart-banner").classList.remove("hidden");
    toast("Server profile saved");
  } catch (err) { toast(err.message, true); }
});

async function loadIcon() {
  try {
    const res = await fetch("/api/server/icon", { headers: { Authorization: "Bearer " + token } });
    if (res.ok) $("#profile-icon").src = URL.createObjectURL(await res.blob());
  } catch (_) { /* the default logo stays */ }
}

/** Any image becomes the 64 by 64 PNG Minecraft wants. */
async function iconPng(file) {
  const bitmap = await createImageBitmap(file);
  const canvas = el("canvas", { width: 64, height: 64 });
  const size = Math.min(bitmap.width, bitmap.height);
  canvas.getContext("2d").drawImage(bitmap, (bitmap.width - size) / 2, (bitmap.height - size) / 2, size, size, 0, 0, 64, 64);
  return new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
}

$("#profile-icon-pick").addEventListener("click", () => $("#profile-icon-input").click());
$("#profile-icon-input").addEventListener("change", async (e) => {
  const file = e.target.files[0];
  e.target.value = "";
  if (!file) return;
  try {
    const png = await iconPng(file);
    const res = await fetch("/api/server/icon", { method: "PUT", headers: { Authorization: "Bearer " + token }, body: png });
    if (!res.ok) throw new Error(await res.text());
    $("#profile-icon").src = URL.createObjectURL(png);
    state.settings = null;
    await loadSettings();
    toast("Icon saved. Refresh the server list to see it");
  } catch (err) { toast("Couldn't use that image: " + err.message, true); }
});

/* Accounts */
async function loadUsers() {
  let data;
  try { data = await api("/users"); } catch (_) { return; }
  $("#user-list").replaceChildren(...data.users.map((name) => {
    const self = name.toLowerCase() === data.current.toLowerCase();
    const buttons = [el("button", { class: "btn small", onclick: () => changePassword(name, self) }, "Change password")];
    if (!self) buttons.push(el("button", { class: "btn small danger", onclick: () => removeUser(name) }, "Remove"));
    return el("li", {}, el("span", {}, name + (self ? " (you)" : "")), el("span", { class: "row" }, buttons));
  }));
}

async function changePassword(username, self) {
  const password = prompt(`New password for ${username}`);
  if (!password) return;
  try {
    await api("/users/password", { method: "POST", body: JSON.stringify({ username, password }) });
    toast("Password changed");
    if (self) { setSession(""); showLogin("Sign in with your new password."); }
  } catch (err) { toast(err.message, true); }
}

async function removeUser(username) {
  if (!confirm(`Remove the account ${username}?`)) return;
  try {
    await api("/users/delete", { method: "POST", body: JSON.stringify({ username }) });
    loadUsers();
  } catch (err) { toast(err.message, true); }
}

$("#user-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const body = { username: $("#user-name").value.trim(), password: $("#user-password").value };
  try {
    await api("/users", { method: "POST", body: JSON.stringify(body) });
    $("#user-name").value = "";
    $("#user-password").value = "";
    toast(`Account ${body.username} added`);
    loadUsers();
  } catch (err) { toast(err.message, true); }
});

/* PumpkinX features */
let featureConfig = null;
async function loadFeatures() {
  let cfg, s;
  try { [cfg, s] = await Promise.all([api("/config/pumpkinx"), api("/status")]); } catch (_) { return; }
  featureConfig = cfg;
  $("#f-multiversion").checked = cfg.multiversion.enabled;
  $("#f-patchbukkit").checked = cfg.patchbukkit.enabled;
  $("#f-keystone").checked = cfg.keystone.enabled;
  $("#f-keystone").disabled = !s.keystone.embedded;
  $("#ks-state").textContent = !s.keystone.embedded ? "This build was made without Keystone." : s.keystone.running ? "Running now." : "Not running.";
  $("#f-dashboard").checked = cfg.dashboard.enabled;
  $("#f-address").value = cfg.dashboard.address;
  $("#f-files").checked = cfg.dashboard.file_manager;
  $("#mv-range").textContent = s.multiversion.range;
  $("#mv-state").textContent = s.multiversion.running ? "Running now." : "Not running.";
  const pb = s.patchbukkit;
  $("#pb-dir").textContent = pb.plugin_dir;
  const pbToggle = $("#f-patchbukkit");
  pbToggle.disabled = !pb.compiled_in;
  let pbText = pb.running ? "Running now." : "Not running.";
  if (!pb.compiled_in) pbText = "This build was made without PatchBukkit.";
  else if (!pb.java.found) pbText += " No Java found on this machine; install Java 25 first.";
  else pbText += ` Java: ${pb.java.version}`;
  $("#pb-state").textContent = pbText;
  $("#pb-jars").replaceChildren(...pb.jars.map((j) => el("li", {}, el("span", {}, j), el("span", { class: "tag" }, "jar"))));
}
$("#features-save").addEventListener("click", async () => {
  if (!featureConfig) return;
  const cfg = structuredClone(featureConfig);
  cfg.multiversion.enabled = $("#f-multiversion").checked;
  cfg.patchbukkit.enabled = $("#f-patchbukkit").checked;
  cfg.keystone.enabled = $("#f-keystone").checked;
  cfg.dashboard.enabled = $("#f-dashboard").checked;
  cfg.dashboard.address = $("#f-address").value.trim();
  cfg.dashboard.file_manager = $("#f-files").checked;
  try {
    await api("/config/pumpkinx", { method: "PUT", body: JSON.stringify(cfg) });
    featureConfig = cfg;
    toast("pumpkinx.toml saved");
    refreshStatus();
  } catch (err) { toast(err.message, true); }
});

/* Files */
const files = { path: "", open: null, dirty: false };
const join = (dir, name) => (dir ? dir + "/" + name : name);
const q = (path) => "path=" + encodeURIComponent(path);

async function loadFiles(path) {
  let data;
  try { data = await api("/files?" + q(path)); } catch (err) { if (err.message !== "unauthorized") toast(err.message, true); return; }
  files.path = data.path;
  const crumbs = [el("a", { onclick: () => loadFiles("") }, "server")];
  let walked = "";
  for (const part of data.path ? data.path.split("/") : []) {
    walked = join(walked, part);
    const target = walked;
    crumbs.push(el("span", { class: "muted" }, "/"), el("a", { onclick: () => loadFiles(target) }, part));
  }
  $("#files-crumbs").replaceChildren(...crumbs);
  $("#files-empty").classList.toggle("hidden", data.entries.length > 0);
  $("#files-body").replaceChildren(...data.entries.map((f) => fileRow(f, data.in_trash)));
}

function fileRow(f, inTrash) {
  const path = join(files.path, f.name);
  const open = () => (f.is_dir ? loadFiles(path) : openFile(path));
  const actions = [];
  if (!f.is_dir) actions.push(el("button", { class: "btn small", onclick: () => downloadFile(path, f.name) }, "Download"));
  if (!f.read_only && !f.is_symlink) {
    actions.push(el("button", { class: "btn small", onclick: () => renameFile(path, f.name) }, "Rename"));
    actions.push(el("button", { class: "btn small danger", onclick: () => deleteFile(path, f.name, inTrash) }, inTrash ? "Delete forever" : "Delete"));
  }
  return el("tr", {},
    el("td", {}, el("a", { class: "file-name" + (f.is_dir ? " dir" : ""), onclick: open }, (f.is_dir ? "▸ " : "") + f.name),
      f.read_only ? el("span", { class: "tag" }, "settings page") : ""),
    el("td", { class: "muted" }, f.is_dir ? "" : fmtBytes(f.size)),
    el("td", { class: "muted" }, f.modified ? new Date(f.modified).toLocaleString() : ""),
    el("td", { class: "actions" }, actions));
}

async function openFile(path) {
  if (!leaveEditor()) return;
  let data;
  try { data = await api("/files/read?" + q(path)); } catch (err) { toast(err.message, true); return; }
  files.open = { path, modified: data.modified, readOnly: data.read_only };
  files.dirty = false;
  $("#editor-name").textContent = path;
  $("#editor-text").value = data.content;
  $("#editor-text").readOnly = data.read_only;
  $("#editor-save").classList.toggle("hidden", data.read_only);
  $("#editor-state").textContent = data.read_only ? "read only" : "";
  $("#file-editor").classList.remove("hidden");
  $("#file-editor").scrollIntoView({ behavior: "smooth", block: "nearest" });
}

function leaveEditor() {
  return !files.dirty || confirm("Discard your unsaved changes?");
}

function closeEditor() {
  if (!leaveEditor()) return;
  files.open = null;
  files.dirty = false;
  $("#file-editor").classList.add("hidden");
}

async function saveFile() {
  if (!files.open || files.open.readOnly) return;
  try {
    const res = await api("/files/write", { method: "PUT", body: JSON.stringify({ path: files.open.path, content: $("#editor-text").value, modified: files.open.modified }) });
    files.open.modified = res.modified;
    files.dirty = false;
    $("#editor-state").textContent = "saved";
    loadFiles(files.path);
  } catch (err) { toast(err.message, true); }
}

async function downloadFile(path, name) {
  try {
    const res = await fetch("/api/files/download?" + q(path), { headers: { Authorization: "Bearer " + token } });
    if (!res.ok) throw new Error(await res.text());
    const url = URL.createObjectURL(await res.blob());
    const link = el("a", { href: url, download: name });
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
  } catch (err) { toast(err.message, true); }
}

async function renameFile(path, name) {
  const next = prompt("New name", name);
  if (!next || next === name) return;
  try {
    await api("/files/rename", { method: "POST", body: JSON.stringify({ from: path, to: join(files.path, next) }) });
    loadFiles(files.path);
  } catch (err) { toast(err.message, true); }
}

async function deleteFile(path, name, inTrash) {
  const message = inTrash ? `Permanently delete ${name}? This can't be undone.` : `Move ${name} to the trash? Don't do this to world files while the server runs.`;
  if (!confirm(message)) return;
  try {
    await api("/files?" + q(path), { method: "DELETE" });
    toast(inTrash ? `${name} deleted` : `${name} moved to .pumpkinx-trash`);
    if (files.open && files.open.path === path) { files.dirty = false; closeEditor(); }
    loadFiles(files.path);
  } catch (err) { toast(err.message, true); }
}

// Sent in pieces, because Cloudflare refuses requests over 100 MB.
const UPLOAD_CHUNK = 32 * 1024 * 1024;

async function sendFile(file, path, overwrite) {
  const progress = $("#files-progress");
  let offset = 0;
  do {
    const part = file.slice(offset, offset + UPLOAD_CHUNK);
    const res = await fetch(`/api/files/upload?${q(path)}&overwrite=${overwrite}&offset=${offset}&total=${file.size}`,
      { method: "PUT", headers: { Authorization: "Bearer " + token }, body: part });
    if (!res.ok) return res;
    offset += part.size;
    progress.textContent = `Uploading ${file.name}: ${file.size ? Math.floor((offset / file.size) * 100) : 100}%`;
  } while (offset < file.size);
  return { ok: true, status: 200 };
}

async function uploadFiles(list) {
  const progress = $("#files-progress");
  progress.classList.remove("hidden");
  for (const file of list) {
    const path = join(files.path, file.name);
    try {
      progress.textContent = `Uploading ${file.name}`;
      let res = await sendFile(file, path, false);
      if (res.status === 409) {
        if (!confirm(`${file.name} already exists. Replace it?`)) continue;
        res = await sendFile(file, path, true);
      }
      if (!res.ok) throw new Error(await res.text());
      toast(`${file.name} uploaded`);
    } catch (err) { toast(`${file.name}: ${err.message}`, true); }
  }
  progress.classList.add("hidden");
  loadFiles(files.path);
}

$("#files-upload").addEventListener("click", () => $("#files-input").click());
$("#files-input").addEventListener("change", (e) => { uploadFiles([...e.target.files]); e.target.value = ""; });
$("#files-new-folder").addEventListener("click", async () => {
  const name = prompt("Folder name");
  if (!name) return;
  try {
    await api("/files/mkdir", { method: "POST", body: JSON.stringify({ path: join(files.path, name) }) });
    loadFiles(files.path);
  } catch (err) { toast(err.message, true); }
});
$("#files-new-file").addEventListener("click", async () => {
  const name = prompt("File name");
  if (!name) return;
  const path = join(files.path, name);
  try {
    // A write without `modified` would replace an existing file, so check first.
    const listing = await api("/files?" + q(files.path));
    if (listing.entries.some((f) => f.name === name)) { toast("That name is already in use", true); return; }
    await api("/files/write", { method: "PUT", body: JSON.stringify({ path, content: "" }) });
    await loadFiles(files.path);
    openFile(path);
  } catch (err) { toast(err.message, true); }
});
$("#editor-save").addEventListener("click", saveFile);
$("#editor-close").addEventListener("click", closeEditor);
$("#editor-text").addEventListener("input", () => { files.dirty = true; $("#editor-state").textContent = "unsaved"; });
$("#editor-text").addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "s") { e.preventDefault(); saveFile(); }
});
const drop = $("#files-drop");
drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("dragging"); });
drop.addEventListener("dragleave", () => drop.classList.remove("dragging"));
drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("dragging"); uploadFiles([...e.dataTransfer.files]); });

/* Plugins */
async function loadPlugins() {
  let data;
  try { data = await api("/plugins"); } catch (_) { return; }
  const list = $("#plugins-list");
  if (!data.loaded.length) list.replaceChildren(el("p", { class: "empty" }, "No plugins loaded."));
  else list.replaceChildren(...data.loaded.map((p) => el("div", { class: "card plugin" },
    el("h3", {}, p.name, el("span", { class: "tag " + (p.active ? "on" : "off") }, p.active ? "active" : "inactive")),
    el("p", { class: "muted small" }, `v${p.version}${p.authors.length ? " · " + p.authors.join(", ") : ""}`),
    el("p", {}, p.description || ""))));
  $("#plugins-failed").replaceChildren(...data.failed.map((f) => el("div", { class: "card" },
    el("h3", { class: "badc" }, `${f.name} failed`), el("p", { class: "muted" }, f.error))));
}

/* Restart */
async function restart() {
  if (!confirm("Stop the server now? Under the systemd service it starts again on its own.")) return;
  try {
    await api("/server/stop", { method: "POST" });
    toast("Server is stopping");
    state.restartPending = false;
    $("#restart-banner").classList.add("hidden");
    setOnline(false);
  } catch (err) { toast(err.message, true); }
}
document.querySelectorAll("[data-action=restart]").forEach((b) => b.addEventListener("click", restart));
$("#banner-restart").addEventListener("click", restart);

/* Boot */
function start() {
  $("#login").classList.add("hidden");
  $("#app").classList.remove("hidden");
  timers.forEach(clearInterval);
  refreshStatus();
  pollLogs();
  timers = [
    setInterval(refreshStatus, 2000),
    setInterval(pollLogs, 1500),
    setInterval(() => { if (state.page === "players") loadPlayers(); }, 4000),
  ];
  let page = "overview";
  try { page = localStorage.getItem("pumpkinx-page") || "overview"; } catch (_) { /* storage blocked */ }
  showPage(document.getElementById("page-" + page) ? page : "overview");
}

if (token) api("/status").then(start).catch(() => {}); else showLogin();
