// Gestión de tokens de extensión y de equipo (modo servidor): crea, lista y revoca con la sesión de administrador.
(() => {
  const $ = (id) => document.getElementById(id);
  const dark = matchMedia("(prefers-color-scheme: dark)");
  const tema = () => { document.documentElement.dataset.theme = dark.matches ? "dark" : "light"; };
  tema();
  dark.addEventListener("change", tema);

  async function post(cmd, args) {
    const r = await fetch("/api/" + cmd, { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" }, body: JSON.stringify(args || {}) });
    let data = null;
    try { data = await r.json(); } catch {}
    return { status: r.status, data };
  }
  const fecha = (s) => (s ? new Date(s * 1000).toLocaleString() : "nunca");

  async function cargar() {
    const { status, data } = await post("list_ext_tokens");
    if (status === 401) { $("lock").hidden = false; $("lockPw").focus(); return; }
    $("lock").hidden = true;
    $("err").textContent = status === 200 ? "" : (data && data.error) || "Error";
    const tb = $("lista");
    tb.replaceChildren();
    for (const t of data || []) {
      const tr = tb.insertRow();
      tr.insertCell().textContent = t.name;
      tr.insertCell().textContent = t.kind === "device" ? "Equipo" : "Extensión";
      tr.insertCell().textContent = fecha(t.created);
      tr.insertCell().textContent = fecha(t.last_used);
      const b = document.createElement("button");
      b.className = "btn danger";
      b.type = "button";
      b.textContent = "Revocar";
      b.setAttribute("aria-label", "Revocar token " + t.name);
      b.onclick = async () => {
        if (!confirm("¿Revocar el token «" + t.name + "»? Lo que lo use dejará de funcionar.")) return;
        await post("revoke_ext_token", { id: t.id });
        cargar();
      };
      tr.insertCell().append(b);
    }
    $("vacio").hidden = !!(data && data.length);
  }

  $("lockForm").addEventListener("submit", async (e) => {
    e.preventDefault();
    const { status, data } = await post("login", { password: $("lockPw").value });
    $("lockPw").value = "";
    $("lockErr").textContent = status === 200 ? "" : (data && data.error) || "Contraseña incorrecta";
    if (status === 200) cargar();
  });

  $("nuevo").addEventListener("submit", async (e) => {
    e.preventDefault();
    const { status, data } = await post("create_ext_token", { name: $("nombre").value, kind: $("tipo").value });
    if (status !== 200) { $("err").textContent = (data && data.error) || "Error"; return; }
    $("err").textContent = "";
    $("nombre").value = "";
    $("valor").value = data.token;
    $("creado").hidden = false;
    $("valor").select();
    cargar();
  });

  $("copiar").addEventListener("click", async () => {
    $("valor").select();
    try { await navigator.clipboard.writeText($("valor").value); $("copiar").textContent = "Copiado"; } catch { document.execCommand("copy"); }
  });

  cargar();
})();
