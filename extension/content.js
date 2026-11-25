// Bloquea la impresión en los sitios que dicten las reglas cacheadas.
(() => {
  const api = globalThis.browser ?? chrome;
  const host = location.hostname.toLowerCase();
  if (!host) return;

  const coincide = (lista) => lista.some((d) => {
    d = d.toLowerCase();
    return host === d || host.endsWith("." + d);
  });

  const bloqueado = (r) =>
    !!r && (coincide(r.block || []) || (Array.isArray(r.allow_only) && !coincide(r.allow_only)));

  // Dos frentes independientes: la impresión de la página (window.print, Ctrl+P, @media print) y los
  // agentes locales (QZ Tray / Zebra). Ambos se activan cuando el sitio está bloqueado.
  let pOn = false, aOn = false;
  let estilo = null;
  let aviso = null;

  const teclas = (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "p") {
      e.preventDefault();
      e.stopImmediatePropagation();
      mostrarAviso();
    }
  };

  function actualizar(p, a) {
    if (p === pOn && a === aOn) return;
    const habia = pOn || aOn;
    if (p !== pOn) {
      if (p) {
        estilo = document.createElement("style");
        estilo.textContent = "@media print{body{display:none!important}}";
        document.documentElement.append(estilo);
        window.addEventListener("keydown", teclas, true);
        window.addEventListener("beforeprint", mostrarAviso, true);
      } else {
        estilo?.remove();
        window.removeEventListener("keydown", teclas, true);
        window.removeEventListener("beforeprint", mostrarAviso, true);
      }
    }
    pOn = p;
    aOn = a;
    // detail primitivo (JSON): en Firefox un objeto no cruza del mundo aislado al de la página.
    document.dispatchEvent(new CustomEvent("controlimpresion:bloquear", { detail: JSON.stringify({ print: p, agentes: a }) }));
    if ((p || a) && !habia) {
      aviso = document.createElement("div");
      aviso.textContent = "Impresión bloqueada por política";
      aviso.setAttribute("role", "status");
      aviso.style.cssText =
        "position:fixed;bottom:12px;right:12px;z-index:2147483647;padding:8px 14px;" +
        "background:#fff3cd;color:#664d03;border:1px solid #664d03;border-radius:6px;" +
        "font:14px system-ui,sans-serif;display:none";
      document.documentElement.append(aviso);
      document.addEventListener("controlimpresion:aviso", mostrarAviso);
    } else if (!(p || a) && habia) {
      document.removeEventListener("controlimpresion:aviso", mostrarAviso);
      aviso?.remove();
      aviso = null;
    }
  }

  // Tras desbloquear un sitio cuya conexión al agente local se cortó, la página no reconecta sola: se ofrece recargar.
  document.addEventListener("controlimpresion:desbloqueado", () => {
    if (window !== window.top) return;
    const caja = document.createElement("div");
    caja.setAttribute("role", "status");
    caja.style.cssText =
      "position:fixed;bottom:12px;right:12px;z-index:2147483647;display:flex;align-items:center;gap:10px;padding:8px 12px;" +
      "background:#d1e7dd;color:#0a3622;border:1px solid #0a3622;border-radius:6px;font:14px system-ui,sans-serif";
    const t = document.createElement("span");
    t.textContent = "Impresión desbloqueada. Recarga la página para reconectar con el agente de impresión.";
    const b = document.createElement("button");
    b.textContent = "Recargar";
    b.style.cssText = "font:inherit;font-weight:600;padding:4px 10px;border:1px solid #0a3622;border-radius:6px;background:#fff;color:#0a3622;cursor:pointer";
    b.onclick = () => location.reload();
    const x = document.createElement("button");
    x.textContent = "✕";
    x.setAttribute("aria-label", "Cerrar");
    x.style.cssText = "font:inherit;padding:2px 8px;border:1px solid #0a3622;border-radius:6px;background:transparent;color:#0a3622;cursor:pointer";
    x.onclick = () => caja.remove();
    caja.append(t, b, x);
    document.documentElement.append(caja);
  });

  function mostrarAviso() {
    if (!aviso) return;
    aviso.style.display = "block";
    setTimeout(() => aviso && (aviso.style.display = "none"), 4000);
  }

  // Imprimir en un sitio permitido: se informa para que el servicio asocie el dominio al trabajo.
  // Solo eventos reales del navegador (isTrusted): una página no puede fabricar intenciones.
  // Se envía solo el origen (sin ruta ni parámetros) para no filtrar URLs con datos sensibles.
  window.addEventListener("beforeprint", (e) => {
    if (!e.isTrusted || pOn || window !== window.top) return;
    api.runtime.sendMessage({ type: "print_intent", url: location.origin, title: String(document.title).slice(0, 300) }).catch(() => {});
  }, true);

  // Una página que habla con QZ Tray ordena imprimir sin beforeprint: page.js lo avisa por evento DOM.
  // Solo origen y título (nunca el contenido del mensaje); como mucho 1 aviso cada 2 s.
  let ultimoAgente = 0;
  document.addEventListener("controlimpresion:impresion", (e) => {
    if (e.target !== document || !location.protocol.startsWith("http")) return;
    const ahora = Date.now();
    if (ahora - ultimoAgente < 2000) return;
    ultimoAgente = ahora;
    api.runtime.sendMessage({ type: "print_intent", url: location.origin, title: String(document.title).slice(0, 300), kind: "agent" }).catch(() => {});
  });

  // Las reglas las deja background.js en storage.local; sin interruptores: si el sitio está bloqueado se
  // bloquea la impresión de la página y los agentes locales juntos.
  let reglas = null;
  const aplicar = () => { const b = bloqueado(reglas); actualizar(b, b); };
  api.storage.local.get("rules").then((d) => { reglas = d.rules; aplicar(); });
  api.storage.onChanged.addListener((c, area) => {
    if (area === "local" && c.rules) { reglas = c.rules.newValue; aplicar(); }
  });
})();
