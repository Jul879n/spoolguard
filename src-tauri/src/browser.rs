//! Registro automático del host de Native Messaging y de los archivos de la extensión.

use serde::Serialize;
use std::{fs, path::{Path, PathBuf}, process::Command};

const NAME: &str = "cl.cohasa.controlimpresion";
/// ID fijo: sale de la `key` incluida en extension/manifest.json.
pub const EXT_ID: &str = "afjahbecgkkpbenpmgelpiceakiolikp";
const FIREFOX_ID: &str = "controlimpresion@cohasa.cl";

#[derive(Serialize)]
pub struct ExtensionStatus {
    /// Navegadores donde quedó registrado el host.
    pub registered: Vec<String>,
    pub last_seen: Option<u64>,
    pub folder: String,
}

fn manifest(path: &Path, firefox: bool) -> String {
    let who = if firefox {
        format!("\"allowed_extensions\": [\"{FIREFOX_ID}\"]")
    } else {
        format!("\"allowed_origins\": [\"chrome-extension://{EXT_ID}/\"]")
    };
    format!(
        "{{\n  \"name\": \"{NAME}\",\n  \"description\": \"SpoolGuard: host de reglas\",\n  \"path\": {},\n  \"type\": \"stdio\",\n  {who}\n}}\n",
        serde_json::to_string(&path.display().to_string()).unwrap()
    )
}

/// (nombre, carpeta de la app del navegador, carpeta de manifiestos, es Firefox)
#[cfg(not(windows))]
fn browsers() -> Vec<(&'static str, PathBuf, PathBuf, bool)> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        let a = home.join("Library/Application Support");
        vec![
            ("Chrome", a.join("Google/Chrome"), a.join("Google/Chrome/NativeMessagingHosts"), false),
            ("Chromium", a.join("Chromium"), a.join("Chromium/NativeMessagingHosts"), false),
            ("Edge", a.join("Microsoft Edge"), a.join("Microsoft Edge/NativeMessagingHosts"), false),
            ("Brave", a.join("BraveSoftware/Brave-Browser"), a.join("BraveSoftware/Brave-Browser/NativeMessagingHosts"), false),
            ("Firefox", a.join("Firefox"), a.join("Mozilla/NativeMessagingHosts"), true),
        ]
    } else {
        let c = home.join(".config");
        vec![
            ("Chrome", c.join("google-chrome"), c.join("google-chrome/NativeMessagingHosts"), false),
            ("Chromium", c.join("chromium"), c.join("chromium/NativeMessagingHosts"), false),
            ("Edge", c.join("microsoft-edge"), c.join("microsoft-edge/NativeMessagingHosts"), false),
            ("Brave", c.join("BraveSoftware/Brave-Browser"), c.join("BraveSoftware/Brave-Browser/NativeMessagingHosts"), false),
            ("Firefox", home.join(".mozilla/firefox"), home.join(".mozilla/native-messaging-hosts"), true),
        ]
    }
}

/// Copia la extensión, crea el envoltorio del host y registra el manifiesto en cada navegador instalado.
pub fn ensure(data_dir: &Path, resource_dir: Option<PathBuf>) -> Result<ExtensionStatus, String> {
    let src = resource_dir.map(|r| r.join("extension")).filter(|p| p.exists());
    if let Some(src) = src {
        control_core::extension::install_from(data_dir, &src).map_err(|e| e.to_string())?;
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut registered = vec![];

    #[cfg(not(windows))]
    {
        let wrapper = data_dir.join("native_host.sh");
        fs::write(&wrapper, format!("#!/bin/sh\nexec \"{}\" --native-host \"$@\"\n", exe.display())).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755));
        }
        for (n, app_dir, mdir, ff) in browsers() {
            if !app_dir.exists() {
                continue;
            }
            fs::create_dir_all(&mdir).map_err(|e| e.to_string())?;
            fs::write(mdir.join(format!("{NAME}.json")), manifest(&wrapper, ff)).map_err(|e| e.to_string())?;
            registered.push(n.to_string());
        }
    }
    #[cfg(windows)]
    {
        let wrapper = data_dir.join("native_host.bat");
        fs::write(&wrapper, format!("@echo off\r\n\"{}\" --native-host %*\r\n", exe.display())).map_err(|e| e.to_string())?;
        for (n, key, ff) in [
            ("Chrome", r"HKCU\Software\Google\Chrome\NativeMessagingHosts", false),
            ("Edge", r"HKCU\Software\Microsoft\Edge\NativeMessagingHosts", false),
            ("Brave", r"HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts", false),
            ("Firefox", r"HKCU\Software\Mozilla\NativeMessagingHosts", true),
        ] {
            let file = data_dir.join(format!("{NAME}.{}.json", n.to_lowercase()));
            fs::write(&file, manifest(&wrapper, ff)).map_err(|e| e.to_string())?;
            let ok = crate::printers::no_window(Command::new("reg")
                .args(["add", &format!("{key}\\{NAME}"), "/ve", "/t", "REG_SZ", "/d"])
                .arg(&file)
                .arg("/f"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                registered.push(n.to_string());
            }
        }
    }
    Ok(status(data_dir, registered))
}

fn status(data_dir: &Path, registered: Vec<String>) -> ExtensionStatus {
    let last_seen = fs::read_to_string(data_dir.join("host_seen")).ok().and_then(|s| s.trim().parse().ok());
    ExtensionStatus { registered, last_seen, folder: data_dir.join("extension").display().to_string() }
}

/// Abre la carpeta de la extensión o la página de extensiones del navegador.
pub fn open(data_dir: &Path, what: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
    let mut c = Command::new(opener);
    match what {
        "folder" => c.arg(data_dir.join("extension")),
        "chrome" if cfg!(target_os = "macos") => c.args(["-a", "Google Chrome", "chrome://extensions"]),
        "chrome" => c.arg("chrome://extensions"),
        _ => return Err("destino desconocido".into()),
    };
    c.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Profile {
    pub dir: String,
    pub name: String,
    pub email: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BrowserInfo {
    pub id: String,
    pub name: String,
    pub profiles: Vec<Profile>,
}

struct Spec {
    id: &'static str,
    name: &'static str,
    mac_app: &'static str,
    mac_data: &'static str,
    linux_bins: &'static [&'static str],
    linux_data: &'static str,
    win_exe: &'static [&'static str],
    win_data: &'static str,
    scheme: &'static str,
}

const SPECS: &[Spec] = &[
    Spec { id: "chrome", name: "Google Chrome", mac_app: "Google Chrome", mac_data: "Google/Chrome",
        linux_bins: &["google-chrome", "google-chrome-stable"], linux_data: "google-chrome",
        win_exe: &[r"Google\Chrome\Application\chrome.exe"], win_data: r"Google\Chrome\User Data", scheme: "chrome" },
    Spec { id: "edge", name: "Microsoft Edge", mac_app: "Microsoft Edge", mac_data: "Microsoft Edge",
        linux_bins: &["microsoft-edge", "microsoft-edge-stable"], linux_data: "microsoft-edge",
        win_exe: &[r"Microsoft\Edge\Application\msedge.exe"], win_data: r"Microsoft\Edge\User Data", scheme: "edge" },
    Spec { id: "chromium", name: "Chromium", mac_app: "Chromium", mac_data: "Chromium",
        linux_bins: &["chromium", "chromium-browser"], linux_data: "chromium",
        win_exe: &[r"Chromium\Application\chrome.exe"], win_data: r"Chromium\User Data", scheme: "chrome" },
    Spec { id: "brave", name: "Brave", mac_app: "Brave Browser", mac_data: "BraveSoftware/Brave-Browser",
        linux_bins: &["brave-browser", "brave"], linux_data: "BraveSoftware/Brave-Browser",
        win_exe: &[r"BraveSoftware\Brave-Browser\Application\brave.exe"], win_data: r"BraveSoftware\Brave-Browser\User Data", scheme: "brave" },
    Spec { id: "firefox", name: "Firefox", mac_app: "Firefox", mac_data: "Firefox",
        linux_bins: &["firefox"], linux_data: ".mozilla/firefox",
        win_exe: &[r"Mozilla Firefox\firefox.exe"], win_data: r"Mozilla\Firefox", scheme: "" },
];

/// Perfiles de Chromium desde el JSON de `Local State` (`profile.info_cache`).
pub fn parse_local_state(json: &str) -> Vec<Profile> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut out: Vec<Profile> = v["profile"]["info_cache"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(dir, p)| Profile {
                    dir: dir.clone(),
                    name: p["name"].as_str().unwrap_or(dir).to_string(),
                    email: p["user_name"].as_str().filter(|e| !e.is_empty()).map(String::from),
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Perfiles de Firefox desde `profiles.ini` (secciones `[ProfileN]`); dir = name = campo `Name`.
pub fn parse_profiles_ini(ini: &str) -> Vec<Profile> {
    let mut out = vec![];
    let mut in_profile = false;
    for line in ini.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix('[') {
            in_profile = line.len() > 8 && rest.to_lowercase().starts_with("profile");
        } else if in_profile {
            if let Some(n) = line.strip_prefix("Name=") {
                if !n.is_empty() {
                    out.push(Profile { dir: n.to_string(), name: n.to_string(), email: None });
                }
            }
        }
    }
    out
}

fn env_path(k: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(k).unwrap_or_default())
}

fn data_dir_of(s: &Spec) -> PathBuf {
    if cfg!(windows) {
        let base = if s.id == "firefox" { env_path("APPDATA") } else { env_path("LOCALAPPDATA") };
        base.join(s.win_data)
    } else if cfg!(target_os = "macos") {
        env_path("HOME").join("Library/Application Support").join(s.mac_data)
    } else if s.id == "firefox" {
        env_path("HOME").join(s.linux_data)
    } else {
        env_path("HOME").join(".config").join(s.linux_data)
    }
}

/// Ejecutable/app instalado: en macOS la ruta del .app, en Windows el .exe, en Linux el binario.
fn installed(s: &Spec) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let app = format!("{}.app", s.mac_app);
        [PathBuf::from("/Applications"), env_path("HOME").join("Applications")]
            .into_iter()
            .map(|d| d.join(&app))
            .find(|p| p.exists())
    } else if cfg!(windows) {
        let roots = [env_path("ProgramFiles"), env_path("ProgramFiles(x86)"), env_path("LOCALAPPDATA")];
        s.win_exe.iter().flat_map(|e| roots.iter().map(move |r| r.join(e))).find(|p| p.exists())
    } else {
        s.linux_bins.iter().find_map(|b| {
            Command::new("which").arg(b).output().ok().filter(|o| o.status.success()).map(|_| PathBuf::from(b))
        })
    }
}

fn profiles_of(s: &Spec) -> Vec<Profile> {
    let d = data_dir_of(s);
    if s.id == "firefox" {
        fs::read_to_string(d.join("profiles.ini")).map(|t| parse_profiles_ini(&t)).unwrap_or_default()
    } else {
        fs::read_to_string(d.join("Local State")).map(|t| parse_local_state(&t)).unwrap_or_default()
    }
}

pub fn list() -> Vec<BrowserInfo> {
    SPECS
        .iter()
        .filter(|s| installed(s).is_some())
        .map(|s| BrowserInfo { id: s.id.into(), name: s.name.into(), profiles: profiles_of(s) })
        .collect()
}

/// Valida navegador y perfil contra la lista real, asegura la carpeta y abre la página de extensiones.
/// Devuelve la ruta absoluta a cargar (en Firefox, manifest.firefox.json).
pub fn install(data_dir: &Path, resource_dir: Option<PathBuf>, browser: &str, profile: Option<&str>) -> Result<InstallResult, String> {
    let s = SPECS.iter().find(|s| s.id == browser).ok_or("Navegador no válido")?;
    let app = installed(s).ok_or("Navegador no instalado")?;
    if let Some(p) = profile {
        if !profiles_of(s).iter().any(|x| x.dir == p) {
            return Err("Perfil no válido".into());
        }
    }
    ensure(data_dir, resource_dir)?;
    let folder = data_dir.join("extension");
    let (profile_args, url): (Vec<String>, String) = if s.id == "firefox" {
        (profile.map(|p| vec!["-P".into(), p.into()]).unwrap_or_default(), "about:debugging#/runtime/this-firefox".into())
    } else {
        (profile.map(|p| vec![format!("--profile-directory={p}")]).unwrap_or_default(), format!("{}://extensions", s.scheme))
    };
    let (opened, note) = open_page(s, &app, &profile_args, &url);
    let target = if s.id == "firefox" { folder.join("manifest.firefox.json") } else { folder };
    Ok(InstallResult { path: target.display().to_string(), opened, note })
}

/// Resultado de install: la ruta siempre se devuelve; `opened` indica si se pudo abrir el navegador.
#[derive(serde::Serialize)]
pub struct InstallResult {
    pub path: String,
    pub opened: bool,
    pub note: Option<String>,
}

/// Abre `url` en el navegador. En macOS un navegador ya abierto ignora `open -n --args` y el
/// binario directo, así que se usa AppleScript (`open location`, comprobado con chrome://extensions);
/// solo si no está en marcha se lanza con `open -na` (arranque en frío, respeta el perfil).
/// En Windows/Linux el binario reenvía los argumentos al proceso en marcha (método clásico).
fn open_page(s: &Spec, app: &Path, profile_args: &[String], url: &str) -> (bool, Option<String>) {
    if !cfg!(target_os = "macos") {
        let ok = Command::new(app).args(profile_args).arg(url).spawn().is_ok();
        return (ok, None);
    }
    let running = Command::new("osascript")
        .args(["-e", &format!("application \"{}\" is running", s.mac_app)])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if running && s.id != "firefox" {
        let script = format!("tell application \"{}\" to open location \"{}\"", s.mac_app, url);
        let ok = Command::new("osascript").args(["-e", &script]).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            let note = (!profile_args.is_empty()).then(|| "El navegador ya estaba abierto: se usó su último perfil activo.".to_string());
            return (true, note);
        }
    }
    if running {
        // Firefox en marcha: open -a con la URL en una pestaña nueva del perfil activo.
        let ok = Command::new("open").arg("-a").arg(app).arg("--args").arg("-new-tab").arg(url).status().map(|x| x.success()).unwrap_or(false);
        return (ok, None);
    }
    let ok = Command::new("open").arg("-na").arg(app).arg("--args").args(profile_args).arg(url).status().map(|x| x.success()).unwrap_or(false);
    (ok, None)
}

fn add_dir(z: &mut zip::ZipWriter<fs::File>, root: &Path, dir: &Path) -> Result<(), String> {
    use std::io::Write;
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let ft = e.file_type().map_err(|e| e.to_string())?;
        let p = e.path();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            add_dir(z, root, &p)?;
        } else {
            let rel = p.strip_prefix(root).map_err(|e| e.to_string())?;
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            z.start_file(name, opts).map_err(|e| e.to_string())?;
            z.write_all(&fs::read(&p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Guarda un .zip con el contenido de la carpeta de la extensión (rutas relativas).
pub fn export_zip(data_dir: &Path, resource_dir: Option<PathBuf>, out: &Path) -> Result<(), String> {
    ensure(data_dir, resource_dir)?;
    let root = data_dir.join("extension");
    let mut z = zip::ZipWriter::new(fs::File::create(out).map_err(|e| e.to_string())?);
    add_dir(&mut z, &root, &root)?;
    z.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_state() {
        let j = r#"{"profile":{"info_cache":{"Profile 6":{"name":"Trabajo","user_name":"a@b.cl"},"Default":{"name":"Persona 1","user_name":""}}}}"#;
        let p = parse_local_state(j);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], Profile { dir: "Default".into(), name: "Persona 1".into(), email: None });
        assert_eq!(p[1].email.as_deref(), Some("a@b.cl"));
        assert!(parse_local_state("no json").is_empty());
    }

    #[test]
    fn profiles_ini() {
        let ini = "[Install4F96]\nDefault=Profiles/x.default\n\n[Profile1]\nName=work\nIsRelative=1\nPath=Profiles/w\n\n[Profile0]\nName=default-release\nPath=Profiles/d\n\n[General]\nStartWithLastProfile=1\n";
        let p = parse_profiles_ini(ini);
        assert_eq!(p.iter().map(|x| x.dir.as_str()).collect::<Vec<_>>(), ["work", "default-release"]);
        assert_eq!(p[0].name, "work");
    }
}
