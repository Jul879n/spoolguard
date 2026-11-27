// Modo servidor: sin Tauri, window.__TAURI__ se emula con llamadas HTTP a /api/<comando>.
// En la app de escritorio window.__TAURI__ ya existe y este archivo no hace nada.
(() => {
  if (window.__TAURI__) return;
  window.__MODO_SERVIDOR__ = true;
  const $ = (s) => document.querySelector(s);
  let waiting = null, release = null, firstHasPw = true, uploaded = "";

  // Oculta lo exclusivo de escritorio: inicio automático, notificaciones del sistema, extensión de navegador y envío de eventos.
  // La retención del registro se configura en la app; solo CONTROL_LOG_RETENTION_DAYS (si está definida) la sobrescribe.
  const css = document.createElement("style");
  css.textContent = ".card:has(#e-inst),.card:has(#u-chk),#ver,#s-sync,.field:has(#s-auto),.field:has(#s-nb),.field:has(#s-na){display:none}";
  document.head.append(css);

  async function post(cmd, args) {
    const r = await fetch("/api/" + cmd, { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" }, body: JSON.stringify(args || {}) });
    let data = null;
    try { data = await r.json(); } catch {}
    return { status: r.status, data };
  }

  // Sin sesión: muestra la pantalla de acceso existente (#lock) y espera a que se inicie sesión.
  function needLogin() {
    waiting ??= new Promise((res) => { release = res; });
    $("#lock").hidden = false;
    $("#lockPw")?.focus();
    return waiting;
  }

  async function login(password) {
    const { status, data } = await post("login", { password });
    if (status === 200) { release?.(); waiting = release = null; return true; }
    if (status === 429) setTimeout(() => { const e = $("#lockErr"); if (e) e.textContent = data?.error || "Demasiados intentos"; }, 0);
    return false;
  }

  async function invoke(cmd, args) {
    if (cmd === "verify_password") return login(args?.password ?? "");
    if (cmd === "has_password" && firstHasPw) {
      // Primera consulta (arranque): con sesión vigente no se pide la contraseña otra vez.
      firstHasPw = false;
      return (await post("get_paused")).status === 401;
    }
    // Archivos: en el navegador se descargan y se suben.
    if (cmd === "write_text_file") {
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([args.content], { type: /\.json$/.test(args.path) ? "application/json" : "text/plain;charset=utf-8" }));
      a.download = args.path;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
      return null;
    }
    if (cmd === "read_text_file" || cmd === "read_backup_file") return uploaded;
    for (;;) {
      const { status, data } = await post(cmd, args);
      if (status === 401) { await needLogin(); continue; }
      if (status >= 400) throw (data && data.error) || "Error " + status;
      return data;
    }
  }

  const noop = () => Promise.resolve();
  window.__TAURI__ = {
    core: { invoke },
    dialog: {
      save: async (o) => o?.defaultPath || "archivo",
      open: () => new Promise((res) => {
        const i = document.createElement("input");
        i.type = "file"; i.accept = ".json";
        i.onchange = async () => { const f = i.files[0]; if (!f) return res(null); uploaded = await f.text(); res(f.name); };
        i.oncancel = () => res(null);
        i.click();
      }),
      ask: async (msg) => confirm(msg),
    },
    window: { getCurrentWindow: () => ({ setTheme: noop }) },
    event: { listen: () => Promise.resolve(() => {}) },
  };
})();
