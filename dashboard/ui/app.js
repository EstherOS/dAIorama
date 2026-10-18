// dAIorama dashboard UI. Read-only; tokens never reach this page.
// Every API string is written with textContent (h() uses createTextNode): a session label or a
// history detail is untrusted input, and innerHTML in an operator page is the wrong default.
const state = {
  managers: [],
  manager: null,
  sessions: [],
  session: null,
  cursor: null,
  lastRecord: null,
  historyLoading: false,
  historyEnd: false,
  timer: null,
};

async function getJSON(url) {
  const response = await fetch(url, { headers: { accept: "application/json" } });
  const body = await response.json().catch(() => ({ error: "invalid JSON" }));
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

function el(id) { return document.getElementById(id); }
function short(value) { return value === undefined || value === null || value === "" ? "—" : String(value); }

/** DOM builder: attributes are properties/text, children are text nodes unless already nodes. */
function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === undefined || value === null) continue;
    if (key === "class") node.className = String(value);
    else if (key === "style") node.style.cssText = String(value);
    else if (key.startsWith("on") && typeof value === "function") node[key] = value;
    else if (key === "tabIndex") node.tabIndex = value;
    else node.setAttribute(key, String(value));
  }
  for (const child of children.flat()) {
    if (child === undefined || child === null) continue;
    node.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function clear(node) { while (node.firstChild) node.removeChild(node.firstChild); }

/** Rendered truth for the view column: "up" is a running front, "down" is a live session without
 * one, and a dead/closed session is neither — it is over, so it shows an em dash. */
function viewState(session) {
  if (session.state === "dead" || session.state === "closed") {
    return { text: "—", className: "muted", title: `session is ${session.state}` };
  }
  if (session.view) {
    return { text: "up", className: "good", title: "view front is running" };
  }
  return { text: "down", className: "warn", title: "running session; no view started" };
}

function stateClass(value) {
  return value === "running" ? "good" : value === "degraded" ? "warn" : value === "dead" || value === "closed" ? "bad" : "muted";
}

function renderManagers() {
  const list = el("manager-list");
  clear(list);
  for (const manager of state.managers) {
    const selected = manager.name === state.manager;
    list.appendChild(h("li", {
      class: selected ? "selected" : "",
      tabIndex: 0,
      role: "button",
      "aria-pressed": selected ? "true" : "false",
      onclick: () => selectManager(manager.name),
      onkeydown: (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectManager(manager.name); } },
    },
      h("strong", {}, manager.name),
      h("br"),
      h("span", { class: "muted" }, manager.address),
      h("br"),
      manager.ok
        ? h("span", {},
            h("span", { class: "good" }, manager.readiness?.state ?? "?"),
            ` · v${manager.readiness?.versions?.protocol ?? "?"} · ${manager.readiness?.sessions?.running ?? 0} running`)
        : h("span", { class: "bad" }, manager.error ?? "unreachable"),
    ));
  }
  const reachable = state.managers.filter((manager) => manager.ok).length;
  el("summary").textContent = state.managers.length
    ? `${reachable}/${state.managers.length} managers reachable`
    : "no managers configured";
}

async function refreshManagers() {
  try {
    state.managers = (await getJSON("/api/managers")).managers ?? [];
  } catch (error) {
    state.managers = [{ name: "dashboard", address: "—", ok: false, error: String(error) }];
  }
  if (state.manager && !state.managers.some((manager) => manager.name === state.manager)) {
    state.manager = null;
    state.sessions = [];
    state.session = null;
  }
  // A single configured manager is the common operator setup: pick it (preferring a reachable
  // one) instead of making the first screen an empty "pick a manager" prompt.
  if (!state.manager && state.managers.length) {
    state.manager = (state.managers.find((manager) => manager.ok) ?? state.managers[0]).name;
  }
  renderManagers();
}

function selectManager(name) {
  state.manager = name;
  state.session = null;
  state.sessions = [];
  resetHistory();
  renderManagers();
  renderSessions();
  renderDetail(null);
  loadSessions();
}

function renderSessions() {
  const rows = el("session-rows");
  clear(rows);
  for (const session of state.sessions) {
    const view = viewState(session);
    const selected = session.id === state.session;
    rows.appendChild(h("tr", {
      class: selected ? "selected" : "",
      tabIndex: 0,
      "aria-selected": selected ? "true" : "false",
      onclick: () => selectSession(session.id),
      onkeydown: (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectSession(session.id); } },
    },
      h("td", {}, short(session.id)),
      h("td", {}, short(session.label)),
      h("td", { class: stateClass(session.state) }, short(session.state)),
      h("td", { class: view.className, title: view.title }, view.text),
      h("td", {}, short(session.scope)),
      h("td", {}, `${session.resolution ?? "?"} @ ${session.scale ?? "?"}`),
      h("td", {}, short(session.idle_since)),
    ));
  }
  el("sessions-for").textContent = state.manager ? `· ${state.manager}` : "";
  el("sessions-note").textContent = !state.manager
    ? "pick a manager"
    : (state.sessions.length ? "" : "no sessions for this manager");
}

async function loadSessions() {
  if (!state.manager) { state.sessions = []; renderSessions(); return; }
  try {
    state.sessions = (await getJSON(`/api/sessions?manager=${encodeURIComponent(state.manager)}`)).sessions ?? [];
  } catch (error) {
    state.sessions = [];
    el("sessions-note").textContent = String(error);
  }
  renderSessions();
  if (state.session) {
    const session = state.sessions.find((candidate) => candidate.id === state.session);
    if (session) renderDetail(session);
  }
}

function selectSession(id) {
  state.session = id;
  renderSessions();
  renderDetail(state.sessions.find((session) => session.id === id) ?? null);
  resetHistory();
  loadHistory();
}

function renderDetail(session) {
  const body = el("detail-body");
  clear(body);
  const button = el("open-view");
  if (!session) {
    el("detail-id").textContent = "";
    body.appendChild(h("span", { class: "muted" }, "Pick a session."));
    button.disabled = true;
    el("view-note").textContent = "";
    return;
  }
  el("detail-id").textContent = session.id;
  const view = viewState(session);
  body.appendChild(h("div", {}, h("strong", {}, "state "), h("span", { class: stateClass(session.state) }, short(session.state))));
  body.appendChild(h("div", {}, h("strong", {}, "view "), h("span", { class: view.className, title: view.title }, view.text),
    h("span", { class: "muted" }, session.state === "dead" || session.state === "closed"
      ? ` (${session.state} — no view can start)`
      : view.text === "down" ? " (view down — session still running)" : "")));
  body.appendChild(h("div", {}, h("strong", {}, "backend "), short(session.backend),
    " · ", h("strong", {}, "scope "), short(session.scope),
    " · ", h("strong", {}, "origin "), short(session.origin)));
  body.appendChild(h("div", {}, h("strong", {}, "size "), `${session.resolution ?? "?"} @ ${session.scale ?? "?"}`,
    " · ", h("strong", {}, "renderer "), short(session.renderer)));

  const over = session.state === "dead" || session.state === "closed";
  button.disabled = over;
  el("view-note").textContent = over ? `session ${session.state} — view cannot start` : "opens the session's own view front";
}

function resetHistory() {
  clear(el("history"));
  el("history-note").textContent = "";
  state.cursor = null;
  state.lastRecord = null;
  state.historyEnd = false;
}

async function loadHistory() {
  if (!state.manager || !state.session || state.historyLoading || state.historyEnd) return;
  state.historyLoading = true;
  el("older").disabled = true;
  el("history-note").textContent = "loading…";
  const query = new URLSearchParams({ manager: state.manager, session: state.session, limit: "50" });
  if (state.cursor) query.set("cursor", state.cursor);
  try {
    const page = await getJSON(`/api/history?${query}`);
    for (const record of page.records ?? []) {
      // The cursor defines page boundaries; the only guard needed is against a literal repeat
      // of the immediately previous record (never a coarse timestamp key, which would drop
      // distinct same-second events).
      const serialized = JSON.stringify(record);
      if (serialized === state.lastRecord) continue;
      state.lastRecord = serialized;
      el("history").appendChild(h("li", {},
        h("span", { class: "badge" }, record.event),
        " ",
        h("span", { class: "muted" }, record.at),
        h("br"),
        h("span", { class: "muted" }, `scope ${record.scope} · origin ${record.origin}${record.cause ? ` · cause ${record.cause}` : ""}`),
      ));
    }
    state.cursor = page.next_cursor ?? null;
    state.historyEnd = !state.cursor;
    el("history-note").textContent = state.historyEnd
      ? (el("history").childElementCount ? "end of history" : "no history for this session")
      : "";
  } catch (error) {
    el("history-note").textContent = String(error);
  } finally {
    state.historyLoading = false;
    el("older").disabled = !state.cursor || state.historyLoading;
  }
}

async function openView() {
  if (!state.manager || !state.session) return;
  el("view-note").textContent = "starting…";
  try {
    const response = await fetch("/api/view", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ manager: state.manager, session: state.session }),
    });
    const view = await response.json();
    if (!response.ok || view.error) throw new Error(view.error || `HTTP ${response.status}`);
    el("view-note").textContent = `generation ${view.generation ?? "?"} · opened`;
    window.open(viewTarget(view), "_blank", "noopener");
  } catch (error) {
    el("view-note").textContent = String(error);
  }
}

/** The view URL to open. `get_view` answers an address that already carries the credential
 * (05) *and* the credential as its own field, so appending blindly put two copies of the
 * secret in the URL. Setting the parameter on the parsed address keeps exactly one. */
function viewTarget(view) {
  const base = view.address?.includes("://") ? view.address : `http://${view.address}`;
  try {
    const url = new URL(base);
    if (view.credential) url.searchParams.set("credential", view.credential);
    return url.toString();
  } catch {
    return base;
  }
}

el("open-view").onclick = openView;
el("older").onclick = () => loadHistory();

async function tick() {
  await refreshManagers();
  if (state.manager) await loadSessions();
}
tick();
state.timer = setInterval(tick, 5000);
