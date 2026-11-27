const { invoke } = window.__TAURI__.core;
const $ = (s) => document.querySelector(s);
const esc = (t) => String(t ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const toast = (m) => { const t = $("#toast"); t.textContent = m; t.classList.add("show"); setTimeout(() => t.classList.remove("show"), 2500); };
// prompt()/confirm() no funcionan en el webview: diálogo propio. Devuelve null si se cancela, o los valores de los campos.
function ask(title, fields = [], okText = "Aceptar") {
  return new Promise((res) => {
    const d = document.createElement("dialog");
    d.innerHTML = `<form method="dialog" class="dlg"><b>${esc(title)}</b>${fields.map((f, i) => `<label>${esc(f)}<input type="password" data-i="${i}" autocomplete="off"></label>`).join("")}<div class="row" style="justify-content:flex-end"><button class="btn" value="no">Cancelar</button><button class="btn pri" value="ok">${esc(okText)}</button></div></form>`;
    document.body.append(d);
    d.addEventListener("close", () => { const ok = d.returnValue === "ok"; const v = [...d.querySelectorAll("input")].map((i) => i.value); d.remove(); res(ok ? v : null); });
    d.showModal();
    d.querySelector("input")?.focus();
  });
}
// Instalar la extensión en un navegador/perfil: diálogo modal con pasos según navegador.
async function installExtDialog() {
  const browsers = await call("list_browsers");
  const d = document.createElement("dialog");
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  if (!browsers.length) {
    d.innerHTML = `<form method="dialog" class="dlg"><b>Instalar en navegador</b><div class="sub">No se detectó ningún navegador compatible</div><div class="row" style="justify-content:flex-end"><button class="btn pri" value="no">Cerrar</button></div></form>`;
    d.showModal(); return;
  }
  const SCHEME = { edge: "edge", brave: "brave" };
  let path = "";
  d.innerHTML = `<form method="dialog" class="dlg"><b>Instalar en navegador</b>
    <label for="ib-b">Navegador<select id="ib-b">${browsers.map((b, i) => `<option value="${i}">${esc(b.name)}</option>`).join("")}</select></label>
    <label for="ib-p" id="ib-pl">Perfil<select id="ib-p"></select></label>
    <ol id="ib-steps" class="sub" style="margin:0;padding-left:18px"></ol>
    <div id="ib-warn" class="sub" hidden></div>
    <div id="ib-man" hidden><label for="ib-path">Ruta (no se pudo copiar; cópiala manualmente)<input id="ib-path" readonly></label></div>
    <div class="row" style="justify-content:flex-end"><button class="btn" value="no">Cancelar</button><button class="btn pri" type="button" id="ib-go">Abrir y copiar ruta</button></div></form>`;
  const cur = () => browsers[+$("#ib-b").value] || browsers[0];
  const steps = () => {
    const b = cur(), p = path ? `<b>${esc(path)}</b>` : "(se copiará al abrir)";
    $("#ib-steps").innerHTML = b.id === "firefox"
      ? `<li>Se abrirá about:debugging.</li><li>Pulsa «Este Firefox» → «Cargar complemento temporal…».</li><li>Elige el archivo manifest.firefox.json (ruta ya copiada): ${p}</li><li>Es temporal: se pierde al cerrar Firefox.</li>`
      : `<li>Se abrirá ${esc(SCHEME[b.id] || "chrome")}://extensions en ese perfil.</li><li>Activa «Modo de desarrollador».</li><li>Pulsa «Cargar descomprimida» y pega la ruta (ya copiada): ${p}</li>`;
  };
  const profiles = () => {
    const ps = cur().profiles || [];
    $("#ib-p").innerHTML = ps.map((p, i) => `<option value="${i}">${esc(p.email ? `${p.name} (${p.email})` : p.name)}</option>`).join("");
    $("#ib-pl").hidden = ps.length <= 1;
    path = ""; $("#ib-man").hidden = true; $("#ib-warn").hidden = true; steps();
  };
  $("#ib-b").onchange = profiles;
  $("#ib-go").onclick = async () => {
    const b = cur(), pr = (b.profiles || [])[+$("#ib-p").value];
    const btn = $("#ib-go"); btn.disabled = true;
    try {
      const r = await call("install_extension", { browser: b.id, profile: pr ? pr.dir : null });
      path = r.path;
      try { await navigator.clipboard.writeText(path); toast("Ruta copiada"); }
      catch { $("#ib-man").hidden = false; const i = $("#ib-path"); i.value = path; i.focus(); i.select(); }
      steps();
      const warn = r.opened ? (r.note || "") : `No se pudo abrir el navegador automáticamente. Abre ${SCHEME[b.id] || "chrome"}://extensions en el perfil ${pr ? pr.name : "activo"} y sigue los pasos con la ruta copiada.`;
      $("#ib-warn").textContent = warn; $("#ib-warn").hidden = !warn;
    } catch {} finally { btn.disabled = false; }
  };
  profiles();
  d.showModal();
  $("#ib-b").focus();
}
// Copia de seguridad (.spoolguard.json): exportar por categorías e importar con vista previa. Mismo flujo en escritorio y en el panel web.
const BK_CATS = [
  ["reglas", "Reglas y restricciones", "Por impresora y globales (aplicaciones, sitios, IP, modo)"],
  ["parametros", "Parámetros", "Control de desconocidos, intervalo, notificaciones, tema y retención"],
  ["bloqueo_rapido", "Bloqueo rápido", "Equipos bloqueados e impresoras pausadas (solo servidor)"],
  ["registro", "Registro de eventos", "Con rango de fechas opcional"],
  ["inventario", "Inventario de impresoras", "Nombre, tipo de conexión y puerto (informativo al importar)"],
];
const bkName = Object.fromEntries(BK_CATS.map(([k, t]) => [k, t]));
const bkDate = (v, end) => v ? Math.floor(new Date(v + (end ? "T23:59:59" : "T00:00:00")).getTime() / 1000) : null;
function bkDialog() {
  const d = document.createElement("dialog");
  d.className = "wide";
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  return d;
}
async function exportBackupDialog() {
  const d = bkDialog();
  d.innerHTML = `<form method="dialog" class="dlg"><b>Exportar copia de seguridad</b>
    <div class="sub">Elige qué incluir. El archivo no lleva contraseña, tokens de equipo o de extensión, ni sesión: tras importarlo en otro equipo hay que definirlos de nuevo.</div>
    ${BK_CATS.filter(([k]) => k !== "bloqueo_rapido" || window.__MODO_SERVIDOR__).map(([k, t, s]) => `<label class="chk"><input type="checkbox" data-c="${k}" ${k === "inventario" ? "" : "checked"}><span><b>${esc(t)}</b><br><span class="sub">${esc(s)}</span></span></label>`).join("")}
    <div class="bk-reg" id="bk-reg"><label>Registro desde<input type="date" id="bk-from"></label><label>hasta<input type="date" id="bk-to"></label><label>Máx. eventos<input type="number" id="bk-max" min="1" max="50000" value="10000"></label></div>
    <span class="err" id="bk-err"></span>
    <div class="row" style="justify-content:flex-end"><button class="btn" value="no">Cancelar</button><button type="button" class="btn pri" id="bk-go">Exportar…</button></div></form>`;
  const reg = d.querySelector('[data-c="registro"]'), box = d.querySelector("#bk-reg");
  const sync = () => box.querySelectorAll("input").forEach((i) => i.disabled = !reg.checked);
  reg.onchange = sync; sync();
  d.querySelector("#bk-go").onclick = async (ev) => {
    const cats = [...d.querySelectorAll("[data-c]:checked")].map((i) => i.dataset.c), err = d.querySelector("#bk-err");
    if (!cats.length) { err.textContent = "Elige al menos una categoría"; return; }
    ev.target.disabled = true; err.textContent = "";
    try {
      const args = { categories: cats };
      if (cats.includes("registro")) Object.assign(args, { from: bkDate(d.querySelector("#bk-from").value, false), to: bkDate(d.querySelector("#bk-to").value, true), max_events: Math.min(50000, Math.max(1, +d.querySelector("#bk-max").value || 10000)) });
      const text = await invoke("export_backup", args);
      const name = `spoolguard-copia-${new Date().toISOString().slice(0, 10)}.spoolguard.json`;
      const p = await saveAs(name, text, "json");
      if (p) { toast("Guardado: " + p); d.close(); }
    } catch (e) { err.textContent = String(e); }
    finally { ev.target.disabled = false; }
  };
  d.showModal();
}
async function importBackupFlow() {
  const path = await dlg.open({ multiple: false, filters: [{ name: "Copia de SpoolGuard", extensions: ["json"] }] });
  if (!path) return;
  const json = await call("read_backup_file", { path });
  await importPreviewDialog(json);
}
async function importPreviewDialog(json) {
  let mode = "combinar", on = null, pv = await call("preview_import", { json, modo: "combinar" });
  on = new Set(pv.categorias.filter((c) => c !== "inventario" && (c !== "bloqueo_rapido" || window.__MODO_SERVIDOR__)));
  const modo = () => Object.fromEntries([...on].map((c) => [c, mode]));
  if (on.size) pv = await call("preview_import", { json, modo: modo() });
  const d = bkDialog();
  const line = (c) => {
    if (!on.has(c) && c !== "inventario") return pv.categorias.includes(c) ? '<span class="sub">No se importará</span>' : "";
    const r = pv[c];
    if (!r) return "";
    if (c === "reglas") return `${r.nuevas} nuevas, ${r.modificadas} modificadas, ${r.eliminadas} eliminadas, ${r.iguales} sin cambios · regla global: ${esc(r.global)}`;
    if (c === "parametros") return r.cambios.length ? r.cambios.map((x) => `${esc(x.campo)}: ${esc(x.de)} → ${esc(x.a)}`).join(" · ") : "Sin cambios";
    if (c === "bloqueo_rapido") return `${r.bloqueos_nuevos} bloqueos nuevos, ${r.pausadas_nuevas} impresoras pausadas nuevas, ${r.bloqueos_eliminados + r.pausadas_eliminadas} eliminados`;
    if (c === "registro") return `${r.a_importar} eventos a importar, ${r.duplicados} ya existen, ${r.descartados} descartados${r.a_borrar ? `; se borrarán ${r.a_borrar} actuales` : ""}`;
    return `${r.impresoras} impresoras (solo informativo)`;
  };
  const render = () => {
    const rules = on.has("reglas") && pv.reglas?.impresoras?.length ? `<div class="bk-list">${pv.reglas.impresoras.map((p) => `<div class="field"><span>${esc(p.nombre)}</span><span><span class="badge ${p.estado === "eliminada" ? "bad" : p.estado === "igual" ? "off" : "ok"}">${esc(p.estado)}</span>${p.existe ? "" : ' <span class="badge warn">no existe en este equipo</span>'}</span></div>`).join("")}</div>` : "";
    d.innerHTML = `<form method="dialog" class="dlg"><b>Importar copia de seguridad</b>
      <div class="sub">${pv.legacy ? "Archivo de reglas de una versión anterior" : `Creada ${pv.creado ? esc(fmt(pv.creado)) : "—"} · equipo ${esc(pv.equipo || "—")} · versión ${esc(pv.app_version || "—")}`}</div>
      ${pv.categorias.map((c) => `<label class="chk"><input type="checkbox" data-c="${c}" ${on.has(c) ? "checked" : ""} ${c === "inventario" || (c === "bloqueo_rapido" && !window.__MODO_SERVIDOR__) ? "disabled" : ""}><span><b>${esc(bkName[c])}</b><br><span class="sub">${line(c)}</span></span></label>`).join("") || '<div class="sub">El archivo no contiene nada que importar.</div>'}
      ${rules}
      <div class="field">Al importar ${segm("bk-mode", [["combinar", "Combinar"], ["reemplazar", "Reemplazar"]], mode)}</div>
      <div class="sub">${mode === "combinar" ? "Combinar suma lo del archivo a lo actual (listas unidas; activación y modo del archivo)." : "Reemplazar sustituye lo actual por lo del archivo; lo que no esté en él se elimina."} Antes de aplicar se guarda config.json.bak.</div>
      ${pv.errores.map((x) => `<div class="badge bad" style="white-space:normal">${esc(x)}</div>`).join("")}
      ${pv.avisos.map((x) => `<div class="badge warn" style="white-space:normal">${esc(x)}</div>`).join("")}
      <div class="row" style="justify-content:flex-end"><button class="btn" value="no">Cancelar</button><button type="button" class="btn pri" id="bk-ok" ${pv.ok && on.size ? "" : "disabled"}>Importar</button></div></form>`;
    d.querySelectorAll("[data-c]").forEach((i) => i.onchange = async () => { i.checked ? on.add(i.dataset.c) : on.delete(i.dataset.c); await refresh(); });
    d.querySelectorAll("#bk-mode button").forEach((b) => b.onclick = async (e) => { e.preventDefault(); mode = b.dataset.v; await refresh(); });
    d.querySelector("#bk-ok").onclick = apply;
  };
  const refresh = async () => { pv = await call("preview_import", { json, modo: modo() }); render(); };
  const apply = async (ev) => {
    if (mode === "reemplazar" && !await confirmNative("Reemplazar sustituye lo actual por lo del archivo. Se guarda una copia previa (config.json.bak). ¿Continuar?", "Reemplazar")) return;
    ev.target.disabled = true;
    try {
      const r = await call("apply_import", { json, modo: modo() });
      d.close();
      toast(r.warning || `Importación completada${r.eventos_importados ? ` (${r.eventos_importados} eventos)` : ""}`);
      await load();
    } catch { ev.target.disabled = false; }
  };
  render();
  d.showModal();
}

// Diálogos y selector de archivos nativos del sistema.
const dlg = window.__TAURI__.dialog;
async function saveAs(name, content, ext) {
  const path = await dlg.save({ defaultPath: name, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
  if (!path) return null;
  await invoke("write_text_file", { path, content });
  return path;
}
const confirmNative = (msg, okLabel) => dlg.ask(msg, { title: "SpoolGuard", kind: "warning", okLabel, cancelLabel: "Cancelar" });
const IS_WIN = /Windows/.test(navigator.userAgent);
const call = async (cmd, args) => { try { return await invoke(cmd, args); } catch (e) { toast(String(e)); throw e; } };

const GLOBAL = "__global__", emptyRules = () => ({ enabled: false, mode: "block", apps: [], sites: [], ips: [] });
let globalRules = emptyRules(), printers = [], selected = GLOBAL, settings = {}, paused = false, log = { rows: [], total: 0, printers: [] }, filter = { state: "all", printer: "", q: "", from: "", to: "", user: "", host: "", page: 1, size: 25 };
let logTab = "events", who = { by: "user", rows: [] }, inv = [], net = { cidrs: [], direct: { enabled: false, interval_secs: 300 }, direct_available: false, direct_status: [] }, disc = { running: false, devices: [], ranges: [] }, discTimer = 0;

const KINDS = [
  ["apps", "Aplicaciones", "Nombre de app o patrón (*.docx)"],
  ["sites", "Sitios web", "dominio.cl"],
  ["ips", "IP / equipos", "192.168.1.0/24"],
];

function badge(p) {
  const r = p.rules;
  if (!r.enabled && globalRules.enabled) return ""; // la regla global ya se ve en el chip «Global · N»
  if (!r.enabled) return '<span class="badge off">— Sin control</span>';
  if (paused) return '<span class="badge warn">⏸ Pausado</span>';
  return r.mode === "allow" ? '<span class="badge ok">✓ Modo permitir</span>' : '<span class="badge bad">✕ Modo bloqueo</span>';
}
// Reglas suspendidas: r.paused[patrón] = instante Unix de reanudación (0 = a mano); vencida, vuelve a aplicar sola.
const isPaused = (r, v) => { const t = r.paused?.[v]; return t !== undefined && (t === 0 || t > Date.now() / 1000); };
const pausedLabel = (r, v) => { const t = r.paused[v]; return t ? "hasta " + new Date(t * 1000).toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" }) : "hasta reanudar"; };
const nRules = (r) => KINDS.reduce((n, [k]) => n + r[k].filter((v) => !isPaused(r, v)).length, 0);

// Pregunta cuánto suspender una regla; devuelve el instante Unix (0 = hasta reanudar a mano) o null si se cancela.
function pauseDialog(v) {
  return new Promise((res) => {
    const d = document.createElement("dialog");
    document.body.append(d);
    const sec = (m) => Math.floor(Date.now() / 1000) + m * 60;
    const tomorrow = new Date(); tomorrow.setDate(tomorrow.getDate() + 1); tomorrow.setHours(8, 0, 0, 0);
    const opts = [["15 minutos", sec(15)], ["1 hora", sec(60)], ["4 horas", sec(240)], ["Hasta mañana 08:00", Math.floor(tomorrow / 1000)], ["Hasta reanudar a mano", 0]];
    d.innerHTML = `<form method="dialog" class="dlg"><b>Suspender «${esc(v)}»</b><div class="sub">La regla deja de aplicar y vuelve sola al terminar el plazo.</div>
      ${opts.map(([t], i) => `<button class="btn" type="button" data-o="${i}">${t}</button>`).join("")}
      <div class="row" style="justify-content:flex-end"><button class="btn" value="no">Cancelar</button></div></form>`;
    let out = null;
    d.querySelectorAll("[data-o]").forEach((b) => b.onclick = () => { out = opts[b.dataset.o][1]; d.close(); });
    d.addEventListener("close", () => { d.remove(); res(out); });
    d.showModal();
  });
}

async function save(p) {
  const g = p.name === GLOBAL, cmd = g ? "set_global_rules" : "set_rules", args = g ? { rules: p.rules } : { printer: p.name, rules: p.rules };
  const ask = (what) => confirmNative(`Windows necesita permiso para administrar ${what}. ¿Concederlo ahora? (pedirá confirmación de administrador una vez)`, "Conceder");
  let w;
  try { w = await invoke(cmd, args); } catch (e) {
    if (g || !String(e).startsWith("ACCESS_DENIED") || !(await ask("esta impresora"))) { toast(String(e)); throw e; }
    await call("grant_printer_access", { printer: p.name });
    w = await call(cmd, args);
  }
  if (g) {
    // El global devuelve { warning, denied }: las colas sin permiso se pueden conceder y reintentar.
    if (w.denied.length && await ask(`estas impresoras: ${w.denied.join(", ")}`)) {
      for (const n of w.denied) await call("grant_printer_access", { printer: n });
      w = await call(cmd, args);
    }
    w = w.warning;
  }
  if (w) toast(w);
}

const KIND = { usb: "USB", red: "Red (Ethernet/WiFi)", wifi: "WiFi", compartida: "Compartida", otra: "Otra" };
const CONN = {
  lista: '<span class="badge ok">✓ Conectada</span>', ocupada: '<span class="badge warn">⏳ Imprimiendo</span>',
  detenida: '<span class="badge warn">⏸ Detenida</span>', desconectada: '<span class="badge bad">✕ Desconectada</span>',
  desconocido: '<span class="badge off">? Desconocido</span>',
};

const svg = (d) => `<svg class="ic" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${d}</svg>`;
const IC = {
  usb: svg('<circle cx="10" cy="7" r="1"/><circle cx="4" cy="20" r="1"/><circle cx="20" cy="18" r="1"/><path d="M4.7 19.3 19 5"/><path d="m21 3-3 1 2 2Z"/><path d="M9.26 7.68 5 12l2 5"/><path d="m10 14 5 2 3.5-3.5"/>'),
  red: svg('<rect x="16" y="16" width="6" height="6" rx="1"/><rect x="2" y="16" width="6" height="6" rx="1"/><rect x="9" y="2" width="6" height="6" rx="1"/><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/><path d="M12 12V8"/>'),
  wifi: svg('<path d="M12 20h.01"/><path d="M2 8.82a15 15 0 0 1 20 0"/><path d="M5 12.859a10 10 0 0 1 14 0"/><path d="M8.5 16.429a5 5 0 0 1 7 0"/>'),
  compartida: svg('<path d="M18 21a8 8 0 0 0-16 0"/><circle cx="10" cy="8" r="5"/><path d="M22 20c0-3.37-2-6.5-4-8a5 5 0 0 0-.45-8.3"/>'),
  otra: svg('<path d="M6 9V3h12v6"/><path d="M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2"/><rect x="6" y="14" width="12" height="8" rx="1"/>'),
  globe: svg('<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>'),
  lock: svg('<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>'),
  app: svg('<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18"/>'),
  site: svg('<circle cx="12" cy="12" r="10"/><path d="M2 12h20"/><path d="M12 2a15 15 0 0 1 0 20"/>'),
  ip: svg('<rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><path d="M6 6h.01M6 18h.01"/>'),
};
// Reglas que aplican a una impresora: las de la regla global (si está activa) y las propias (si están activas).
const gCount = () => (globalRules.enabled ? nRules(globalRules) : 0);
const ruleItems = (r, origin) => KINDS.flatMap(([k, , ]) => r[k].map((v) => ({ k, v, origin, paused: isPaused(r, v) })));
const KIC = { apps: "app", sites: "site", ips: "ip" };
function rulesDetail(p) {
  const items = [...(globalRules.enabled ? ruleItems(globalRules, "Global") : []), ...(p.rules.enabled ? ruleItems(p.rules, "Propia") : [])];
  if (!items.length) return '<div class="sub">Sin reglas que apliquen.</div>';
  return '<div class="chips">' + items.map((it) => `<span class="chip${it.paused ? " paused" : ""}" title="${it.origin}${it.paused ? " · suspendida" : ""}">${IC[KIC[it.k]]}${esc(it.v)}<small>${it.origin}${it.paused ? " · suspendida" : ""}</small></span>`).join("") + "</div>";
}

const openDet = new Set(); // impresoras con el detalle de reglas abierto (sobrevive a repintar)

function renderPrinters() {
  const el = $("#v-printers");
  el.innerHTML = '<div class="bar"><h2 class="grow" style="margin:0">Impresoras</h2>' + (window.__MODO_SERVIDOR__ ? '<button class="btn" id="blockhost">Bloquear equipo</button>' : "") + '<button class="btn" id="refresh">Actualizar</button></div>' + (printers.length ? "" : '<p class="sub">No se encontraron impresoras en el sistema.</p>') +
    '<div class="pgrid">' + printers.map((p, i) => `<div class="card pcard"><div class="phead"><span class="pico">${IC[p.info.kind] || IC.otra}</span><div class="grow"><b>${esc(p.name)}</b><div class="sub">${esc(KIND[p.info.kind] || "—")} · ${esc(p.info.uri)}</div></div><button class="sw ${p.rules.enabled ? "on" : ""}" role="switch" aria-checked="${p.rules.enabled}" aria-label="Controlar ${esc(p.name)}" data-i="${i}"></button></div>
      <div class="pbadges">${CONN[p.info.status] || ""}${badge(p)}<span class="badge ${globalRules.enabled ? "ok" : "off"}" title="Reglas de la regla global">${IC.globe}Global · ${gCount()}</span><span class="badge ${p.rules.enabled ? "ok" : "off"}" title="Reglas propias de esta impresora">${IC.lock}Propias · ${nRules(p.rules)}</span></div>
      ${p.info.hold_supported === false ? '<div class="sub">⚠ Esta impresora no admite retención por cola</div>' : ""}
      <button class="pdet-btn" data-det="${i}" aria-expanded="${openDet.has(p.name)}" aria-controls="det-${i}">${svg('<path d="m9 18 6-6-6-6"/>').replace('class="ic"', 'class="ic chev"')}Reglas que aplican (${gCount() + (p.rules.enabled ? nRules(p.rules) : 0)})</button><div class="pdet-panel" id="det-${i}" ${openDet.has(p.name) ? "" : "hidden"}>${rulesDetail(p)}</div>${window.__MODO_SERVIDOR__ ? `<div class="bar" style="margin:8px 0 0"><button class="btn" data-pause="${i}">${p.quick_paused ? "Reanudar" : "Pausar impresora"}</button></div>` : ""}</div>`).join("") + "</div>";
  $("#refresh").onclick = async () => { await refreshPrinters(); toast("Impresoras actualizadas"); };
  // Bloqueo rápido (solo panel web del servidor).
  const bh = $("#blockhost");
  if (bh) bh.onclick = async () => {
    const host = (prompt("IP o nombre del equipo a bloquear (efecto inmediato):") || "").trim();
    if (!host) return;
    try { await invoke("block_host", { host }); globalRules = await invoke("get_global_rules"); toast("Equipo bloqueado: " + host); renderRules(); }
    catch (e) { toast(String(e)); }
  };
  el.querySelectorAll("[data-pause]").forEach((b) => b.onclick = async () => {
    const p = printers[b.dataset.pause];
    try { await invoke("pause_printer", { printer: p.name, paused: !p.quick_paused }); await refreshPrinters(); }
    catch (e) { toast(String(e)); }
  });
  el.querySelectorAll("[data-det]").forEach((b) => b.onclick = () => {
    const n = printers[b.dataset.det].name, on = !openDet.has(n);
    on ? openDet.add(n) : openDet.delete(n);
    b.setAttribute("aria-expanded", on);
    $("#det-" + b.dataset.det).hidden = !on;
  });
  el.querySelectorAll(".sw").forEach((b) => b.onclick = async () => {
    const p = printers[b.dataset.i]; p.rules.enabled = !p.rules.enabled;
    if (!p.rules.mode) p.rules.mode = "block";
    try { await save(p); } catch { p.rules.enabled = !p.rules.enabled; }
    renderPrinters(); renderRules(); status();
  });
}

function renderRules() {
  const el = $("#v-rules");
  if (!printers.length) { el.innerHTML = "<h2>Reglas</h2><p class='sub'>Sin impresoras.</p>"; return; }
  if (selected !== GLOBAL && !printers.find((p) => p.name === selected)) selected = printers[0].name;
  const isG = selected === GLOBAL, p = isG ? { name: GLOBAL, rules: globalRules } : printers.find((x) => x.name === selected), r = p.rules, mode = r.mode || "block";
  el.innerHTML = `<h2>Reglas</h2>
  <div class="bar"><label>Impresora <select id="sel"><option value="${GLOBAL}" ${isG ? "selected" : ""}>★ Todas las impresoras (global)</option>${printers.map((x) => `<option value="${esc(x.name)}" ${x.name === selected ? "selected" : ""}>${esc(x.name)}</option>`).join("")}</select></label></div>
  ${!isG && globalRules.enabled ? '<div class="sub">Además aplica la regla global.</div>' : ""}
  ${isG ? `<div class="card row"><div class="grow"><b>Controlar todas las impresoras</b><div class="sub">Se suma a las reglas de cada impresora: si cualquiera bloquea, se bloquea.</div></div><button class="sw ${r.enabled ? "on" : ""}" id="gsw" role="switch" aria-checked="${r.enabled}" aria-label="Controlar todas las impresoras"></button></div>` : ""}
  <div class="card row"><b class="grow">Modo</b><div class="seg"><button data-m="block" class="${mode === "block" ? "on" : ""}">Bloquear coincidencias</button><button data-m="allow" class="${mode === "allow" ? "on" : ""}">Permitir solo coincidencias</button></div></div>
  <div class="grid">${KINDS.map(([k, t, ph]) => `<div class="card"><h3>${t}</h3><div>${r[k].map((v, j) => `<span class="chip${isPaused(r, v) ? " paused" : ""}">${k === "apps" ? `<img class="ico" data-app="${esc(v)}" alt="" hidden>` : ""}${esc(v)}${isPaused(r, v) ? `<small>${esc(pausedLabel(r, v))}</small>` : ""}<button data-pk="${k}" data-pj="${j}" aria-label="${isPaused(r, v) ? "Reanudar" : "Suspender"} ${esc(v)}" title="${isPaused(r, v) ? "Reanudar" : "Suspender"}">${isPaused(r, v) ? "▶" : "⏸"}</button><button data-k="${k}" data-j="${j}" aria-label="Quitar ${esc(v)}">✕</button></span>`).join("") || '<span class="sub">Sin entradas</span>'}</div>
    <form class="add" data-k="${k}"><input placeholder="${ph}" aria-label="${t}" ${k === "apps" ? 'autocomplete="off"' : ""}><button class="btn" type="submit">Agregar</button></form>${k === "apps" ? '<div class="suggest" id="suggest" hidden></div>' : ""}
    ${k === "sites" ? '<div class="sub" style="margin-top:8px">Opcional: sin la extensión de navegador solo coincide si el título de la página incluye el dominio.</div>' : ""}</div>`).join("")}</div>
  <datalist id="apps"></datalist>`;
  setupApps(p);
  const gsw = $("#gsw");
  if (gsw) gsw.onclick = async () => {
    r.enabled = !r.enabled; if (!r.mode) r.mode = "block";
    try { await save(p); } catch { r.enabled = !r.enabled; }
    renderRules(); renderPrinters(); status();
  };
  $("#sel").onchange = (e) => { selected = e.target.value; renderRules(); };
  el.querySelectorAll(".seg button").forEach((b) => b.onclick = async () => { r.mode = b.dataset.m; await save(p); renderRules(); renderPrinters(); });
  el.querySelectorAll(".chip button[data-k]").forEach((b) => b.onclick = async () => { const [v] = r[b.dataset.k].splice(b.dataset.j, 1); if (r.paused) delete r.paused[v]; await save(p); renderRules(); renderPrinters(); });
  el.querySelectorAll(".chip button[data-pk]").forEach((b) => b.onclick = async () => {
    const v = r[b.dataset.pk][b.dataset.pj];
    if (isPaused(r, v)) delete r.paused[v];
    else {
      const until = await pauseDialog(v);
      if (until === null) return;
      r.paused = Object.fromEntries(Object.entries(r.paused || {}).filter(([k]) => isPaused(r, k))); // limpia vencidas
      r.paused[v] = until;
    }
    if (!r.mode) r.mode = "block";
    await save(p); renderRules(); renderPrinters(); status();
  });
  el.querySelectorAll("form.add").forEach((f) => f.onsubmit = async (e) => {
    e.preventDefault(); const v = f.querySelector("input").value.trim(); if (!v) return;
    r[f.dataset.k].push(v); if (!r.mode) r.mode = "block"; await save(p); renderRules(); renderPrinters();
  });
}

const iconCache = {};
async function appIcon(name) {
  if (!(name in iconCache)) iconCache[name] = invoke("app_icon", { name }).catch(() => null);
  return iconCache[name];
}
let appList = null;
async function setupApps(p) {
  const el = $("#v-rules");
  el.querySelectorAll("img.ico").forEach(async (i) => { const u = await appIcon(i.dataset.app); if (u) { i.src = u; i.hidden = false; } });
  const input = el.querySelector('form.add[data-k="apps"] input'), box = $("#suggest");
  if (!input) return;
  appList ??= await invoke("list_apps").catch(() => []);
  const show = () => {
    const q = input.value.trim().toLowerCase();
    const items = appList.filter((a) => !q || a.toLowerCase().includes(q)).slice(0, 8);
    box.hidden = !items.length;
    box.innerHTML = items.map((a) => `<button type="button" class="opt" data-n="${esc(a)}"><img class="ico" data-app="${esc(a)}" alt="" hidden><span>${esc(a)}</span></button>`).join("");
    box.querySelectorAll("img.ico").forEach(async (i) => { const u = await appIcon(i.dataset.app); if (u) { i.src = u; i.hidden = false; } });
    box.querySelectorAll(".opt").forEach((b) => b.onmousedown = (e) => { e.preventDefault(); input.value = b.dataset.n; box.hidden = true; input.form.requestSubmit(); });
  };
  input.onfocus = input.oninput = show;
  input.onblur = () => { box.hidden = true; };
}
const fmt = (t) => new Date(t * 1000).toLocaleString("es-CL");
// Registro: la base filtra y pagina (query_events); aquí solo se pintan la página y los controles.
const logFilter = () => {
  const day = (v, end) => (v ? Math.floor(new Date(v + (end ? "T23:59:59" : "T00:00:00")).getTime() / 1000) : null);
  return { decision: filter.state === "all" ? "" : filter.state, printer: filter.printer, text: filter.q, from: day(filter.from), to: day(filter.to, true), ...(filter.user ? { user: filter.user } : {}), ...(filter.host ? { host: filter.host } : {}) };
};
let logTimer = 0;
async function refreshLog() {
  try { log = await invoke("query_events", { filter: logFilter(), page: filter.page, size: filter.size }); } catch (e) { toast(String(e)); return; }
  if (logTab === "who") return refreshWho();
  const pages = Math.max(1, Math.ceil(log.total / filter.size));
  if (filter.page > pages) { filter.page = pages; return refreshLog(); }
  renderLog();
}
// CSV en el cliente (inventario, descubrimiento y totales): mismas reglas que el CSV de eventos, con fórmulas neutralizadas.
const csvCell = (v) => { v = String(v ?? ""); return '"' + (/^[=+\-@\t\r]/.test(v) ? "'" : "") + v.replace(/"/g, '""') + '"'; };
const toCsv = (head, rows) => [head, ...rows].map((r) => r.map(csvCell).join(",")).join("\n") + "\n";
const logTabs = () => `<div class="seg tabs" id="ltabs">${[["events", "Eventos"], ["who", "Quién imprimió qué"]].map(([k, t]) => `<button data-t="${k}" class="${logTab === k ? "on" : ""}">${t}</button>`).join("")}</div>`;
const bindTabs = () => $("#ltabs").querySelectorAll("button").forEach((b) => b.onclick = () => { logTab = b.dataset.t; refreshLog(); });
const filterChips = () => [["user", "Usuario"], ["host", "Equipo"]].filter(([k]) => filter[k]).map(([k, t]) => `<span class="chip">${t}: ${esc(filter[k])}<button data-x="${k}" aria-label="Quitar filtro ${t}">✕</button></span>`).join("");
const bindChips = (reset) => document.querySelectorAll("#v-log .chip button[data-x]").forEach((b) => b.onclick = () => { filter[b.dataset.x] = ""; reset(); });
const pagesText = (r) => r.pages == null ? "—" : r.pages_jobs < r.jobs ? `${r.pages} (parcial)` : String(r.pages);
async function refreshWho() {
  try { who.rows = await invoke("summarize_events", { filter: logFilter(), by: who.by }); } catch (e) { toast(String(e)); return; }
  renderWho();
}
const GROUPS = [["user", "Usuario"], ["host", "Equipo"], ["printer", "Impresora"], ["decision", "Decisión"]];
const DEC = { allow: "Permitido", block: "Bloqueado" };
function renderWho() {
  const el = $("#v-log"), rows = who.rows || [], names = log.printers || [], gname = GROUPS.find(([k]) => k === who.by)[1];
  const tot = rows.reduce((a, r) => ({ jobs: a.jobs + r.jobs, allowed: a.allowed + r.allowed, blocked: a.blocked + r.blocked, pages: r.pages == null ? a.pages : (a.pages ?? 0) + r.pages, pj: a.pj + r.pages_jobs }), { jobs: 0, allowed: 0, blocked: 0, pages: null, pj: 0 });
  const label = (r) => r.key === "" ? "(desconocido)" : who.by === "decision" ? DEC[r.key] || r.key : r.key;
  el.innerHTML = `<h2>Registro</h2>${logTabs()}<div class="bar">
    <div class="seg" id="wg" aria-label="Agrupar por">${GROUPS.map(([k, t]) => `<button data-g="${k}" class="${who.by === k ? "on" : ""}">${t}</button>`).join("")}</div>
    <div class="seg">${[["all", "Todos"], ["ok", "Permitidos"], ["bad", "Bloqueados"]].map(([k, t]) => `<button data-s="${k}" class="${filter.state === k ? "on" : ""}">${t}</button>`).join("")}</div>
    <select id="fp" aria-label="Impresora"><option value="">Todas las impresoras</option>${names.map((n) => `<option ${n === filter.printer ? "selected" : ""}>${esc(n)}</option>`).join("")}</select>
    <input id="ff" type="date" aria-label="Desde" value="${esc(filter.from)}"><input id="ft" type="date" aria-label="Hasta" value="${esc(filter.to)}">
    <input id="fq" type="search" placeholder="Buscar…" aria-label="Buscar" value="${esc(filter.q)}">${filterChips()}
    <button class="btn" id="wexp">Exportar CSV</button></div>
    <div class="tw"><table><tr><th>${gname}</th><th class="num">Trabajos</th><th class="num">Permitidos</th><th class="num">Bloqueados</th><th class="num">Páginas</th><th></th></tr>
    ${rows.map((r, i) => `<tr><td>${esc(label(r))}</td><td class="num">${r.jobs}</td><td class="num">${r.allowed}</td><td class="num">${r.blocked}</td><td class="num">${pagesText(r)}</td><td>${["user", "host"].includes(who.by) && r.key ? `<button class="btn" data-i="${i}">Ver eventos</button>` : ""}</td></tr>`).join("") || '<tr><td colspan="6" class="sub">Sin trabajos para estos filtros</td></tr>'}
    ${rows.length ? `<tr><th>Total</th><th class="num">${tot.jobs}</th><th class="num">${tot.allowed}</th><th class="num">${tot.blocked}</th><th class="num">${tot.pages == null ? "—" : tot.pj < tot.jobs ? tot.pages + " (parcial)" : tot.pages}</th><th></th></tr>` : ""}</table></div>
    <div class="sub">Las páginas solo se suman cuando el sistema las informa (Windows: páginas del spooler; CUPS: job-impressions); "(parcial)" indica que algunos trabajos no las informaron. Los avisos del sistema no cuentan como trabajos.</div>`;
  bindTabs(); bindChips(() => { refreshLog(); });
  const redo = () => refreshLog();
  $("#wg").querySelectorAll("button").forEach((b) => b.onclick = () => { who.by = b.dataset.g; refreshLog(); });
  el.querySelectorAll(".seg button[data-s]").forEach((b) => b.onclick = () => { filter.state = b.dataset.s; redo(); });
  $("#fp").onchange = (e) => { filter.printer = e.target.value; redo(); };
  $("#ff").onchange = (e) => { filter.from = e.target.value; redo(); };
  $("#ft").onchange = (e) => { filter.to = e.target.value; redo(); };
  $("#fq").oninput = (e) => { filter.q = e.target.value; clearTimeout(logTimer); logTimer = setTimeout(async () => { await refreshLog(); const i = $("#fq"); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }, 250); };
  el.querySelectorAll("button[data-i]").forEach((b) => b.onclick = () => { filter[who.by] = who.rows[b.dataset.i].key; logTab = "events"; filter.page = 1; refreshLog(); });
  $("#wexp").onclick = async () => {
    const csv = toCsv([gname, "trabajos", "permitidos", "bloqueados", "paginas", "trabajos_con_paginas"], rows.map((r) => [label(r), r.jobs, r.allowed, r.blocked, r.pages ?? "", r.pages_jobs]));
    const p = await saveAs("quien-imprimio-que.csv", csv, "csv").catch((e) => { toast(String(e)); }); if (p) toast("Guardado: " + p);
  };
}
// Estado de una fila del registro: los avisos del sistema no son permitidos ni bloqueados, y un bloqueado que ya había salido se dice.
function stateBadge(a) {
  if (a.decision === "notice") return '<span class="badge warn">ℹ Aviso</span>';
  if (a.allowed) return '<span class="badge ok">✓ Permitido</span>';
  if (a.status === "impreso antes de cancelar") return '<span class="badge bad" title="El trabajo ya se estaba imprimiendo cuando se canceló">✕ Bloqueado, pero salió impreso</span>';
  return '<span class="badge bad">✕ Bloqueado</span>';
}
function renderLog() {
  if (logTab === "who") return renderWho();
  const el = $("#v-log"), rows = log.rows || [], total = log.total || 0, names = log.printers || [];
  const server = !!window.__MODO_SERVIDOR__;
  const pages = Math.max(1, Math.ceil(total / filter.size));
  const reset = () => { filter.page = 1; refreshLog(); };
  el.innerHTML = `<h2>Registro</h2>${logTabs()}<div class="bar">
    <div class="seg">${[["all", "Todos"], ["ok", "Permitidos"], ["bad", "Bloqueados"]].map(([k, t]) => `<button data-s="${k}" class="${filter.state === k ? "on" : ""}">${t}</button>`).join("")}</div>
    <select id="fp" aria-label="Impresora"><option value="">Todas las impresoras</option>${names.map((n) => `<option ${n === filter.printer ? "selected" : ""}>${esc(n)}</option>`).join("")}</select>
    <input id="ff" type="date" aria-label="Desde" value="${esc(filter.from)}"><input id="ft" type="date" aria-label="Hasta" value="${esc(filter.to)}">
    <input id="fq" type="search" placeholder="Buscar…" aria-label="Buscar" value="${esc(filter.q)}">${filterChips()}
    <button class="btn" id="exp">Exportar CSV</button><button class="btn danger" id="clr">Borrar</button></div>
    <div class="tw"><table><tr><th>Fecha</th><th>Estado</th><th>Documento</th><th>Impresora</th><th>Usuario</th>${server ? "<th>Equipo</th>" : ""}<th>Origen</th><th>Salida</th><th>Págs.</th><th>Motivo</th></tr>
    ${rows.map((a) => `<tr><td class="nw">${esc(fmt(a.time))}</td><td>${stateBadge(a)}</td><td>${esc(a.document)}</td><td>${esc(a.printer)}</td><td>${esc(a.user)}</td>${server ? `<td>${esc(a.device || a.host)}</td>` : ""}<td>${esc(a.origin || "local")}</td><td style="min-width:200px">${a.port || a.conn_type ? `<span class="mono">${esc(a.port || "—")}</span><div class="sub">${esc(CONN_T[a.conn_type] || a.conn_type || "")}</div>` : "—"}</td><td>${esc(a.pages ?? "—")}</td><td>${esc(a.reason)}</td></tr>`).join("") || `<tr><td colspan="${server ? 10 : 9}" class="sub">Sin registros</td></tr>`}</table></div>
    <div class="bar pager"><span class="sub">${total} registros</span><span class="grow"></span>
      <label class="sub">Por página <select id="fs">${[10, 25, 50, 100].map((n) => `<option ${n === filter.size ? "selected" : ""}>${n}</option>`).join("")}</select></label>
      <button class="btn" id="pp" ${filter.page <= 1 ? "disabled" : ""}>‹ Anterior</button><span>Página ${filter.page} de ${pages}</span>
      <button class="btn" id="pn" ${filter.page >= pages ? "disabled" : ""}>Siguiente ›</button></div>`;
  bindTabs(); bindChips(reset);
  el.querySelectorAll(".seg button[data-s]").forEach((b) => b.onclick = () => { filter.state = b.dataset.s; reset(); });
  $("#fp").onchange = (e) => { filter.printer = e.target.value; reset(); };
  $("#ff").onchange = (e) => { filter.from = e.target.value; reset(); };
  $("#ft").onchange = (e) => { filter.to = e.target.value; reset(); };
  $("#fq").oninput = (e) => {
    filter.q = e.target.value; filter.page = 1;
    clearTimeout(logTimer);
    logTimer = setTimeout(async () => { await refreshLog(); const i = $("#fq"); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }, 250);
  };
  $("#fs").onchange = (e) => { filter.size = +e.target.value; reset(); };
  $("#pp").onclick = () => { filter.page--; refreshLog(); };
  $("#pn").onclick = () => { filter.page++; refreshLog(); };
  $("#exp").onclick = async () => { const p = await saveAs("registro-impresion.csv", await call("export_log_csv", { filter: logFilter() }), "csv").catch((e) => { toast(String(e)); }); if (p) toast("Guardado: " + p); };
  $("#clr").onclick = async () => { if (await confirmNative("¿Borrar todo el registro?", "Borrar")) { await call("clear_log"); reset(); } };
}


// --- Inventario, descubrimiento de red y detección de impresión directa ---
const CONN_T = { usb: "USB", socket: "Red (socket 9100)", ipp: "Red (IPP)", lpd: "Red (LPD)", wsd: "Red (WSD)", red: "Red", compartida: "Compartida (SMB)", archivo: "Archivo", local: "Local (serie/paralelo)", desconocido: "Desconocido" };
const MANAGED = { controlada: '<span class="badge ok">✓ Cola controlada</span>', cola_sin_control: '<span class="badge warn">⚠ Cola sin control</span>', sin_cola: '<span class="badge bad">✕ Sin control (impresión directa)</span>' };
const yn = (v) => v == null ? "—" : v ? "Sí" : "No";
async function refreshInventory() {
  const el = $("#v-inventory");
  el.innerHTML = "<h2>Inventario</h2><p class='sub'>Leyendo impresoras y puertos…</p>";
  try { inv = await invoke("get_inventory"); net = await invoke("get_network_settings"); disc = await invoke("discover_status").catch(() => disc); } catch (e) { toast(String(e)); return; }
  renderInventory();
  if (disc.running) pollDiscovery();
}
function renderInventory() {
  const el = $("#v-inventory"), server = !!window.__MODO_SERVIDOR__;
  el.innerHTML = `<div class="bar"><h2 class="grow" style="margin:0">Inventario</h2><button class="btn" id="iv-ref">Actualizar</button><button class="btn" id="iv-csv">Exportar CSV</button></div>
  <div class="tw"><table><tr><th>Nombre</th><th>Modelo / driver</th><th>Puerto / URI</th><th>Conexión</th><th>IP / host</th><th>Compartida</th><th>Estado</th><th>Retención</th><th>Control</th></tr>
  ${inv.map((r) => `<tr><td><b>${esc(r.name)}</b></td><td>${esc(r.driver || "—")}</td><td class="mono">${esc(r.uri || "—")}</td><td>${esc(CONN_T[r.conn_type] || r.conn_type || "—")}</td><td class="mono">${esc(r.host || "—")}</td><td>${yn(r.shared)}</td><td>${CONN[r.status] || esc(r.status || "—")}</td><td>${r.hold_supported ? "Sí" : '<span class="badge warn">No</span>'}</td><td>${r.controlled ? '<span class="badge ok">✓ Activo</span>' : '<span class="badge off">— Sin control</span>'}</td></tr>`).join("") || '<tr><td colspan="9" class="sub">No se encontraron impresoras en el sistema.</td></tr>'}</table></div>
  <div class="sub" style="margin:-4px 0 14px">Solo se muestra lo que el sistema informa; "—" significa que no lo entrega. El inventario también se incluye en la copia de seguridad (categoría "Inventario", sin credenciales).</div>
  <div class="card" id="iv-net"><h3>Impresoras en la red</h3>
    <div class="sub" style="margin-bottom:8px">Opcional. Solo prueba las subredes que escribas aquí (por defecto ninguna): una conexión TCP a los puertos 9100, 515 y 631 de cada dirección, y una consulta IPP si el 631 responde. Máximo ${net.limits?.min_prefix ? "un /" + net.limits.min_prefix : "un /24"} por rango y ${net.limits?.max_hosts || 512} direcciones por búsqueda, 8 a la vez. No usa SNMP. Cada búsqueda queda anotada en el Registro con quién la inició y el rango.</div>
    <div class="bar"><input id="nt-cidr" placeholder="192.168.1.0/24, 192.168.2.0/24" aria-label="Subredes a buscar" value="${esc((net.cidrs || []).join(", "))}" style="flex:1;min-width:240px">
      <button class="btn" id="nt-save">Guardar rangos</button></div>
    <label class="chk-line"><input type="checkbox" id="nt-pub"> Confirmo que el rango no es una red privada y quiero probarlo (solo si estás autorizado)</label>
    <div class="bar" style="margin-top:10px"><button class="btn pri" id="nt-go" ${disc.running ? "disabled" : ""}>Buscar impresoras en la red</button><button class="btn" id="nt-stop" ${disc.running ? "" : "hidden"}>Cancelar</button>
      <div class="prog" id="nt-prog" ${disc.running ? "" : "hidden"}><i style="width:${disc.total ? Math.round(100 * disc.done / disc.total) : 0}%"></i></div><span class="sub" id="nt-msg"></span></div>
    <div id="nt-res">${discoveryHtml()}</div></div>
  ${net.direct_available ? directHtml() : ""}`;
  $("#iv-ref").onclick = refreshInventory;
  $("#iv-csv").onclick = async () => {
    const csv = toCsv(["nombre", "modelo_driver", "puerto_uri", "tipo_conexion", "ip_host", "compartida", "estado", "retencion_soportada", "control_activo"], inv.map((r) => [r.name, r.driver, r.uri, r.conn_type, r.host, r.shared == null ? "" : r.shared ? "si" : "no", r.status, r.hold_supported ? "si" : "no", r.controlled ? "si" : "no"]));
    const p = await saveAs("inventario-impresoras.csv", csv, "csv").catch((e) => { toast(String(e)); }); if (p) toast("Guardado: " + p);
  };
  const cidrs = () => $("#nt-cidr").value.split(/[,\s]+/).filter(Boolean);
  $("#nt-save").onclick = async () => {
    try { await invoke("set_network_settings", { cidrs: cidrs(), direct: net.direct }); net.cidrs = cidrs(); toast("Rangos guardados"); } catch (e) { toast(String(e)); }
  };
  $("#nt-go").onclick = async () => {
    try { disc = await invoke("discover_network", { cidrs: cidrs(), confirm_public: $("#nt-pub").checked }); } catch (e) { toast(String(e)); return; }
    renderInventory(); pollDiscovery();
  };
  $("#nt-stop").onclick = () => invoke("discover_cancel").catch((e) => toast(String(e)));
  const dcsv = $("#nt-csv");
  if (dcsv) dcsv.onclick = async () => {
    const csv = toCsv(["ip", "puertos", "modelo", "nombre_ipp", "cola", "gestion", "explicacion"], disc.devices.map((d) => [d.ip, (d.ports || []).join(" "), d.model, d.name, d.queue, d.managed, d.explanation]));
    const p = await saveAs("impresoras-en-la-red.csv", csv, "csv").catch((e) => { toast(String(e)); }); if (p) toast("Guardado: " + p);
  };
  if (net.direct_available) bindDirect();
}
function discoveryHtml() {
  const ds = disc.devices || [];
  if (!disc.started && !disc.running) return "";
  if (disc.running) return `<div class="sub" style="margin-top:8px">Buscando… ${disc.done || 0} de ${disc.total || 0} pruebas (${disc.hosts || 0} direcciones).</div>`;
  const risk = ds.filter((d) => d.managed === "sin_cola").length;
  return `<div class="sub" style="margin:10px 0 6px">${ds.length ? `${ds.length} dispositivo${ds.length === 1 ? "" : "s"} con puertos de impresión abiertos${risk ? `, <b>${risk} sin cola controlada</b> (riesgo de impresión directa)` : ""}.` : "Ningún dispositivo respondió en los rangos indicados."} Iniciado por ${esc(disc.by)}. ${esc(disc.error || "")}</div>
  ${ds.length ? `<div class="tw"><table><tr><th>IP</th><th>Puertos</th><th>Identificación (IPP)</th><th>Gestión</th><th>Explicación</th></tr>
  ${ds.map((d) => `<tr><td class="mono">${esc(d.ip)}</td><td>${esc((d.ports || []).join(", "))}</td><td>${d.ipp ? esc([d.model, d.name].filter(Boolean).join(" · ") || "Responde IPP") : '<span class="sub">No respondió IPP</span>'}</td><td>${MANAGED[d.managed] || ""}${d.queue ? `<div class="sub">${esc(d.queue)}</div>` : ""}</td><td class="sub" style="max-width:340px">${esc(d.explanation)}</td></tr>`).join("")}</table></div><button class="btn" id="nt-csv">Exportar CSV</button>` : ""}`;
}
function pollDiscovery() {
  clearTimeout(discTimer);
  discTimer = setTimeout(async () => {
    try { disc = await invoke("discover_status"); } catch { return; }
    if ($("#v-inventory").hidden) { if (disc.running) pollDiscovery(); return; }
    if (disc.running) { const b = $("#nt-prog i"); if (b) b.style.width = (disc.total ? Math.round(100 * disc.done / disc.total) : 0) + "%"; const r = $("#nt-res"); if (r) r.innerHTML = discoveryHtml(); pollDiscovery(); }
    else renderInventory();
  }, 600);
}
function directHtml() {
  const d = net.direct || {}, st = net.direct_status || [];
  return `<div class="card" id="iv-direct"><h3>Detección de impresión directa (mejor esfuerzo)</h3>
    <div class="sub" style="margin-bottom:8px">Apagada por defecto. Cada cierto tiempo consulta por IPP a las impresoras de red que tienen cola aquí y compara el contador de la propia impresora con lo que el servidor liberó; si sube sin trabajos del servidor, anota "Impresión directa sospechosa" en el Registro. No identifica al usuario. Depende del modelo: si la impresora no expone contadores por IPP figura como "no verificable". No se ha probado con impresoras reales.</div>
    <div class="field">Activar detección ${sw("dr-on", d.enabled, "Activar detección de impresión directa")}</div>
    <div class="field">Intervalo (segundos, 30 a 86400) <input id="dr-int" type="number" min="30" max="86400" value="${d.interval_secs || 300}" style="width:90px"></div>
    <div class="field"><span class="sub">${d.enabled ? "Activa" : "Apagada"}</span><button class="btn pri" id="dr-save">Guardar</button></div>
    ${st.length ? `<div class="tw" style="margin-top:8px"><table><tr><th>Impresora</th><th>IP</th><th>Fuente</th><th>Estado</th><th>Alertas</th></tr>${st.map((x) => `<tr><td>${esc(x.printer)}</td><td class="mono">${esc(x.host)}</td><td>${esc(x.source || "—")}</td><td>${x.error ? `<span class="badge warn">${esc(x.error)}</span>` : '<span class="badge ok">✓ Verificable</span>'}</td><td>${esc(x.alerts)}</td></tr>`).join("")}</table></div>` : ""}</div>`;
}
function bindDirect() {
  const save = async (enabled) => {
    const secs = Math.max(30, Math.min(86400, +$("#dr-int").value || 300));
    try { await invoke("set_network_settings", { cidrs: net.cidrs, direct: { enabled, interval_secs: secs } }); net.direct = { enabled, interval_secs: secs }; toast("Guardado"); renderInventory(); } catch (e) { toast(String(e)); }
  };
  $("#dr-on").onclick = () => save(!net.direct.enabled);
  $("#dr-save").onclick = () => save(net.direct.enabled);
}

const sw = (id, on, label) => `<button class="sw ${on ? "on" : ""}" id="${id}" role="switch" aria-checked="${on}" aria-label="${label}"></button>`;
const segm = (id, opts, cur) => `<div class="seg" id="${id}">${opts.map(([v, t]) => `<button data-v="${v}" class="${cur === v ? "on" : ""}">${t}</button>`).join("")}</div>`;

// Configuración de inicio: diálogo paso a paso con el estado real de cada ajuste. Se abre solo la primera vez.
async function setupDialog() {
  if (document.querySelector("dialog.setup")) return;
  const d = document.createElement("dialog");
  d.className = "setup wide";
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  let n = 0;
  const state = async () => {
    const [auto, hasPw, ext, g] = await Promise.all([
      invoke("get_autostart").catch(() => false), invoke("has_password").catch(() => false),
      invoke("extension_status").catch(() => null), invoke("guard_status").catch(() => [0, 0, false]),
    ]);
    const [gOn, gTot, gOk] = g;
    return [
      { t: "Permiso de administrador", ok: !gOk || (gTot > 0 && gOn >= gTot),
        d: gOk ? (IS_WIN ? "Activa la protección previa a la impresión: cada trabajo espera el veredicto de la app dentro del spooler antes de salir hacia la impresora. Pide el permiso de administrador (UAC) una vez."
          : "Activa la protección previa a la impresión: la app ve cada trabajo antes de que salga papel (Word y otras apps lo envían sin retención). Pide la contraseña de administrador una vez.") : "No aplica en este sistema.",
        b: gOk && gOn < gTot ? ["Activar protección", async () => { await call("guard_install"); toast("Protección activada"); }] : null },
      { t: "Notificaciones", ok: null,
        d: "Para ver los bloqueos al instante: Ajustes del Sistema → Notificaciones → SpoolGuard → Permitir. La app no puede comprobarlo." },
      { t: "Iniciar con el sistema", ok: auto, d: "El control queda activo desde que se enciende el equipo.",
        b: auto ? null : ["Activar", async () => { await call("set_autostart", { enabled: true }); }] },
      { t: "Reglas globales", ok: globalRules.enabled && gCount() > 0,
        d: "Define qué aplicaciones, sitios o equipos se bloquean en todas las impresoras. Puedes hacerlo ahora o después.", b: ["Abrir Reglas", () => { d.close(); go("rules"); }] },
      { t: "Extensión de navegador (opcional)", ok: !!ext?.last_seen && Date.now() / 1000 - ext.last_seen < 180, opt: true,
        d: "Controla la impresión desde sitios web. Chrome pide cargarla una vez en chrome://extensions.", b: ["Instalar…", () => installExtDialog()] },
      { t: "Contraseña de la app (opcional)", ok: hasPw, opt: true,
        d: "Evita que otras personas cambien las reglas.", b: hasPw ? null : ["Definir en Ajustes", () => { d.close(); go("settings"); }] },
      { t: "Probar", ok: null,
        d: "Imprime desde una app bloqueada: debe verse «bloqueado» en Registro y no salir papel. Luego imprime desde una permitida." },
    ];
  };
  const draw = async () => {
    const st = await state(), s = st[n], last = n === st.length - 1;
    const tag = s.ok === null ? ["off", "Manual"] : s.ok ? ["ok", "✓ Listo"] : s.opt ? ["off", "Opcional"] : ["warn", "Pendiente"];
    d.innerHTML = `<form method="dialog" class="dlg"><b>Configuración de inicio</b>
      <div class="sub">Paso ${n + 1} de ${st.length}</div>
      <div class="dots">${st.map((_, i) => `<i class="${i === n ? "on" : i < n ? "past" : ""}"></i>`).join("")}</div>
      <h3 style="margin:4px 0">${esc(s.t)} <span class="badge ${tag[0]}">${tag[1]}</span></h3>
      <div class="sub">${esc(s.d)}</div>
      ${s.b ? `<div><button class="btn" type="button" id="su-act">${esc(s.b[0])}</button></div>` : ""}
      <div class="row" style="justify-content:space-between"><button class="btn" value="skip">Omitir configuración</button>
        <span class="row">${n ? '<button class="btn" type="button" id="su-back">Atrás</button>' : ""}<button class="btn pri" type="button" id="su-next">${last ? "Terminar" : "Siguiente"}</button></span></div></form>`;
    if ($("#su-act")) $("#su-act").onclick = async () => { await s.b[1](); if (d.isConnected) draw(); };
    if ($("#su-back")) $("#su-back").onclick = () => { n--; draw(); };
    $("#su-next").onclick = () => { if (last) d.close(); else { n++; draw(); } };
  };
  d.addEventListener("close", () => localStorage.setItem("sg-setup-done", "1"));
  await draw();
  d.showModal();
}

// Versión y actualización (solo bajo demanda; en el panel web del servidor no existe).
let appVersion = "", upd = { st: "idle" };
const fmtMB = (n) => (n / 1048576).toFixed(1) + " MB";
function paintUpd() {
  const el = $("#u-box"), b = $("#u-chk");
  if (!el) return;
  const i = upd.info, busy = upd.st === "checking" || upd.st === "downloading";
  if (b) { b.disabled = busy; b.textContent = upd.st === "checking" ? "Buscando…" : "Buscar actualización"; }
  const note = i?.notas ? `<div class="sub upd-notes">${esc(i.notas)}</div>` : "";
  el.innerHTML = {
    idle: "", checking: "",
    ok: `<span class="badge ok">✓ Al día · v${esc(i?.actual)}</span>`,
    offline: '<span class="badge off">Sin conexión</span>',
    error: `<span class="badge bad">✕ ${esc(upd.msg)}</span>`,
    new: `<span class="badge warn">Hay una versión nueva: v${esc(i?.ultima)}</span>${note}<div class="row upd-act">${i?.url_asset
      ? `<button class="btn pri" id="u-dl">Descargar e instalar</button><span class="sub">${i.tamano ? fmtMB(i.tamano) + " · " : ""}se verifica la firma antes de abrir el instalador</span>`
      : '<button class="btn pri" id="u-web">Ver la versión en GitHub</button><span class="sub">No hay instalador para este sistema; descárgalo desde la página.</span>'}</div>`,
    downloading: `<div class="row"><div class="prog" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${upd.pct || 0}"><i style="width:${upd.pct || 0}%"></i></div><span class="sub">Descargando… ${upd.pct || 0}%</span></div>`,
    done: `<span class="badge ok">✓ Firma verificada</span><div class="sub upd-notes">Se abrió el instalador (${esc(upd.msg)}). Sigue sus pasos para terminar.</div>`,
    failed: `<span class="badge bad">✕ Verificación fallida</span><div class="sub upd-notes">${esc(upd.msg)}. No se abrió el instalador.</div>`,
  }[upd.st] ?? "";
  if ($("#u-dl")) $("#u-dl").onclick = downloadUpdate;
  if ($("#u-web")) $("#u-web").onclick = () => invoke("open_release_page", { url: i.url_release }).catch((e) => toast(String(e)));
}
async function checkUpdate() {
  upd = { st: "checking" }; paintUpd();
  try {
    const info = await invoke("check_update");
    upd = { st: info.hay_nueva ? "new" : "ok", info };
  } catch (e) {
    upd = /^sin conexi/i.test(String(e)) ? { st: "offline" } : { st: "error", msg: String(e) };
  }
  paintUpd();
}
async function downloadUpdate() {
  const info = upd.info;
  upd = { st: "downloading", info, pct: 0 }; paintUpd();
  try {
    const r = await invoke("download_update");
    upd = { st: "done", info, msg: r.nombre };
  } catch (e) {
    upd = /^sin conexi|interrumpida/i.test(String(e)) ? { st: "error", msg: "Descarga interrumpida: " + String(e) } : { st: "failed", info, msg: String(e) };
  }
  paintUpd();
}
if (!window.__MODO_SERVIDOR__) window.__TAURI__.event.listen("update-progress", (e) => {
  const [done, total] = e.payload;
  if (upd.st !== "downloading") return;
  upd.pct = total ? Math.min(100, Math.floor(done * 100 / total)) : 0;
  const bar = document.querySelector("#u-box .prog");
  if (bar) { bar.firstElementChild.style.width = upd.pct + "%"; bar.setAttribute("aria-valuenow", upd.pct); bar.nextElementSibling.textContent = `Descargando… ${upd.pct}%`; }
});

async function renderSettings() {
  const el = $("#v-settings"), auto = await invoke("get_autostart"), hasPw = await invoke("has_password");
  const ext = await invoke("extension_status").catch(() => null);
  const [gOn, gTot, gOk] = await invoke("guard_status").catch(() => [0, 0, false]);
  const gBadge = !gOk ? '<span class="badge off">— No aplica en este sistema</span>'
    : !gTot ? `<span class="badge off">— Sin colas ${IS_WIN ? "protegibles" : "con filtros"}</span>`
    : gOn >= gTot ? `<span class="badge ok">✓ Activa en ${gOn} de ${gTot} colas</span>`
    : `<span class="badge warn">⚠ Activa en ${gOn} de ${gTot} colas</span>`;
  const ago = ext?.last_seen ? Math.max(0, Math.floor(Date.now() / 1000 - ext.last_seen)) : null;
  const extBadge = !ext || !ext.registered.length ? '<span class="badge off">— Sin navegador compatible</span>'
    : ago !== null && ago < 180 ? `<span class="badge ok">✓ Extensión conectada (hace ${ago} s)</span>`
    : '<span class="badge warn">⏳ Host registrado; falta cargar la extensión</span>';
  const s = settings;
  el.innerHTML = `<h2>Ajustes</h2><div class="cols">
  <div class="card"><h3>Comportamiento del control</h3>
    <div class="field">Trabajos sin datos suficientes ${segm("s-unk", [["allow", "Permitir"], ["block", "Bloquear"]], s.unknown_policy)}</div>
    <div class="field">Intervalo de revisión (s) <input id="s-int" type="number" min="1" max="60" value="${s.interval_secs}" style="width:70px"></div>
    <div class="field">Notificar bloqueos ${sw("s-nb", s.notify_block, "Notificar bloqueos")}</div>
    <div class="field">Notificar impresiones permitidas ${sw("s-na", s.notify_allow, "Notificar impresiones permitidas")}</div></div>
  <div class="card"><h3>Aplicación</h3>
    <div class="field">Iniciar con el sistema ${sw("s-auto", auto, "Iniciar con el sistema")}</div>
    <div class="field">Tema ${segm("s-theme", [["light", "Claro"], ["dark", "Oscuro"], ["system", "Sistema"]], s.theme)}</div>
    <div class="field">Contraseña de administrador <button class="btn" id="s-pw">${hasPw ? "Cambiar" : "Definir"}</button></div></div>
  ${window.__MODO_SERVIDOR__ ? "" : `<div class="card" id="s-ver"><h3>Versión</h3>
    <div class="field">SpoolGuard v${esc(appVersion)} <button class="btn" id="u-chk">Buscar actualización</button></div>
    <div id="u-box" class="upd" aria-live="polite"></div></div>`}
  ${gOk ? `<div class="card"><h3>Protección previa a la impresión</h3>
    <div class="field">Estado ${gBadge}</div>
    <div class="sub" style="margin:6px 0">${IS_WIN ? "Cierra el hueco en que un trabajo podría salir mientras la cola está activa y antes de que la app lo evalúe. " : "Algunas aplicaciones (Word, por ejemplo) envían el trabajo sin retención y se imprimiría antes de poder evaluarlo. "}${IS_WIN ? "Un procesador de impresión de SpoolGuard en el spooler de Windows hace esperar cada trabajo el veredicto de la app antes de enviarlo a la impresora. Pide el permiso de administrador (UAC) una vez; al actualizar el componente puede reiniciar el servicio Spooler (los trabajos en cola se conservan). Si agregas una impresora, pulsa Reparar." : "Un filtro de CUPS hace esperar cada trabajo el veredicto de la app antes de enviarlo a la impresora. Pide la contraseña de administrador una vez."}</div>
    <div class="field">Espera del veredicto (s) <input id="s-vto" type="number" min="1" max="600" value="${s.verdict_timeout_secs ?? 10}" style="width:70px"></div>
    <div class="field">Estricto: no imprimir si la app no responde ${sw("s-strict", !!s.strict, "Estricto: no imprimir si la app no responde")}</div>
    <div class="sub" style="margin:6px 0">${s.strict ? "Estricto activo: si la app no da el veredicto a tiempo (o está cerrada), el trabajo NO se imprime y queda retenido hasta que la app lo evalúe. Riesgo: con la app cerrada nadie imprime en estas colas."+(IS_WIN?" Conviene una espera larga (p. ej. 120 s).":" En macOS y Linux la espera no tiene límite: el plazo de arriba no se aplica y los trabajos de la cola quedan detrás del retenido.") : "Apagado: pasada la espera sin veredicto (o con la app cerrada) el trabajo se imprime, para no parar la oficina."}</div>
    <div class="field"><span class="sub"></span>${gOn ? '<button class="btn" id="g-off">Desactivar</button>' : ""}<button class="btn pri" id="g-inst">${gOn >= gTot && gTot ? "Reparar" : "Activar"}</button></div></div>` : ""}
  <div class="card"><h3>Extensión de navegador (opcional, para sitios web)</h3>
    <div class="field">Estado ${extBadge}</div>
    <div class="sub" style="margin:6px 0">El host se registra solo al abrir la app${ext?.registered.length ? " (" + esc(ext.registered.join(", ")) + ")" : ""}. Chrome exige cargar la extensión una vez: Modo desarrollador → Cargar descomprimida → elegir la carpeta.</div>
    <div class="field"><span class="sub">${esc(ext?.folder || "")}</span></div>
    <div class="field"><button class="btn" id="e-folder">Abrir carpeta de la extensión</button><button class="btn" id="e-chrome">Abrir chrome://extensions</button></div>
    <div class="field"><button class="btn" id="e-zip">Descargar extensión (.zip)</button><button class="btn pri" id="e-inst">Instalar en navegador…</button></div></div>
  <div class="card"><h3>Datos</h3>
    <div class="field">Retención del registro (días) <input id="s-ret" type="number" min="1" max="3650" value="${s.retention_days}" style="width:80px"></div></div>
  <div class="card" id="s-bk"><h3>Copia de seguridad</h3>
    <div class="sub" style="margin:0 0 6px">Guarda reglas, parámetros y registro en un archivo <b>.spoolguard.json</b> para restaurarlos o llevarlos a otro equipo o al servidor. Nunca incluye contraseña, tokens ni sesión.</div>
    <div class="field">Archivo de copia <span><button class="btn" id="bk-exp">Exportar…</button> <button class="btn" id="bk-imp">Importar…</button></span></div></div>
  <div class="card" id="s-sync"><h3>Enviar eventos al servidor</h3>
    <div class="field">Enviar eventos al servidor ${sw("s-se", s.send_events, "Enviar eventos al servidor")}</div>
    <div class="sub" style="margin:6px 0">Apagado por defecto: los eventos se quedan solo en este equipo y no se abre ninguna conexión. Al activarlo se envían, desde ese momento, fecha, equipo, IP, usuario, impresora, documento, sitio, aplicación, decisión, motivo y estado de cada trabajo. Si el servidor no responde se acumulan aquí y se reenvían después.</div>
    <div class="field">Dirección <input id="s-url" type="url" placeholder="https://servidor:8080" value="${esc(s.server_url)}" style="width:230px" autocomplete="off"></div>
    <div class="field">Token de equipo <input id="s-tok" type="password" placeholder="${s.server_token ? "Guardado (escribe para cambiar)" : "sgm_…"}" style="width:230px" autocomplete="off"></div>
    <div class="field"><span class="sub" id="s-sst"></span><button class="btn pri" id="s-ssave">Guardar</button></div></div></div>`;
  paintUpd();
  if ($("#u-chk")) $("#u-chk").onclick = checkUpdate;
  const push = async (patch) => { Object.assign(settings, patch); await call("set_settings", { settings }); applyTheme(); };
  const tog = (id, key) => $(id).onclick = async () => { await push({ [key]: !settings[key] }); renderSettings(); };
  tog("#s-nb", "notify_block"); tog("#s-na", "notify_allow");
  if ($("#g-inst")) $("#g-inst").onclick = async () => { $("#g-inst").disabled = true; try { await call("guard_install"); toast("Protección activada"); } finally { renderSettings(); } };
  if ($("#g-off")) $("#g-off").onclick = async () => {
    if (!await confirmNative("Se quitará la protección previa y las colas volverán a su configuración original. Los trabajos podrán salir sin esperar el veredicto de la app. ¿Continuar?", "Desactivar")) return;
    $("#g-off").disabled = true; try { await call("guard_uninstall"); toast("Protección desactivada"); } finally { renderSettings(); }
  };
  $("#s-auto").onclick = async () => { await call("set_autostart", { enabled: !auto }); renderSettings(); };
  const seg = (id, key) => $(id).querySelectorAll("button").forEach((b) => b.onclick = async () => { await push({ [key]: b.dataset.v }); renderSettings(); });
  seg("#s-unk", "unknown_policy"); seg("#s-theme", "theme");
  $("#e-folder").onclick = () => call("open_extension", { what: "folder" });
  $("#e-chrome").onclick = () => call("open_extension", { what: "chrome" });
  $("#e-zip").onclick = async () => {
    const path = await dlg.save({ defaultPath: "control-impresion-extension.zip", filters: [{ name: "ZIP", extensions: ["zip"] }] });
    if (!path) return;
    try { await call("export_extension_zip", { path }); toast("Guardado: " + path); } catch {}
  };
  $("#e-inst").onclick = () => installExtDialog().catch(() => {});
  $("#s-int").onchange = (e) => push({ interval_secs: Math.max(1, Math.min(60, +e.target.value || 1)) });
  $("#s-strict") && ($("#s-strict").onclick = async () => { await push({ strict: !settings.strict }); renderSettings(); });
  $("#s-vto") && ($("#s-vto").onchange = async (e) => {
    const v = Number(e.target.value);
    if (!Number.isInteger(v) || v < 1 || v > 600) { toast("La espera debe ser un número entero de segundos entre 1 y 600."); e.target.value = settings.verdict_timeout_secs ?? 10; return; }
    await push({ verdict_timeout_secs: v });
  });
  $("#s-ret").onchange = (e) => push({ retention_days: Math.max(1, +e.target.value || 90) });
  const paintSync = async () => {
    const st = await invoke("get_sync_status").catch(() => null), el = $("#s-sst");
    if (!st || !el) return;
    el.textContent = !settings.send_events ? "Envío apagado" : `${st.pending} pendientes` + (st.last_ok ? " · último envío " + fmt(st.last_ok) : "") + (st.last_error ? " · " + st.last_error : "");
  };
  paintSync();
  $("#s-se").onclick = async () => { try { await push({ send_events: !settings.send_events }); } catch { settings.send_events = false; } renderSettings(); };
  $("#s-ssave").onclick = async () => {
    const url = $("#s-url").value.trim(), tok = $("#s-tok").value.trim();
    if (url.startsWith("http://") && !await confirmNative("La dirección usa http:// (sin cifrar): el token viajaría en claro. ¿Guardar de todos modos?", "Guardar")) return;
    try { await push({ server_url: url, ...(tok ? { server_token: tok } : {}) }); toast("Guardado"); } catch {}
    settings = await invoke("get_settings"); // el token en claro no se conserva en la interfaz
    renderSettings();
  };
  $("#s-pw").onclick = async () => {
    const v = await ask("Contraseña de administrador", hasPw ? ["Actual", "Nueva (vacía = quitarla)"] : ["Nueva contraseña"]); if (!v) return;
    await call("set_password", { current: hasPw ? v[0] : "", new: hasPw ? v[1] : v[0] }); toast("Contraseña actualizada"); renderSettings();
  };
  $("#bk-exp").onclick = () => exportBackupDialog().catch(() => {});
  $("#bk-imp").onclick = () => importBackupFlow().catch(() => {});
}

document.documentElement.dataset.platform = /Windows/.test(navigator.userAgent) ? "windows" : /Mac/.test(navigator.userAgent) ? "mac" : "linux";
function applyTheme() {
  const t = settings.theme === "system" ? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light") : settings.theme;
  document.documentElement.dataset.theme = t;
  // El efecto translúcido nativo sigue el tema del SO: se alinea con el elegido (null = el del sistema).
  window.__TAURI__.window.getCurrentWindow().setTheme(settings.theme === "system" ? null : settings.theme).catch(() => {});
}
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => settings && settings.theme === "system" && applyTheme());
function paintPause() {
  const st = $("#state");
  st.className = "badge " + (paused ? "warn" : "ok");
  st.textContent = paused ? "⏸ Control en pausa" : "✓ Control activo";
  $("#pause").textContent = paused ? "Reanudar control" : "Pausar control";
  renderPrinters();
}
async function syncPause() {
  const p = await invoke("get_paused").catch(() => paused);
  if (p !== paused) { paused = p; paintPause(); }
}
$("#pause").onclick = async () => { paused = !paused; await call("set_paused", { paused }); paintPause(); };
function status() {
  const n = printers.filter((p) => p.rules.enabled || globalRules.enabled).length;
  $("#count").textContent = `${printers.length} impresoras · ${n} controladas`;
}
async function refreshPrinters() {
  try { printers = await invoke("list_printers"); } catch (e) { toast(String(e)); return; }
  renderPrinters(); status();
}
async function load() {
  settings = await invoke("get_settings"); applyTheme();
  if (!window.__MODO_SERVIDOR__) { appVersion = await invoke("app_version").catch(() => ""); $("#ver").textContent = appVersion ? "v" + appVersion : ""; }
  await refreshLog(); renderSettings();
  $("#v-printers").innerHTML = "<h2>Impresoras</h2><p class='sub'>Buscando impresoras…</p>";
  try { printers = await invoke("list_printers"); } catch (e) { printers = []; toast(String(e)); }
  globalRules = await invoke("get_global_rules").catch(emptyRules);
  renderPrinters(); renderRules(); status();
}

function go(view) {
  if (!started) return; // bloqueada: nada que mostrar
  if (view === "setup") { setupDialog(); return; }
  document.querySelectorAll("nav a").forEach((x) => x.classList.toggle("on", x.dataset.view === view));
  document.querySelectorAll("main section").forEach((s) => s.hidden = s.id !== "v-" + view);
  if (view === "log") refreshLog();
  if (view === "inventory") refreshInventory();
}
const toggleNav = (open) => { $("nav").classList.toggle("open", open); $("#scrim").hidden = !open; $("#menu").setAttribute("aria-expanded", open); };
$("#menu").onclick = () => toggleNav(true);
$("#scrim").onclick = () => toggleNav(false);
document.querySelectorAll("nav a").forEach((a) => a.onclick = (e) => { e.preventDefault(); go(a.dataset.view); toggleNav(false); });
// Menú nativo (Vista, Cmd+1..4, Cmd+,).
window.__TAURI__.event.listen("navigate", (e) => go(e.payload === "preferences" ? "settings" : e.payload));

$("#lockForm").onsubmit = async (e) => {
  e.preventDefault();
  if (await invoke("verify_password", { password: $("#lockPw").value })) { $("#lock").hidden = true; $("#lockPw").value = ""; await start(); }
  else $("#lockErr").textContent = "Contraseña incorrecta";
};

// Los datos se piden solo con la app desbloqueada (los comandos de lectura rechazan la llamada si está bloqueada).
let started = false;
async function start() {
  if (started) return;
  started = true;
  await load();
  if (!window.__MODO_SERVIDOR__ && !localStorage.getItem("sg-setup-done")) setupDialog();
}
(async () => {
  if (await invoke("has_password")) $("#lock").hidden = false;
  else await start();
  setInterval(() => { if (started && !$("#v-printers").hidden && !document.hidden) refreshPrinters(); }, 5000);
  setInterval(syncPause, 2000);
  setInterval(() => {
    if (!started || $("#v-log").hidden || logTab !== "events" || document.hidden || ["fq", "ff", "ft"].includes(document.activeElement.id)) return;
    invoke("query_events", { filter: logFilter(), page: filter.page, size: filter.size }).then((l) => { if (l.total !== log.total || l.rows[0]?.id !== log.rows[0]?.id) { log = l; renderLog(); } }).catch(() => {});
  }, 2000);
})();
