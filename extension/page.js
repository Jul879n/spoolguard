// Mundo de la página: anula window.print y corta los agentes locales de impresión
// (QZ Tray, Zebra Browser Print) cuando content.js lo ordena.
(() => {
  const original = window.print;
  const RealWS = window.WebSocket;
  const realFetch = window.fetch;
  const realOpen = XMLHttpRequest.prototype.open;
  const EV_AVISO = "controlimpresion:aviso";

  // Destinos de impresión local. QZ Tray: wss 8181/8282/8383/8484, ws 8182/8283/8384/8485
  // (qzind/tray Constants.java; su cliente JS usa localhost y localhost.qz.io).
  // Zebra Browser Print: http 9100, https 9101.
  const PUERTOS = new Set(["8181", "8182", "8282", "8283", "8383", "8384", "8484", "8485", "9100", "9101"]);
  const hostLocal = (h) =>
    h === "localhost" || h === "localhost.qz.io" || h === "[::1]" || h === "0.0.0.0" || /^127(\.\d{1,3}){3}$/.test(h);
  const MUERTO = "http://blocked.invalid/"; // falla en red de forma nativa (error/close, status 0)

  let activo = false; // agentes locales (QZ Tray / Zebra) bloqueados
  let refs = new Set(); // WeakRef a sockets abiertos hacia destinos de impresión

  const esImpresion = (u) => {
    try {
      const x = new URL(String(u), location.href);
      return /^(wss?|https?):$/.test(x.protocol) && hostLocal(x.hostname.toLowerCase()) && PUERTOS.has(x.port);
    } catch { return false; }
  };
  let cortado = false; // se cortó una conexión a un agente local mientras estaba bloqueado
  const avisar = () => { cortado = true; document.dispatchEvent(new CustomEvent(EV_AVISO)); };

  // Protocolo de QZ Tray: cada mensaje es JSON {"call": "...", ...}; qz.print() envía call "print"
  // (qz-tray.js: dataPromise('print', ...)). Solo se avisa; el contenido nunca sale de la página.
  function vigilarEnvio(ws) {
    const realSend = ws.send;
    ws.send = function send(data) {
      try {
        if (typeof data === "string" && data.includes('"print"') && JSON.parse(data).call === "print") {
          document.dispatchEvent(new CustomEvent("controlimpresion:impresion"));
        }
      } catch {}
      return Reflect.apply(realSend, this, arguments);
    };
  }

  const WSProxy = new Proxy(RealWS, {
    construct(t, args) {
      const dest = esImpresion(args[0]);
      if (dest && activo) {
        avisar();
        return Reflect.construct(t, [MUERTO.replace("http", "ws")]);
      }
      const ws = Reflect.construct(t, args);
      if (dest) { refs.add(new WeakRef(ws)); vigilarEnvio(ws); }
      return ws;
    },
  });

  const fetchBloq = function fetch(input, init) {
    if (activo && esImpresion(input instanceof Request ? input.url : input)) {
      avisar();
      return Promise.reject(new TypeError("Failed to fetch"));
    }
    return Reflect.apply(realFetch, this, arguments);
  };

  const openBloq = function open(...a) {
    if (activo && esImpresion(a[1])) { avisar(); a[1] = MUERTO; }
    return Reflect.apply(realOpen, this, a);
  };

  function instalar() {
    window.WebSocket = WSProxy;
    Object.defineProperty(RealWS.prototype, "constructor", { value: WSProxy, writable: true, configurable: true });
    window.fetch = fetchBloq;
    XMLHttpRequest.prototype.open = openBloq;
  }
  function restaurar() {
    if (window.WebSocket === WSProxy) {
      window.WebSocket = RealWS;
      Object.defineProperty(RealWS.prototype, "constructor", { value: RealWS, writable: true, configurable: true });
    }
    if (window.fetch === fetchBloq) window.fetch = realFetch;
    if (XMLHttpRequest.prototype.open === openBloq) XMLHttpRequest.prototype.open = realOpen;
  }
  function cerrarAbiertos() {
    for (const r of refs) {
      const ws = r.deref();
      if (ws && ws.readyState <= 1) { try { ws.close(); } catch {} }
    }
    refs = new Set();
  }

  // Siempre instalado (pasa todo sin tocar) para poder rastrear sockets abiertos antes de que
  // lleguen las reglas; solo bloquea mientras `activo`.
  instalar();

  document.addEventListener("controlimpresion:bloquear", (e) => {
    // detail: JSON '{"print":bool,"agentes":bool}' (un string cruza los mundos en Firefox); también se
    // acepta un objeto o un booleano (versiones anteriores, este último vale para ambos).
    let d = e.detail;
    if (typeof d === "string") { try { d = JSON.parse(d); } catch { d = null; } }
    const imprimir = typeof d === "object" && d !== null ? !!d.print : !!d;
    const antes = activo;
    activo = typeof d === "object" && d !== null ? !!d.agentes : !!d;
    // La página (p. ej. qz-tray.js) conecta una sola vez al cargar y no reintenta: avisa para ofrecer recargar.
    if (antes && !activo && cortado) { cortado = false; document.dispatchEvent(new CustomEvent("controlimpresion:desbloqueado")); }
    window.print = imprimir ? () => {} : original;
    if (activo) { instalar(); cerrarAbiertos(); }
    else { restaurar(); refs = new Set(); }
  });
})();
