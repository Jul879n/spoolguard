//! Búsqueda y descarga verificada de actualizaciones (solo bajo demanda del usuario).
//!
//! Cadena de confianza: el instalador se descarga únicamente de github.com/Jul879n/spoolguard (con
//! redirección solo a los CDN de assets de GitHub), y no se entrega hasta cumplir tres comprobaciones:
//! 1) `SHA256SUMS` lleva firma minisign (Ed25519) válida con la clave pública embebida;
//! 2) el SHA-256 del archivo coincide con su línea de `SHA256SUMS`;
//! 3) el archivo lleva su propia firma minisign válida con esa misma clave y el comentario firmado
//!    nombra exactamente ese archivo (impide reutilizar la firma de otro instalador).
//!
//! Si falla cualquiera, el archivo se borra. Este módulo nunca ejecuta nada.

use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// Clave pública minisign de los releases (ver docs/firma-releases.md).
pub const PUBLIC_KEY: &str = "RWT2sE9dCRki0zPwG/uvsyUeU11Ckbk+/Y5E1NimghhGqIBAep88Sl+K";
pub const API_URL: &str = "https://api.github.com/repos/Jul879n/spoolguard/releases/latest";
pub const RELEASES_URL: &str = "https://github.com/Jul879n/spoolguard/releases/latest";
/// Tope del instalador descargado.
pub const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
/// Tope de los documentos auxiliares (JSON de la API, SHA256SUMS, firmas).
const MAX_SMALL: u64 = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const NOTES_MAX_CHARS: usize = 600;

type Res<T> = Result<T, String>;

// --- Transporte inyectable ---

pub struct Response {
    pub status: u16,
    /// Cabecera `Location` (redirecciones).
    pub location: Option<String>,
    pub len: Option<u64>,
    pub body: Box<dyn Read>,
}

/// Una petición GET SIN seguir redirecciones (las sigue `fetch`, validando cada salto).
pub trait Fetch {
    fn get(&self, url: &str) -> Res<Response>;
}

/// Transporte real: ureq + rustls, tiempo máximo de 8 s por solicitud y agente genérico.
pub struct HttpFetch {
    agent: ureq::Agent,
}

impl HttpFetch {
    pub fn new() -> HttpFetch {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent("SpoolGuard")
            .build()
            .into();
        HttpFetch { agent }
    }

    /// Variante para el instalador: 8 s para conectar y sin tope global (el tamaño lo limita MAX_DOWNLOAD).
    pub fn for_download() -> HttpFetch {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent("SpoolGuard")
            .build()
            .into();
        HttpFetch { agent }
    }
}

impl Default for HttpFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for HttpFetch {
    fn get(&self, url: &str) -> Res<Response> {
        let resp = self.agent.get(url).header("Accept", "application/octet-stream, application/vnd.github+json").call().map_err(|e| format!("sin conexión: {e}"))?;
        let status = resp.status().as_u16();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        let len = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        let body = resp.into_body().into_reader();
        Ok(Response { status, location, len, body: Box::new(body) })
    }
}

// --- Validación de URL ---

/// Divide una URL https en (host en minúsculas, ruta). Rechaza usuario/puerto, caracteres de control y rutas con `.`/`..`.
fn split_https(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    if auth.is_empty() || auth.contains(['@', ':', '\\', '%']) || !auth.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    if !tail.starts_with('/') || tail.bytes().any(|b| b <= 0x20 || b == 0x7f || b == b'\\') {
        return None;
    }
    let path = tail.split(['?', '#']).next().unwrap_or("");
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") || path.split('/').any(|s| s == ".." || s == ".") {
        return None;
    }
    Some((auth.to_ascii_lowercase(), tail.to_string()))
}

/// Origen permitido para la primera petición: la API del repo o un asset de sus releases.
pub fn allowed_origin(url: &str) -> bool {
    match split_https(url) {
        Some((h, p)) => (h == "api.github.com" && p.starts_with("/repos/Jul879n/spoolguard/")) || (h == "github.com" && p.starts_with("/Jul879n/spoolguard/releases/download/")),
        None => false,
    }
}

/// Página de release que la app puede abrir en el navegador (solo github.com/Jul879n/spoolguard/releases/…, sin caracteres especiales).
pub fn is_release_page(url: &str) -> bool {
    split_https(url).is_some_and(|(h, p)| h == "github.com" && p.starts_with("/Jul879n/spoolguard/releases/") && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')))
}

/// Destino permitido de una redirección: solo los CDN de assets de GitHub Releases.
pub fn allowed_redirect(url: &str) -> bool {
    match split_https(url) {
        Some((h, p)) => match h.as_str() {
            "objects.githubusercontent.com" => p.starts_with("/github-releases/") || p.starts_with("/github-production-release-asset"),
            "release-assets.githubusercontent.com" => p.starts_with("/github-production-release-asset/"),
            _ => false,
        },
        None => false,
    }
}

/// GET con redirecciones validadas en cada salto.
fn fetch(f: &dyn Fetch, url: &str) -> Res<Response> {
    if !allowed_origin(url) {
        return Err("Dirección de descarga no permitida".into());
    }
    let mut cur = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let r = f.get(&cur)?;
        match r.status {
            200 => return Ok(r),
            301 | 302 | 303 | 307 | 308 => {
                let next = r.location.ok_or("Redirección sin destino")?;
                if !allowed_redirect(&next) {
                    return Err("Redirección a un destino no permitido".into());
                }
                cur = next;
            }
            404 => return Err("No encontrado en GitHub".into()),
            403 | 429 => return Err("GitHub limitó las solicitudes; inténtalo más tarde".into()),
            c => return Err(format!("GitHub respondió {c}")),
        }
    }
    Err("Demasiadas redirecciones".into())
}

fn read_small(f: &dyn Fetch, url: &str, max: u64) -> Res<Vec<u8>> {
    let r = fetch(f, url)?;
    if r.len.is_some_and(|l| l > max) {
        return Err("Respuesta demasiado grande".into());
    }
    let mut buf = Vec::new();
    r.body.take(max + 1).read_to_end(&mut buf).map_err(|e| format!("lectura interrumpida: {e}"))?;
    if buf.len() as u64 > max {
        return Err("Respuesta demasiado grande".into());
    }
    Ok(buf)
}

// --- Versiones ---

#[derive(Debug, PartialEq, Eq, Clone)]
enum Ident {
    Num(u64),
    Text(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
}

/// Versión semántica con prefijo `v` opcional, prerelease (`-rc.1`) y metadatos de compilación (`+x`, ignorados).
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let s = s.split('+').next()?;
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let mut parts = core.split('.');
    let mut nums = [0u64; 3];
    for n in nums.iter_mut() {
        *n = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let pre = match pre {
        None => vec![],
        Some("") => return None,
        Some(p) => p
            .split('.')
            .map(|i| {
                if i.is_empty() || !i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    None
                } else if i.bytes().all(|b| b.is_ascii_digit()) {
                    i.parse().ok().map(Ident::Num)
                } else {
                    Some(Ident::Text(i.to_string()))
                }
            })
            .collect::<Option<Vec<_>>>()?,
    };
    Some(Version { core: nums, pre })
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match self.core.cmp(&o.core) {
            Equal => {}
            x => return x,
        }
        match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => return Equal,
            (true, false) => return Greater,
            (false, true) => return Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&o.pre) {
            let c = match (a, b) {
                (Ident::Num(x), Ident::Num(y)) => x.cmp(y),
                (Ident::Num(_), Ident::Text(_)) => Less,
                (Ident::Text(_), Ident::Num(_)) => Greater,
                (Ident::Text(x), Ident::Text(y)) => x.cmp(y),
            };
            if c != Equal {
                return c;
            }
        }
        self.pre.len().cmp(&o.pre.len())
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

// --- Plataforma y selección de asset ---

#[derive(Clone, Debug)]
pub struct Platform {
    /// `std::env::consts::OS`: "windows" | "macos" | "linux".
    pub os: String,
    /// `std::env::consts::ARCH`: "x86_64" | "aarch64"…
    pub arch: String,
    /// Palabras de ID e ID_LIKE de /etc/os-release (solo Linux).
    pub distro: Vec<String>,
}

impl Platform {
    pub fn current() -> Platform {
        let distro = if cfg!(target_os = "linux") { distro_ids(&fs::read_to_string("/etc/os-release").unwrap_or_default()) } else { vec![] };
        Platform { os: std::env::consts::OS.into(), arch: std::env::consts::ARCH.into(), distro }
    }
}

/// Palabras de `ID=` e `ID_LIKE=` de un os-release.
pub fn distro_ids(os_release: &str) -> Vec<String> {
    let mut out = vec![];
    for l in os_release.lines() {
        if let Some(v) = l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")) {
            out.extend(v.trim().trim_matches(['"', '\'']).split_whitespace().map(|w| w.to_ascii_lowercase()));
        }
    }
    out
}

/// Sufijo del instalador de esta plataforma (`SpoolGuard-<versión>-<sufijo>`); `None` si no se puede decidir.
pub fn asset_suffix(p: &Platform) -> Option<&'static str> {
    let x64 = p.arch == "x86_64";
    let arm = p.arch == "aarch64";
    match p.os.as_str() {
        // En Windows ARM el MSI x64 corre emulado.
        "windows" if x64 || arm => Some("windows-x64.msi"),
        "macos" if arm => Some("macos-arm64.dmg"),
        "macos" if x64 => Some("macos-x86_64.dmg"),
        "linux" if x64 => {
            let has = |names: &[&str]| p.distro.iter().any(|d| names.contains(&d.as_str()));
            if has(&["debian", "ubuntu"]) {
                Some("linux-amd64.deb")
            } else if has(&["fedora", "rhel", "centos", "suse", "opensuse"]) {
                Some("linux-x86_64.rpm")
            } else {
                None
            }
        }
        _ => None,
    }
}

// --- Consulta del release ---

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}
#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    pub actual: String,
    pub ultima: String,
    pub hay_nueva: bool,
    pub notas: String,
    /// Instalador de esta plataforma (None = abrir la página del release).
    pub url_asset: Option<String>,
    pub url_release: String,
    pub tamano: Option<u64>,
    /// `<asset>.minisig`.
    pub firma_url: Option<String>,
    pub sha256_url: Option<String>,
    /// `SHA256SUMS.minisig`.
    pub sha256_firma_url: Option<String>,
}

fn trim_notes(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= NOTES_MAX_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(NOTES_MAX_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// Consulta el último release y lo compara con `current`.
pub fn check_update(f: &dyn Fetch, current: &str, p: &Platform) -> Res<UpdateInfo> {
    let raw = read_small(f, API_URL, MAX_SMALL)?;
    let rel: GhRelease = serde_json::from_slice(&raw).map_err(|e| format!("respuesta de GitHub ilegible: {e}"))?;
    if rel.draft || rel.prerelease {
        return Err("El último release no es una versión estable".into());
    }
    let cur = parse_version(current).ok_or("versión actual ilegible")?;
    let new = parse_version(&rel.tag_name).ok_or("versión del release ilegible")?;
    let hay = new > cur;
    let tag = rel.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
    let url_release = if split_https(&rel.html_url).is_some_and(|(h, path)| h == "github.com" && path.starts_with("/Jul879n/spoolguard/releases/")) { rel.html_url.clone() } else { RELEASES_URL.to_string() };
    let find = |name: &str| rel.assets.iter().find(|a| a.name == name && allowed_origin(&a.browser_download_url));
    let asset = if hay { asset_suffix(p).and_then(|s| find(&format!("SpoolGuard-{tag}-{s}"))) } else { None };
    let sums = if asset.is_some() { find("SHA256SUMS") } else { None };
    Ok(UpdateInfo {
        actual: current.trim().trim_start_matches(['v', 'V']).to_string(),
        ultima: tag,
        hay_nueva: hay,
        notas: trim_notes(rel.body.as_deref().unwrap_or("")),
        url_asset: asset.map(|a| a.browser_download_url.clone()),
        url_release,
        tamano: asset.map(|a| a.size),
        firma_url: asset.and_then(|a| find(&format!("{}.minisig", a.name))).map(|a| a.browser_download_url.clone()),
        sha256_url: sums.map(|a| a.browser_download_url.clone()),
        sha256_firma_url: sums.and_then(|_| find("SHA256SUMS.minisig")).map(|a| a.browser_download_url.clone()),
    })
}

// --- Descarga verificada ---

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Verified {
    pub ruta: PathBuf,
    pub nombre: String,
    pub sha256: String,
}

fn asset_name(url: &str) -> Res<String> {
    let (_, path) = split_https(url).ok_or("Dirección de descarga no permitida")?;
    let name = path.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
    let ok = name.starts_with("SpoolGuard-") && name.len() < 100 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if ok { Ok(name) } else { Err("Nombre de instalador no válido".into()) }
}

/// Verifica la firma minisign de `data`; el comentario firmado debe nombrar `name`.
fn verify_signed(pk: &PublicKey, data: &[u8], sig_text: &str, name: &str) -> Res<()> {
    let sig = Signature::decode(sig_text).map_err(|e| format!("firma ilegible: {e}"))?;
    check_comment(&sig, name)?;
    pk.verify(data, &sig, false).map_err(|e| format!("firma inválida: {e}"))
}

fn check_comment(sig: &Signature, name: &str) -> Res<()> {
    let tag = format!("file:{name}");
    if sig.trusted_comment().split('\t').any(|f| f == tag) { Ok(()) } else { Err("la firma no corresponde a este archivo".into()) }
}

/// Verifica por bloques la firma de un archivo grande.
pub fn verify_file(pk: &PublicKey, path: &Path, sig_text: &str, name: &str) -> Res<()> {
    let sig = Signature::decode(sig_text).map_err(|e| format!("firma ilegible: {e}"))?;
    check_comment(&sig, name)?;
    let mut v = pk.verify_stream(&sig).map_err(|e| format!("firma inválida: {e}"))?;
    let mut fh = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = fh.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        v.update(&buf[..n]);
    }
    v.finalize().map_err(|e| format!("firma inválida: {e}"))
}

/// SHA-256 esperado para `name` en un SHA256SUMS (`<hex>  <nombre>` o `<hex> *<nombre>`).
fn expected_sha(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, n) = l.split_once(char::is_whitespace)?;
        (n.trim().trim_start_matches('*') == name && h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
    })
}

/// Descarga `info.url_asset` en `dir/updates/` y la deja verificada. Ante cualquier fallo borra el archivo.
/// `progress(hecho, total)` se llama durante la descarga.
pub fn download_update(f: &dyn Fetch, info: &UpdateInfo, dir: &Path, public_key: &str, max: u64, progress: &mut dyn FnMut(u64, u64)) -> Res<Verified> {
    if !info.hay_nueva {
        return Err("No hay una versión nueva".into());
    }
    let (Some(url), Some(sig_url), Some(sums_url), Some(sums_sig_url)) = (&info.url_asset, &info.firma_url, &info.sha256_url, &info.sha256_firma_url) else {
        return Err("El release no incluye firma ni SHA256SUMS: no se descarga".into());
    };
    let name = asset_name(url)?;
    let pk = PublicKey::from_base64(public_key).map_err(|e| format!("clave pública inválida: {e}"))?;

    // 1) Documentos de verificación (pequeños) antes de bajar el instalador.
    let sums_raw = read_small(f, sums_url, MAX_SMALL)?;
    let sums_sig = String::from_utf8(read_small(f, sums_sig_url, 4096)?).map_err(|_| "firma de SHA256SUMS ilegible")?;
    verify_signed(&pk, &sums_raw, &sums_sig, "SHA256SUMS").map_err(|e| format!("SHA256SUMS: {e}"))?;
    let sums = String::from_utf8(sums_raw).map_err(|_| "SHA256SUMS ilegible")?;
    let want = expected_sha(&sums, &name).ok_or("SHA256SUMS no incluye este instalador")?;
    let sig_text = String::from_utf8(read_small(f, sig_url, 4096)?).map_err(|_| "firma del instalador ilegible")?;

    // 2) Descarga acotada.
    let out_dir = dir.join("updates");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let final_path = out_dir.join(&name);
    let part = out_dir.join(format!("{name}.part"));
    let _ = fs::remove_file(&final_path);
    let result = (|| -> Res<String> {
        let r = fetch(f, url)?;
        let total = r.len.or(info.tamano).unwrap_or(0);
        if total > max {
            return Err("El instalador supera el tamaño máximo permitido".into());
        }
        let mut body = r.body;
        let mut out = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut h = Sha256::new();
        let mut done = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = body.read(&mut buf).map_err(|e| format!("descarga interrumpida: {e}"))?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > max {
                return Err("El instalador supera el tamaño máximo permitido".into());
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            progress(done, total.max(done));
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        // 3) SHA-256 y firma del instalador.
        if got != want {
            return Err("El SHA-256 no coincide con SHA256SUMS".into());
        }
        verify_file(&pk, &part, &sig_text, &name)?;
        Ok(got)
    })();
    match result {
        Ok(sha256) => {
            fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
            Ok(Verified { ruta: final_path, nombre: name, sha256 })
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;

    const DEB: &[u8] = include_bytes!("../tests/fixtures/update/SpoolGuard-9.9.9-linux-amd64.deb");
    const DEB_SIG: &str = include_str!("../tests/fixtures/update/SpoolGuard-9.9.9-linux-amd64.deb.minisig");
    const SUMS: &[u8] = include_bytes!("../tests/fixtures/update/SHA256SUMS");
    const SUMS_SIG: &str = include_str!("../tests/fixtures/update/SHA256SUMS.minisig");
    const BASE: &str = "https://github.com/Jul879n/spoolguard/releases/download/v9.9.9/";
    const CDN: &str = "https://release-assets.githubusercontent.com/github-production-release-asset/1/abc?sig=x";

    enum Route {
        Body(Vec<u8>, Option<u64>),
        Redirect(String),
    }
    #[derive(Default)]
    struct Mock {
        routes: HashMap<String, Route>,
        hits: std::cell::RefCell<Vec<String>>,
    }
    impl Fetch for Mock {
        fn get(&self, url: &str) -> Res<Response> {
            self.hits.borrow_mut().push(url.to_string());
            match self.routes.get(url) {
                Some(Route::Body(b, l)) => Ok(Response { status: 200, location: None, len: *l, body: Box::new(Cursor::new(b.clone())) }),
                Some(Route::Redirect(to)) => Ok(Response { status: 302, location: Some(to.clone()), len: None, body: Box::new(Cursor::new(vec![])) }),
                None => Ok(Response { status: 404, location: None, len: None, body: Box::new(Cursor::new(vec![])) }),
            }
        }
    }

    fn plat(os: &str, arch: &str, distro: &[&str]) -> Platform {
        Platform { os: os.into(), arch: arch.into(), distro: distro.iter().map(|s| s.to_string()).collect() }
    }
    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }
    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/upd/test").join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn release_json(tag: &str, names: &[&str]) -> Vec<u8> {
        let assets: Vec<String> = names.iter().map(|n| format!(r#"{{"name":"{n}","size":74,"browser_download_url":"https://github.com/Jul879n/spoolguard/releases/download/{tag}/{n}"}}"#)).collect();
        format!(r#"{{"tag_name":"{tag}","html_url":"https://github.com/Jul879n/spoolguard/releases/tag/{tag}","body":"notas","prerelease":false,"draft":false,"assets":[{}]}}"#, assets.join(",")).into_bytes()
    }

    #[test]
    fn compara_versiones() {
        assert!(v("0.2.0") > v("0.1.0"));
        assert!(v("v1.0.0") > v("0.9.9"));
        assert!(v("0.10.0") > v("0.9.0"));
        assert_eq!(v("v1.2.3"), v("1.2.3+build5"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.9"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.11"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        for malo in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.3-", "1.2.x"] {
            assert!(parse_version(malo).is_none(), "{malo}");
        }
    }

    #[test]
    fn elige_asset_por_plataforma() {
        assert_eq!(asset_suffix(&plat("windows", "x86_64", &[])), Some("windows-x64.msi"));
        assert_eq!(asset_suffix(&plat("macos", "aarch64", &[])), Some("macos-arm64.dmg"));
        assert_eq!(asset_suffix(&plat("macos", "x86_64", &[])), Some("macos-x86_64.dmg"));
        assert_eq!(asset_suffix(&plat("linux", "x86_64", &["ubuntu", "debian"])), Some("linux-amd64.deb"));
        assert_eq!(asset_suffix(&plat("linux", "x86_64", &["fedora"])), Some("linux-x86_64.rpm"));
        assert_eq!(asset_suffix(&plat("linux", "x86_64", &["opensuse-leap", "suse", "opensuse"])), Some("linux-x86_64.rpm"));
        assert_eq!(asset_suffix(&plat("linux", "x86_64", &["arch"])), None);
        assert_eq!(asset_suffix(&plat("linux", "aarch64", &["debian"])), None);
        assert_eq!(asset_suffix(&plat("freebsd", "x86_64", &[])), None);
        assert_eq!(distro_ids("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=\"debian foo\"\n"), ["ubuntu", "debian", "foo"]);
    }

    #[test]
    fn check_detecta_version_y_asset() {
        let names = ["SpoolGuard-0.2.0-macos-arm64.dmg", "SpoolGuard-0.2.0-macos-x86_64.dmg", "SpoolGuard-0.2.0-macos-arm64.dmg.minisig", "SHA256SUMS", "SHA256SUMS.minisig"];
        let mut m = Mock::default();
        m.routes.insert(API_URL.into(), Route::Body(release_json("v0.2.0", &names), None));
        let i = check_update(&m, "0.1.0", &plat("macos", "aarch64", &[])).unwrap();
        assert!(i.hay_nueva);
        assert_eq!((i.actual.as_str(), i.ultima.as_str()), ("0.1.0", "0.2.0"));
        assert!(i.url_asset.as_deref().unwrap().ends_with("/v0.2.0/SpoolGuard-0.2.0-macos-arm64.dmg"));
        assert!(i.firma_url.is_some() && i.sha256_url.is_some() && i.sha256_firma_url.is_some());
        // Al día: sin asset.
        let i = check_update(&m, "v0.2.0", &plat("macos", "aarch64", &[])).unwrap();
        assert!(!i.hay_nueva && i.url_asset.is_none());
        // Plataforma sin decisión: solo la página del release.
        let i = check_update(&m, "0.1.0", &plat("linux", "x86_64", &["arch"])).unwrap();
        assert!(i.hay_nueva && i.url_asset.is_none());
        assert_eq!(i.url_release, "https://github.com/Jul879n/spoolguard/releases/tag/v0.2.0");
    }

    #[test]
    fn check_recorta_notas_y_rechaza_prerelease() {
        let mut m = Mock::default();
        let largo = "x".repeat(5000);
        let j = format!(r#"{{"tag_name":"v1.0.0","html_url":"https://evil.example/x","body":"{largo}","assets":[]}}"#);
        m.routes.insert(API_URL.into(), Route::Body(j.into_bytes(), None));
        let i = check_update(&m, "0.1.0", &plat("windows", "x86_64", &[])).unwrap();
        assert!(i.notas.chars().count() <= NOTES_MAX_CHARS + 1);
        assert_eq!(i.url_release, RELEASES_URL);
        m.routes.insert(API_URL.into(), Route::Body(br#"{"tag_name":"v1.0.0","html_url":"","prerelease":true}"#.to_vec(), None));
        assert!(check_update(&m, "0.1.0", &plat("windows", "x86_64", &[])).is_err());
    }

    #[test]
    fn rechaza_hosts_y_rutas() {
        assert!(allowed_origin("https://github.com/Jul879n/spoolguard/releases/download/v1/a.msi"));
        for malo in [
            "http://github.com/Jul879n/spoolguard/releases/download/v1/a.msi",
            "https://github.com.evil.com/Jul879n/spoolguard/releases/download/v1/a.msi",
            "https://evil.com/Jul879n/spoolguard/releases/download/v1/a.msi",
            "https://github.com@evil.com/Jul879n/spoolguard/releases/download/v1/a.msi",
            "https://github.com:444/Jul879n/spoolguard/releases/download/v1/a.msi",
            "https://github.com/otro/repo/releases/download/v1/a.msi",
            "https://github.com/Jul879n/spoolguard/archive/main.zip",
            "https://github.com/Jul879n/spoolguard/releases/download/../../../evil/a.msi",
            "https://github.com/Jul879n/spoolguard/releases/download/%2e%2e/a.msi",
            "https://github.com\\@evil.com/Jul879n/spoolguard/releases/download/v1/a.msi",
        ] {
            assert!(!allowed_origin(malo), "{malo}");
        }
        assert!(allowed_redirect(CDN));
        assert!(allowed_redirect("https://objects.githubusercontent.com/github-production-release-asset-2e65be/1/x"));
        for malo in ["https://evil.com/github-releases/x", "https://objects.githubusercontent.com.evil.com/github-releases/x", "https://objects.githubusercontent.com/otra-cosa/x", "http://objects.githubusercontent.com/github-releases/x", "https://github.com/Jul879n/spoolguard/releases/download/v1/a"] {
            assert!(!allowed_redirect(malo), "{malo}");
        }
    }

    #[test]
    fn paginas_de_release_abribles() {
        assert!(is_release_page(RELEASES_URL));
        assert!(is_release_page("https://github.com/Jul879n/spoolguard/releases/tag/v0.2.0"));
        for malo in ["https://github.com/Jul879n/spoolguard/issues", "https://evil.com/Jul879n/spoolguard/releases/x", "https://github.com/Jul879n/spoolguard/releases/x&calc", "http://github.com/Jul879n/spoolguard/releases/x", "https://github.com/Jul879n/spoolguard/releases/a b"] {
            assert!(!is_release_page(malo), "{malo}");
        }
    }

    #[test]
    fn redireccion_a_host_no_permitido_falla() {
        let url = format!("{BASE}x");
        let mut m = Mock::default();
        m.routes.insert(url.clone(), Route::Redirect("https://evil.example/x".into()));
        assert!(fetch(&m, &url).err().unwrap().contains("no permitido"));
        m.routes.insert(url.clone(), Route::Redirect(CDN.into()));
        m.routes.insert(CDN.into(), Route::Body(b"ok".to_vec(), Some(2)));
        assert!(fetch(&m, &url).is_ok());
        assert!(fetch(&m, "https://evil.example/x").is_err());
        assert_eq!(m.hits.borrow().len(), 3);
    }

    fn info() -> UpdateInfo {
        let n = "SpoolGuard-9.9.9-linux-amd64.deb";
        UpdateInfo {
            actual: "0.1.0".into(),
            ultima: "9.9.9".into(),
            hay_nueva: true,
            notas: String::new(),
            url_asset: Some(format!("{BASE}{n}")),
            url_release: RELEASES_URL.into(),
            tamano: Some(DEB.len() as u64),
            firma_url: Some(format!("{BASE}{n}.minisig")),
            sha256_url: Some(format!("{BASE}SHA256SUMS")),
            sha256_firma_url: Some(format!("{BASE}SHA256SUMS.minisig")),
        }
    }
    fn mock(deb: &[u8], deb_sig: &str, sums: &[u8], sums_sig: &str) -> Mock {
        let i = info();
        let mut m = Mock::default();
        m.routes.insert(i.url_asset.clone().unwrap(), Route::Redirect(CDN.into()));
        m.routes.insert(CDN.into(), Route::Body(deb.to_vec(), Some(deb.len() as u64)));
        m.routes.insert(i.firma_url.unwrap(), Route::Body(deb_sig.as_bytes().to_vec(), None));
        m.routes.insert(i.sha256_url.unwrap(), Route::Body(sums.to_vec(), None));
        m.routes.insert(i.sha256_firma_url.unwrap(), Route::Body(sums_sig.as_bytes().to_vec(), None));
        m
    }
    fn run(m: &Mock, d: &Path, max: u64) -> Res<Verified> {
        download_update(m, &info(), d, PUBLIC_KEY, max, &mut |_, _| {})
    }
    fn sin_restos(d: &Path) -> bool {
        fs::read_dir(d.join("updates")).map(|r| r.count() == 0).unwrap_or(true)
    }

    #[test]
    fn descarga_firmada_ok() {
        let d = tmp("ok");
        let mut calls = 0;
        let r = download_update(&mock(DEB, DEB_SIG, SUMS, SUMS_SIG), &info(), &d, PUBLIC_KEY, MAX_DOWNLOAD, &mut |h, t| {
            calls += 1;
            assert!(h <= t);
        })
        .unwrap();
        assert_eq!(fs::read(&r.ruta).unwrap(), DEB);
        assert!(r.ruta.ends_with("updates/SpoolGuard-9.9.9-linux-amd64.deb") && calls > 0);
        assert!(!d.join("updates/SpoolGuard-9.9.9-linux-amd64.deb.part").exists());
    }

    #[test]
    fn sha256_distinto_se_rechaza_y_borra() {
        let d = tmp("sha");
        let mut malo = DEB.to_vec();
        malo[0] ^= 1;
        let e = run(&mock(&malo, DEB_SIG, SUMS, SUMS_SIG), &d, MAX_DOWNLOAD).unwrap_err();
        assert!(e.contains("SHA-256"), "{e}");
        assert!(sin_restos(&d));
    }

    #[test]
    fn firma_invalida_se_rechaza_y_borra() {
        let d = tmp("firma");
        // SHA-256 y SHA256SUMS correctos, pero la firma del instalador es la de otro archivo.
        let e = run(&mock(DEB, SUMS_SIG, SUMS, SUMS_SIG), &d, MAX_DOWNLOAD).unwrap_err();
        assert!(e.contains("firma"), "{e}");
        assert!(sin_restos(&d));
        // Firma con el comentario alterado para aparentar otro archivo.
        let falsa = DEB_SIG.replace("file:SpoolGuard-9.9.9-linux-amd64.deb", "file:SpoolGuard-9.9.9-linux-x86_64.rpm");
        assert!(run(&mock(DEB, &falsa, SUMS, SUMS_SIG), &d, MAX_DOWNLOAD).is_err());
        assert!(sin_restos(&d));
        // Firma ilegible.
        assert!(run(&mock(DEB, "basura", SUMS, SUMS_SIG), &d, MAX_DOWNLOAD).is_err());
        assert!(sin_restos(&d));
    }

    #[test]
    fn sha256sums_alterado_o_sin_firma_se_rechaza() {
        let d = tmp("sums");
        let mut malo = SUMS.to_vec();
        malo[0] = b'0';
        let e = run(&mock(DEB, DEB_SIG, &malo, SUMS_SIG), &d, MAX_DOWNLOAD).unwrap_err();
        assert!(e.contains("SHA256SUMS"), "{e}");
        assert!(sin_restos(&d));
        let mut i = info();
        i.firma_url = None;
        assert!(download_update(&mock(DEB, DEB_SIG, SUMS, SUMS_SIG), &i, &d, PUBLIC_KEY, MAX_DOWNLOAD, &mut |_, _| {}).is_err());
    }

    #[test]
    fn clave_distinta_se_rechaza() {
        let d = tmp("clave");
        let otra = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        assert!(download_update(&mock(DEB, DEB_SIG, SUMS, SUMS_SIG), &info(), &d, otra, MAX_DOWNLOAD, &mut |_, _| {}).is_err());
        assert!(sin_restos(&d));
    }

    #[test]
    fn tope_de_tamano() {
        let d = tmp("tope");
        // Declarado en Content-Length.
        let e = run(&mock(DEB, DEB_SIG, SUMS, SUMS_SIG), &d, 10).unwrap_err();
        assert!(e.contains("tamaño"), "{e}");
        assert!(sin_restos(&d));
        // Sin Content-Length y con más bytes de los permitidos.
        let mut m = mock(DEB, DEB_SIG, SUMS, SUMS_SIG);
        m.routes.insert(CDN.into(), Route::Body(DEB.to_vec(), None));
        let mut i = info();
        i.tamano = None;
        let e = download_update(&m, &i, &d, PUBLIC_KEY, 10, &mut |_, _| {}).unwrap_err();
        assert!(e.contains("tamaño"), "{e}");
        assert!(sin_restos(&d));
    }

    #[test]
    fn rechaza_urls_de_info_manipuladas() {
        let d = tmp("url");
        let m = mock(DEB, DEB_SIG, SUMS, SUMS_SIG);
        let mut i = info();
        i.url_asset = Some("https://evil.example/SpoolGuard-9.9.9-linux-amd64.deb".into());
        assert!(download_update(&m, &i, &d, PUBLIC_KEY, MAX_DOWNLOAD, &mut |_, _| {}).is_err());
        let mut i = info();
        i.hay_nueva = false;
        assert!(download_update(&m, &i, &d, PUBLIC_KEY, MAX_DOWNLOAD, &mut |_, _| {}).is_err());
        assert!(m.hits.borrow().iter().all(|u| !u.contains("evil")));
    }

    #[test]
    fn firma_real_acepta_y_rechaza_alterado() {
        let pk = PublicKey::from_base64(PUBLIC_KEY).unwrap();
        let n = "SpoolGuard-9.9.9-linux-amd64.deb";
        verify_signed(&pk, DEB, DEB_SIG, n).unwrap();
        let mut malo = DEB.to_vec();
        malo.push(b'!');
        assert!(verify_signed(&pk, &malo, DEB_SIG, n).is_err());
        assert!(verify_signed(&pk, DEB, DEB_SIG, "otro.deb").is_err());
    }

    /// Prueba real contra GitHub (red): `SG_LIVE_DIR=<dir con SHA256SUMS y .minisig> cargo test -p control-core live -- --ignored --nocapture`.
    /// Versión simulada 0.1.0 por SG_FAKE_VERSION (por defecto 0.1.0).
    #[test]
    #[ignore]
    fn live_github() {
        let ver = std::env::var("SG_FAKE_VERSION").unwrap_or("0.1.0".into());
        let p = Platform::current();
        let i = check_update(&HttpFetch::new(), &ver, &p).unwrap();
        println!("plataforma={p:?}\ninfo={i:#?}");
        assert!(i.hay_nueva && i.ultima == "0.2.0");
        assert!(i.url_asset.is_some(), "sin asset para esta plataforma");
        assert!(check_update(&HttpFetch::new(), &i.ultima, &p).unwrap().url_asset.is_none());
        // Compatibilidad: firma hecha por minisign (C) 0.12, la herramienta que usa el CI, verificada con el código de la app.
        if let Ok(c) = std::env::var("SG_CROSS_DIR") {
            let c = Path::new(&c);
            let pk = PublicKey::from_base64(PUBLIC_KEY).unwrap();
            verify_file(&pk, &c.join("prueba.bin"), &fs::read_to_string(c.join("prueba.bin.minisig")).unwrap(), "prueba.bin").unwrap();
            fs::write(c.join("alterado.bin"), b"hola!\n").unwrap();
            assert!(verify_file(&pk, &c.join("alterado.bin"), &fs::read_to_string(c.join("prueba.bin.minisig")).unwrap(), "prueba.bin").is_err());
            println!("firma de minisign C verificada por la app; alterado rechazado");
        }
        // Descarga real del zip de la extensión (34 KB, sin firma): SHA256SUMS firmado (local) con su hash, firma del zip ausente.
        let Ok(live) = std::env::var("SG_LIVE_DIR") else { return };
        let base = "https://github.com/Jul879n/spoolguard/releases/download/v0.2.0/";
        let name = "SpoolGuard-extension-0.2.0.zip";
        struct Mix(String, HttpFetch);
        impl Fetch for Mix {
            fn get(&self, url: &str) -> Res<Response> {
                if let Some(n) = url.strip_prefix("https://github.com/Jul879n/spoolguard/releases/download/v0.2.0/local-") {
                    let b = fs::read(Path::new(&self.0).join(n)).map_err(|e| e.to_string())?;
                    return Ok(Response { status: 200, location: None, len: Some(b.len() as u64), body: Box::new(Cursor::new(b)) });
                }
                self.1.get(url)
            }
        }
        let info = UpdateInfo { url_asset: Some(format!("{base}{name}")), tamano: Some(34510), firma_url: Some(format!("{base}local-SHA256SUMS.minisig")), sha256_url: Some(format!("{base}local-SHA256SUMS")), sha256_firma_url: Some(format!("{base}local-SHA256SUMS.minisig")), ..i };
        let d = tmp("live");
        let mut seen = 0;
        let r = download_update(&Mix(live, HttpFetch::for_download()), &info, &d, PUBLIC_KEY, MAX_DOWNLOAD, &mut |h, _| seen = h);
        println!("descargados={seen} resultado={r:?}");
        assert_eq!(seen, 34510, "debe haberse descargado el zip completo (sha256 ok) antes de fallar la firma");
        assert!(r.is_err());
        assert!(sin_restos(&d), "el archivo debe haberse borrado");
    }
}
