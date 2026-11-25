// Popup: modo de conexión (escritorio/servidor), estado del sitio activo, interruptores y reglas cacheadas.
const api = globalThis.browser ?? chrome;
const $ = (id) => document.getElementById(id);
const MAX = 8;

const ICO = {
  ok: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>',
  no: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 6 6 18M6 6l12 12"/></svg>',
  na: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12h14"/></svg>',
};

function badge(el, tipo, texto) {
  el.className = "badge big " + tipo;
  el.innerHTML = ICO[tipo];
  const s = document.createElement("span");
  s.textContent = texto;
  el.append(s);
}

function hace(ms) {
  if (!ms) return "nunca";
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `hace ${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `hace ${m} min`;
  return `hace ${Math.round(m / 60)} h`;
}

// Misma lógica que content.js.
const coincide = (host, lista) => lista.some((d) => {
  d = d.toLowerCase();
  return host === d || host.endsWith("." + d);
});

function chips(el, lista) {
  el.replaceChildren();
  if (!lista.length) {
    const v = document.createElement("span");
    v.className = "vacio";
    v.textContent = "Ninguno";
    el.append(v);
    return;
  }
  const mk = (t, c) => {
    const s = document.createElement("span");
    s.className = "chip" + (c ? " " + c : "");
    s.textContent = t;
    el.append(s);
  };
  lista.slice(0, MAX).forEach((d) => mk(d));
  if (lista.length > MAX) mk(`+${lista.length - MAX} más`, "mas");
}

async function hostActivo() {
  // activeTab basta para leer la URL de la pestaña activa al abrir el popup; evita host_permissions amplios.
  try {
    const [t] = await api.tabs.query({ active: true, currentWindow: true });
    const u = new URL(t.url);
    return /^https?:$/.test(u.protocol) ? u.hostname.toLowerCase() : null;
  } catch (e) {
    return null;
  }
}

let modoUI = null; // modo mostrado en el selector (puede diferir del guardado hasta pulsar "Guardar y conectar")
let guardado = {}; // última lectura de storage.local

const CLAVES = ["estado", "rules", "modo", "servidor"];

function modoGuardado(d) {
  const s = d.servidor;
  return d.modo === "servidor" && s && s.url && s.token ? "servidor" : "escritorio";
}

function pintarModo() {
  document.querySelectorAll("#modo [role=radio]").forEach((b) => {
    const on = b.dataset.modo === modoUI;
    b.setAttribute("aria-checked", String(on));
    b.tabIndex = on ? 0 : -1;
  });
  $("form-srv").hidden = modoUI !== "servidor";
  $("url").value ||= (guardado.servidor && guardado.servidor.url) || "";
  $("token").placeholder = guardado.servidor && guardado.servidor.token ? "Token guardado" : "Pega el token";
}

async function elegirModo(m) {
  modoUI = m;
  if (m === "escritorio" && modoGuardado(guardado) !== "escritorio") {
    await api.storage.local.set({ modo: "escritorio" });
    msg("");
  }
  pintarModo();
  pintarConexion();
  if (m === "servidor") $("url").focus();
}

function msg(texto, error) {
  const el = $("form-msg");
  el.textContent = texto;
  el.classList.toggle("err", !!error);
}

// Guarda URL y token; el permiso de host se pide aquí porque exige un gesto del usuario.
$("form-srv").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  let u;
  try {
    const t = $("url").value.trim();
    u = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(t) ? t : "https://" + t);
    if (!/^https?:$/.test(u.protocol) || !u.hostname) throw 0;
  } catch (e) {
    return msg("Escribe una dirección http(s) válida, p. ej. https://servidor:8080", true);
  }
  const token = $("token").value.trim() || (guardado.servidor && guardado.servidor.token) || "";
  if (!token) return msg("Pega el token que creaste en el panel (/tokens.html).", true);
  const url = u.origin + u.pathname.replace(/\/+$/, "");
  const b = $("guardar");
  b.disabled = true;
  try {
    // Sin await previo: conserva el gesto del usuario.
    const ok = await api.permissions.request({ origins: [`${u.protocol}//${u.hostname}/*`] });
    if (!ok) return msg("Sin permiso para comunicarse con ese servidor.", true);
    await api.storage.local.set({ modo: "servidor", servidor: { url, token } });
    $("token").value = "";
    $("url").value = url;
    const local = u.protocol === "http:" && !/^(localhost|127(\.\d+){3}|\[::1\])$/.test(u.hostname);
    msg(local ? "Conectando… Aviso: con http el token viaja sin cifrar por la red." : "Conectando…");
    try { await api.runtime.sendMessage({ type: "refrescar" }); } catch (e) {}
    const { estado } = await api.storage.local.get("estado");
    if (estado && estado.ok) msg(local ? "Conectado. Aviso: con http el token viaja sin cifrar por la red." : "Conectado.");
    else msg((estado && estado.error) || "No se pudo conectar.", true);
  } catch (e) {
    msg(String((e && e.message) || e), true);
  } finally {
    b.disabled = false;
  }
});

const radios = document.querySelectorAll("#modo [role=radio]");
radios.forEach((b, i) => {
  b.addEventListener("click", () => elegirModo(b.dataset.modo));
  b.addEventListener("keydown", (e) => {
    const d = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!d) return;
    e.preventDefault();
    const n = radios[(i + d + radios.length) % radios.length];
    n.focus();
    elegirModo(n.dataset.modo);
  });
});

function pintarConexion() {
  const e = guardado.estado || {};
  const efectivo = modoGuardado(guardado);
  const srv = efectivo === "servidor";
  const nombre = srv ? "el servidor" : "la app";
  // Un estado de otro modo no vale: se espera la primera respuesta del modo actual.
  const vigente = !e.modo || e.modo === efectivo;
  if (modoUI !== efectivo) {
    badge($("con-badge"), "na", modoUI === "servidor" ? "Servidor sin configurar" : "Modo escritorio sin aplicar");
    $("con-texto").textContent = modoUI === "servidor" ? "Escribe la dirección y el token y pulsa Guardar y conectar." : "";
    $("con-ultima").textContent = "";
    return;
  }
  if (!vigente) {
    badge($("con-badge"), "na", "Conectando…");
    $("con-texto").textContent = "";
    $("con-ultima").textContent = "";
    return;
  }
  if (e.ok) {
    badge($("con-badge"), "ok", srv ? "Conectado con el servidor" : "Conectada con la app");
    $("con-texto").textContent = "Las reglas se actualizan al instante.";
  } else {
    badge($("con-badge"), "no", `Sin conexión con ${nombre}`);
    $("con-texto").textContent = (srv && e.error ? e.error + ". " : srv ? "" : "Abre SpoolGuard. ") + `Se usan las últimas reglas (${hace(e.reglasEn)}).`;
  }
  $("con-ultima").textContent = "Última sincronización: " + hace(e.ultimoOk);
}

async function pintar() {
  guardado = await api.storage.local.get(CLAVES);
  const { rules } = guardado;
  if (modoUI === null) modoUI = modoGuardado(guardado);
  pintarModo();
  pintarConexion();

  const host = await hostActivo();
  const block = (rules && rules.block) || [];
  const allow = rules && Array.isArray(rules.allow_only) ? rules.allow_only : null;
  if (!host) {
    $("sitio-host").textContent = "No aplica";
    badge($("sitio-badge"), "na", "Sin control en esta pestaña");
    $("sitio-regla").textContent = "Solo se controlan sitios http(s).";
  } else {
    $("sitio-host").textContent = host;
    if (coincide(host, block)) {
      badge($("sitio-badge"), "no", "Impresión bloqueada");
      $("sitio-regla").textContent = "Regla: dominio bloqueado";
    } else if (allow && !coincide(host, allow)) {
      badge($("sitio-badge"), "no", "Impresión bloqueada");
      $("sitio-regla").textContent = "Regla: no está en la lista de solo permitir";
    } else {
      badge($("sitio-badge"), "ok", "Impresión permitida");
      $("sitio-regla").textContent = allow ? "Regla: dominio en la lista de solo permitir" : "Sin regla que lo bloquee";
    }
  }

  $("n-block").textContent = block.length;
  chips($("chips-block"), block);
  $("grupo-allow").hidden = !allow || !allow.length;
  if (allow) {
    $("n-allow").textContent = allow.length;
    chips($("chips-allow"), allow);
  }
}

// --- Actualización de la extensión ---

const cmpVersion = (a, b) => {
  const x = a.split(".").map((n) => parseInt(n, 10) || 0), y = b.split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] || 0) - (y[i] || 0);
    if (d) return Math.sign(d);
  }
  return 0;
};

function pintarUpd({ tipo, texto, info = "", sha = "", accion = null }) {
  if (tipo) { badge($("upd-badge"), tipo, texto); $("upd-badge").hidden = false; } else $("upd-badge").hidden = true;
  $("upd-info").textContent = info;
  $("upd-sha").textContent = sha;
  $("upd-sha").hidden = !sha;
  const a = $("upd-accion");
  a.hidden = !accion;
  a.disabled = false;
  a.textContent = accion ? accion.texto : "";
  a.onclick = accion ? accion.fn : null;
}

const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");

// Servidor: descarga el zip con el token, comprueba su SHA-256 contra /ext/version y lo guarda como archivo.
async function descargarPaquete(u) {
  const a = $("upd-accion");
  a.disabled = true;
  try {
    const { servidor } = await api.storage.local.get("servidor");
    const r = await fetch(u.url, { credentials: "omit", cache: "no-store", headers: { Authorization: "Bearer " + servidor.token }, signal: AbortSignal.timeout(20000) });
    if (!r.ok) throw new Error(`El servidor respondió ${r.status}`);
    const datos = await r.arrayBuffer();
    const sha = hex(await crypto.subtle.digest("SHA-256", datos));
    if (sha !== u.sha256) throw new Error("La descarga no coincide con el SHA-256 anunciado: se descartó");
    const enlace = document.createElement("a");
    enlace.href = URL.createObjectURL(new Blob([datos], { type: "application/zip" }));
    enlace.download = `spoolguard-extension-${u.version}.zip`;
    document.body.append(enlace);
    enlace.click();
    enlace.remove();
    pintarUpd({
      tipo: "ok", texto: `Descargado v${u.version}`,
      info: "Descomprime el zip sobre la carpeta de la extensión y pulsa Recargar en chrome://extensions (Firefox: about:debugging). SHA-256 verificado:",
      sha,
    });
  } catch (e) {
    pintarUpd({ tipo: "no", texto: "No se pudo descargar", info: String((e && e.message) || e) });
  }
}

async function buscarActualizacion() {
  const b = $("upd-buscar");
  b.disabled = true;
  pintarUpd({ tipo: "na", texto: "Buscando…" });
  try {
    const u = await api.runtime.sendMessage({ type: "buscar_actualizacion" });
    if (!u || u.error) throw new Error((u && u.error) || "Sin respuesta");
    if (cmpVersion(u.version, u.actual) <= 0) return pintarUpd({ tipo: "ok", texto: `Al día · v${u.actual}` });
    const nueva = `Hay una versión nueva: v${u.version}`;
    if (u.modo === "servidor") {
      return pintarUpd({ tipo: "na", texto: nueva, info: `Instalada: v${u.actual}. SHA-256 del paquete:`, sha: u.sha256, accion: { texto: `Descargar v${u.version}`, fn: () => descargarPaquete(u) } });
    }
    pintarUpd({ tipo: "na", texto: nueva, info: `Instalada: v${u.actual}. Se copiarán los archivos de la app y la extensión se recargará.`, accion: { texto: "Actualizar", fn: aplicarActualizacion } });
  } catch (e) {
    pintarUpd({ tipo: "no", texto: "Sin conexión", info: String((e && e.message) || e) });
  } finally {
    b.disabled = false;
  }
}

// Escritorio: el host copia los archivos a <datos>/extension y se recarga la extensión.
async function aplicarActualizacion() {
  $("upd-accion").disabled = true;
  try {
    const r = await api.runtime.sendMessage({ type: "aplicar_actualizacion" });
    if (!r || r.error) throw new Error((r && r.error) || "Sin respuesta");
    pintarUpd({ tipo: "ok", texto: `Actualizada a v${r.version}`, info: "Recargando la extensión…" });
    setTimeout(() => api.runtime.reload(), 400);
  } catch (e) {
    pintarUpd({ tipo: "no", texto: "No se pudo actualizar", info: String((e && e.message) || e) });
  }
}

$("upd-buscar").addEventListener("click", buscarActualizacion);

$("refrescar").addEventListener("click", async (ev) => {
  const b = ev.currentTarget;
  b.disabled = true;
  try { await api.runtime.sendMessage({ type: "refrescar" }); } catch (e) {}
  await pintar();
  b.disabled = false;
});

// Los cambios hechos desde aquí o desde el servicio se reflejan al instante.
api.storage.onChanged.addListener((c, area) => {
  if (area !== "local") return;
  if (c.modo) modoUI = null;
  pintar();
});

pintar();
