// Capa de conexión con dos proveedores: "escritorio" (host nativo de la app) y "servidor" (control-server por HTTP).
// Cachea las reglas en storage.local; si falla la conexión se conserva la última caché.
const api = globalThis.browser ?? chrome;
const HOST = "cl.cohasa.controlimpresion";
const POLL_MS = 15000; // modo servidor: sondeo mientras el service worker esté vivo
const TIMEOUT_MS = 8000;

const esLista = (x) => Array.isArray(x) && x.every((d) => typeof d === "string");
const reglasValidas = (r) => !!r && esLista(r.block) && (r.allow_only === null || esLista(r.allow_only));

const accion = api.action ?? api.browserAction;

// Modo efectivo: "servidor" solo si el usuario lo eligió y hay URL y token; si no, el host nativo.
async function config() {
  const d = await api.storage.local.get(["modo", "servidor"]);
  const s = d.servidor;
  const srv = d.modo === "servidor" && s && s.url && s.token ? { url: String(s.url), token: String(s.token) } : null;
  return { modo: srv ? "servidor" : "escritorio", srv };
}

// Insignia del ícono: "✓" verde si hay conexión, "!" rojo si no.
let ultimoOkInsignia = false;
function pintarInsignia(ok) {
  if (ok !== null) ultimoOkInsignia = ok;
  ok = ultimoOkInsignia;
  try {
    accion.setBadgeText({ text: ok ? "✓" : "!" });
    accion.setBadgeBackgroundColor({ color: ok ? "#16a34a" : "#dc2626" });
  } catch (e) {}
}

async function estadoPrevio() {
  return (await api.storage.local.get("estado")).estado || {};
}

// --- Proveedor servidor ---

async function pedirServidor(srv, ruta, opts = {}) {
  let r;
  try {
    r = await fetch(srv.url + ruta, {
      ...opts,
      credentials: "omit",
      cache: "no-store",
      headers: { ...(opts.headers || {}), Authorization: "Bearer " + srv.token },
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
  } catch (e) {
    // El fetch falla sin detalle: red caída, servidor apagado o certificado TLS no confiable. El servidor responde
    // CORS a la extensión, así que el permiso de host no es la causa (el fetch funciona sin él, verificado en Chrome).
    throw new Error(srv.url.startsWith("https:")
      ? "No se pudo conectar de forma segura: certificado no confiable o servidor inalcanzable"
      : "No se pudo conectar con el servidor: dirección incorrecta, servidor apagado o red caída");
  }
  if (!r.ok) {
    throw new Error(r.status === 401 ? "Token inválido o revocado" : r.status === 429 ? "Demasiadas peticiones al servidor" : `El servidor respondió ${r.status}`);
  }
  return r.json();
}

// --- Estado compartido ---

// Guarda reglas y estado solo si el modo no cambió mientras se esperaba la respuesta.
async function guardarReglas(r, modo, ahora) {
  if (!reglasValidas(r) || (await config()).modo !== modo) return false;
  await api.storage.local.set({ rules: { block: r.block, allow_only: r.allow_only } });
  const previo = await estadoPrevio();
  await api.storage.local.set({ estado: { ...previo, ok: true, modo, ultimoOk: ahora, reglasEn: ahora, ultimoIntento: ahora, error: null } });
  pintarInsignia(true);
  return true;
}

async function guardarFallo(modo, ahora, error) {
  if ((await config()).modo !== modo) return;
  const previo = await estadoPrevio();
  const mismo = previo.modo === modo;
  await api.storage.local.set({
    estado: { ok: false, modo, ultimoOk: mismo ? previo.ultimoOk ?? 0 : 0, ultimoIntento: ahora, error: String(error).slice(0, 200), reglasEn: previo.reglasEn ?? 0 },
  });
  pintarInsignia(false);
}

let fallos = 0, ultimoSondeo = 0;
let enCurso = null;
// Pide las reglas al proveedor activo (varias llamadas simultáneas comparten la misma petición).
function refrescar() {
  return (enCurso ??= (async () => {
    const { modo, srv } = await config();
    const ahora = Date.now();
    ultimoSondeo = ahora;
    try {
      const r = srv ? await pedirServidor(srv, "/ext/rules") : await api.runtime.sendNativeMessage(HOST, { type: "get_rules" });
      if (await guardarReglas(r, modo, ahora)) fallos = 0;
      else if (!reglasValidas(r)) throw new Error(srv ? "Respuesta inválida del servidor" : "Respuesta inválida del host");
    } catch (e) {
      fallos++;
      await guardarFallo(modo, ahora, (e && e.message) || e);
    }
  })().finally(() => { enCurso = null; }));
}

// --- Proveedor nativo: puerto persistente (el host empuja las reglas apenas cambia config.json) ---

let puerto = null;
let cerrandoPuerto = false;
async function conectar() {
  if (puerto || (await config()).modo !== "escritorio") return;
  try {
    const p = api.runtime.connectNative(HOST);
    puerto = p;
    p.onMessage.addListener((m) => { guardarReglas(m, "escritorio", Date.now()); });
    p.onDisconnect.addListener(async () => {
      if (puerto === p) puerto = null;
      if (cerrandoPuerto) return;
      await guardarFallo("escritorio", Date.now(), "Conexión con la app cerrada");
    });
    p.postMessage({ type: "subscribe" });
  } catch (e) { puerto = null; }
}
function cerrarPuerto() {
  if (!puerto) return;
  cerrandoPuerto = true;
  try { puerto.disconnect(); } catch (e) {}
  puerto = null;
  cerrandoPuerto = false;
}

// --- Proveedor servidor: flujo SSE (/ext/events); el sondeo de 15 s queda de respaldo ---

let flujo = null; // AbortController del flujo abierto
async function conectarFlujo() {
  if (flujo) return;
  const { srv } = await config();
  if (!srv || flujo) return;
  const ac = (flujo = new AbortController());
  try {
    const r = await fetch(srv.url + "/ext/events", { headers: { Authorization: "Bearer " + srv.token }, signal: ac.signal, cache: "no-store" });
    if (!r.ok || !r.body) throw new Error(String(r.status));
    const lector = r.body.pipeThrough(new TextDecoderStream()).getReader();
    let buf = "";
    for (;;) {
      const { value, done } = await lector.read();
      if (done) break;
      buf += value;
      let i;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const bloque = buf.slice(0, i);
        buf = buf.slice(i + 2);
        const datos = bloque.split("\n").find((l) => l.startsWith("data: "));
        if (bloque.startsWith("event: rules") && datos) {
          try { if (await guardarReglas(JSON.parse(datos.slice(6)), "servidor", Date.now())) fallos = 0; } catch (e) {}
        }
      }
    }
  } catch (e) { /* cortado o sin conexión: la alarma y el sondeo reintentan */ }
  finally { if (flujo === ac) flujo = null; }
}
function cerrarFlujo() {
  const f = flujo;
  flujo = null;
  try { f && f.abort(); } catch (e) {}
}

// --- Programación: alarma (despierta el worker) + sondeo de 15 s en modo servidor ---

async function programar() {
  const { modo } = await config();
  // Chrome permite 30 s como mínimo (antes 1 min); en modo escritorio basta 1 min.
  api.alarms.create("refrescar", { periodInMinutes: modo === "servidor" ? 0.5 : 1 });
  if (modo === "servidor") { cerrarPuerto(); conectarFlujo(); } else { cerrarFlujo(); conectar(); }
}

api.alarms.onAlarm.addListener((a) => { if (a.name === "refrescar") { conectar(); conectarFlujo(); refrescar(); } });

setInterval(async () => {
  const { srv } = await config();
  if (!srv) return;
  // Reintento con espera creciente (hasta 2 min) si el servidor no responde.
  const espera = fallos >= 3 ? Math.min(120000, POLL_MS * 2 ** (fallos - 2)) : POLL_MS;
  if (Date.now() - ultimoSondeo >= espera - 500) refrescar();
}, POLL_MS);

// Al cambiar de pestaña se actualizan las reglas (como mucho cada 5 s) sin esperar al sondeo.
api.tabs.onActivated.addListener(() => { if (Date.now() - ultimoSondeo > 5000) refrescar(); });

api.storage.onChanged.addListener((c, area) => {
  if (area !== "local") return;
  if (c.modo || c.servidor) { fallos = 0; cerrarFlujo(); programar().then(refrescar); }
});

api.runtime.onInstalled.addListener(() => { programar().then(refrescar); });
api.runtime.onStartup.addListener(() => { programar().then(refrescar); });
programar().then(refrescar);

// --- Intenciones: dominio y título del sitio que imprime, para asociarlos al trabajo de la cola ---
// Se valida el remitente y el contenido: solo http(s), textos acotados y a lo más 1 aviso cada 2 s.
let ultimo = 0;
api.runtime.onMessage.addListener((m, sender) => {
  if (!m || m.type !== "print_intent") return;
  if (sender.id !== api.runtime.id || !sender.tab) return;
  if (typeof m.url !== "string" || typeof m.title !== "string") return;
  if (!/^https?:\/\/[^/\s]+$/i.test(m.url) || m.url.length > 300) return;
  const ahora = Date.now();
  if (ahora - ultimo < 2000) return;
  ultimo = ahora;
  const msg = { url: m.url, title: m.title.slice(0, 300) };
  if (m.kind === "agent") msg.kind = "agent";
  enviarIntencion(msg);
});

async function enviarIntencion(msg) {
  const { srv } = await config();
  try {
    if (srv) await pedirServidor(srv, "/ext/intent", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(msg) });
    else await api.runtime.sendNativeMessage(HOST, { type: "print_intent", ...msg });
  } catch (e) {}
}

// Popup: pide refrescar las reglas ahora (solo desde la propia extensión).
api.runtime.onMessage.addListener((m, sender, responder) => {
  if (!m || m.type !== "refrescar" || sender.id !== api.runtime.id || sender.tab) return; // solo el popup, no un content script
  refrescar().then(() => responder({ ok: true }));
  return true;
});

// --- Actualización de la extensión ---
// Escritorio: el host nativo informa la versión empaquetada en la app y copia sus archivos a <datos>/extension;
// luego el popup recarga la extensión. Servidor: /ext/version informa la versión; la descarga la hace el popup.
// Nunca se ejecuta código descargado: solo se ofrece el archivo o se recarga la carpeta ya instalada.

async function buscarActualizacion() {
  const actual = api.runtime.getManifest().version;
  const { modo, srv } = await config();
  if (srv) {
    const r = await pedirServidor(srv, "/ext/version");
    if (typeof r.version !== "string" || !/^[0-9a-f]{64}$/.test(r.sha256 || "")) throw new Error("Respuesta inválida del servidor");
    // Solo una ruta del mismo servidor; nunca otro destino.
    const ruta = typeof r.url === "string" && /^\/[^/]/.test(r.url) ? r.url : "/ext/package.zip";
    return { modo, actual, version: r.version, sha256: r.sha256, size: r.size, url: srv.url + ruta };
  }
  let r;
  try { r = await api.runtime.sendNativeMessage(HOST, { type: "get_version" }); }
  catch (e) { throw new Error("Sin conexión con la app"); }
  if (!r || typeof r.version !== "string") throw new Error((r && r.error) || "Respuesta inválida de la app");
  return { modo, actual, version: r.version };
}

async function aplicarActualizacion() {
  const { srv } = await config();
  if (srv) throw new Error("En modo servidor la actualización se descarga");
  let r;
  try { r = await api.runtime.sendNativeMessage(HOST, { type: "update_extension" }); }
  catch (e) { throw new Error("Sin conexión con la app"); }
  if (!r || r.ok !== true) throw new Error((r && r.error) || "La app no pudo copiar la extensión");
  return { ok: true, version: r.version };
}

api.runtime.onMessage.addListener((m, sender, responder) => {
  if (!m || sender.id !== api.runtime.id || sender.tab) return; // solo el popup, no un content script
  const f = m.type === "buscar_actualizacion" ? buscarActualizacion : m.type === "aplicar_actualizacion" ? aplicarActualizacion : null;
  if (!f) return;
  f().then(responder, (e) => responder({ error: String((e && e.message) || e) }));
  return true;
});
