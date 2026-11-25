use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Lista negra: se bloquea lo que coincida.
    #[default]
    Block,
    /// Lista blanca: solo se permite lo que coincida.
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Decision {
    Allow,
    Deny,
}

/// Datos de un trabajo de impresión; los campos desconocidos van en `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Job {
    pub document: String,
    pub app: Option<String>,
    pub user: Option<String>,
    pub origin_ip: Option<String>,
    /// Nombre del equipo de origen cuando solo se conoce el nombre (Windows informa el nombre de equipo, no la IP).
    /// Lo reportan los clientes y no está autenticado: sirve para reglas por equipo, no como prueba de identidad.
    #[serde(default)]
    pub origin_host: Option<String>,
    pub site: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterRules {
    pub enabled: bool,
    pub mode: Option<Mode>,
    /// Patrones de aplicación/documento (subcadena, sin mayúsculas; `*.ext` admitido).
    #[serde(default)]
    pub apps: Vec<String>,
    /// Dominios; `ejemplo.cl` también cubre sus subdominios.
    #[serde(default)]
    pub sites: Vec<String>,
    /// IPs exactas o prefijos CIDR IPv4 (`192.168.1.0/24`).
    #[serde(default)]
    pub ips: Vec<String>,
    /// Reglas suspendidas: patrón → instante (segundos Unix) hasta el que no aplica; `0` = hasta reanudarla a mano.
    /// Pasado el instante la regla vuelve a aplicar sola.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub paused: std::collections::BTreeMap<String, u64>,
}

fn app_matches(pat: &str, job: &Job) -> bool {
    let pat = pat.trim().to_lowercase();
    if pat.is_empty() {
        return false;
    }
    let doc = job.document.to_lowercase();
    let app = job.app.as_deref().unwrap_or("").to_lowercase();
    if let Some(ext) = pat.strip_prefix("*.") {
        let dot = format!(".{ext}");
        // La extensión al final o seguida de un separador ("carta.docx - Pages", "carta.docx (1)").
        let en = |t: &str| {
            t.ends_with(&dot)
                || t.match_indices(&dot).any(|(i, _)| t[i + dot.len()..].starts_with([' ', '-', '(', ')', '[', ']', '_', ',', ';']))
        };
        return en(&doc) || en(&app);
    }
    doc.contains(&pat) || app.contains(&pat)
}

/// Dominio de un patrón o sitio (sin esquema, ruta ni puerto, en minúsculas).
pub fn host_of(site: &str) -> String {
    let s = site.trim().to_lowercase();
    let s = s.split("://").last().unwrap_or("");
    s.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("").to_string()
}

fn site_matches(pat: &str, job: &Job) -> bool {
    let Some(site) = job.site.as_deref() else { return false };
    let (p, h) = (host_of(pat), host_of(site));
    !p.is_empty() && (h == p || h.ends_with(&format!(".{p}")))
}

/// IPv4 o IPv6 (los `::ffff:a.b.c.d` se tratan como IPv4).
pub fn parse_ip(s: &str) -> Option<std::net::IpAddr> {
    let ip: std::net::IpAddr = s.trim().trim_matches(['[', ']']).split('%').next()?.parse().ok()?;
    Some(match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    })
}

fn bits(ip: std::net::IpAddr) -> u128 {
    match ip {
        std::net::IpAddr::V4(v) => u32::from(v) as u128,
        std::net::IpAddr::V6(v) => u128::from(v),
    }
}

/// Nombre de equipo normalizado para comparar (sin `\\` iniciales ni espacios, en minúsculas).
fn host_key(s: &str) -> String {
    s.trim().trim_start_matches('\\').trim().to_lowercase()
}

/// Regla por equipo: el patrón es un nombre (no una IP ni un CIDR) y coincide con el equipo de origen.
fn host_matches(pat: &str, job: &Job) -> bool {
    let p = host_key(pat);
    if p.is_empty() || p.contains('/') || parse_ip(&p).is_some() {
        return false;
    }
    job.origin_host.as_deref().is_some_and(|h| host_key(h) == p)
        // Un origin_ip que no es una IP es un nombre de equipo (así lo informaban las versiones anteriores).
        || job.origin_ip.as_deref().is_some_and(|h| parse_ip(h).is_none() && host_key(h) == p)
}

fn ip_matches(pat: &str, job: &Job) -> bool {
    job.origin_ip.as_deref().and_then(parse_ip).is_some_and(|ip| cidr_matches(pat, ip)) || host_matches(pat, job)
}

fn cidr_matches(pat: &str, ip: std::net::IpAddr) -> bool {
    match pat.trim().split_once('/') {
        Some((base, n)) => {
            let (Some(b), Ok(n)) = (parse_ip(base), n.parse::<u32>()) else { return false };
            if ip.is_ipv4() != b.is_ipv4() {
                return false;
            }
            let width = if ip.is_ipv4() { 32 } else { 128 };
            if n > width {
                return false;
            }
            let mask = if n == 0 { 0 } else { (u128::MAX << (width - n)) & (u128::MAX >> (128 - width)) };
            bits(ip) & mask == bits(b) & mask
        }
        None => parse_ip(pat) == Some(ip),
    }
}

impl PrinterRules {
    /// ¿La regla `pat` está suspendida ahora?
    pub fn is_paused(&self, pat: &str) -> bool {
        self.paused.get(pat).is_some_and(|&t| {
            t == 0 || t > std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
        })
    }

    pub fn matches(&self, job: &Job) -> bool {
        self.matched(job).is_some()
    }

    /// Primera regla que coincide, como texto para el registro.
    pub fn matched(&self, job: &Job) -> Option<String> {
        if let Some(p) = self.apps.iter().find(|p| !self.is_paused(p) && app_matches(p, job)) {
            return Some(format!("Aplicación: {p}"));
        }
        if let Some(p) = self.sites.iter().find(|p| !self.is_paused(p) && site_matches(p, job)) {
            return Some(format!("Sitio: {p}"));
        }
        self.ips.iter().find(|p| !self.is_paused(p) && ip_matches(p, job)).map(|p| format!("IP: {p}"))
    }

    /// Decisión y motivo para el registro.
    pub fn explain(&self, job: &Job) -> (Decision, String) {
        let d = self.decide(job);
        let m = self.matched(job);
        let why = match (self.enabled, self.mode.unwrap_or_default(), m) {
            (false, _, _) => "Control desactivado".to_string(),
            (_, _, Some(r)) => r,
            (_, Mode::Block, None) => "Sin coincidencia".to_string(),
            (_, Mode::Allow, None) => "No está en la lista permitida".to_string(),
        };
        (d, why)
    }

    pub fn decide(&self, job: &Job) -> Decision {
        if !self.enabled {
            return Decision::Allow;
        }
        match (self.mode.unwrap_or_default(), self.matches(job)) {
            (Mode::Block, true) => Decision::Deny,
            (Mode::Block, false) => Decision::Allow,
            (Mode::Allow, true) => Decision::Allow,
            (Mode::Allow, false) => Decision::Deny,
        }
    }
}

/// Combina la regla global con la de la impresora. Bloqueo gana siempre: basta que una de las dos
/// (activas) deniegue. Con una sola activa decide esa.
pub fn explain_both(global: &PrinterRules, specific: &PrinterRules, job: &Job) -> (Decision, String) {
    if !global.enabled {
        return specific.explain(job);
    }
    let (gd, gr) = global.explain(job);
    if !specific.enabled {
        return (gd, format!("Global · {gr}"));
    }
    let (sd, sr) = specific.explain(job);
    match (gd, sd) {
        (Decision::Deny, _) => (Decision::Deny, format!("Global · {gr}")),
        (_, Decision::Deny) => (Decision::Deny, sr),
        _ => (Decision::Allow, sr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(mode: Mode) -> PrinterRules {
        PrinterRules {
            enabled: true,
            mode: Some(mode),
            apps: vec!["Microsoft Word".into(), "*.docx".into()],
            sites: vec!["facebook.com".into()],
            ips: vec!["192.168.1.0/24".into(), "10.0.0.5".into()],
            ..Default::default()
        }
    }
    fn job(doc: &str) -> Job {
        Job { document: doc.into(), ..Default::default() }
    }

    #[test]
    fn block_by_app_and_extension() {
        let r = rules(Mode::Block);
        assert_eq!(r.decide(&job("Microsoft Word - informe")), Decision::Deny);
        assert_eq!(r.decide(&job("carta.DOCX")), Decision::Deny);
        assert_eq!(r.decide(&job("notas.txt")), Decision::Allow);
    }

    #[test]
    fn allow_mode_is_whitelist() {
        let r = rules(Mode::Allow);
        assert_eq!(r.decide(&job("carta.docx")), Decision::Allow);
        assert_eq!(r.decide(&job("notas.txt")), Decision::Deny);
    }

    #[test]
    fn site_includes_subdomains() {
        let r = rules(Mode::Block);
        let mut j = job("x");
        j.site = Some("https://www.facebook.com/a?b=1".into());
        assert_eq!(r.decide(&j), Decision::Deny);
        j.site = Some("notfacebook.com".into());
        assert_eq!(r.decide(&j), Decision::Allow);
    }

    #[test]
    fn ip_exact_and_cidr() {
        let r = rules(Mode::Block);
        let mut j = job("x");
        j.origin_ip = Some("192.168.1.77".into());
        assert_eq!(r.decide(&j), Decision::Deny);
        j.origin_ip = Some("10.0.0.5".into());
        assert_eq!(r.decide(&j), Decision::Deny);
        j.origin_ip = Some("192.168.2.1".into());
        assert_eq!(r.decide(&j), Decision::Allow);
    }

    #[test]
    fn disabled_allows_everything() {
        let mut r = rules(Mode::Block);
        r.enabled = false;
        assert_eq!(r.decide(&job("carta.docx")), Decision::Allow);
    }

    #[test]
    fn json_roundtrip() {
        let r = rules(Mode::Allow);
        let s = serde_json::to_string(&r).unwrap();
        let back: PrinterRules = serde_json::from_str(&s).unwrap();
        assert_eq!(back.mode, Some(Mode::Allow));
        assert_eq!(back.ips.len(), 2);
    }

    #[test]
    fn extension_con_sufijo_del_visor() {
        let j = |d: &str| Job { document: d.into(), app: None, user: None, origin_ip: None, origin_host: None, site: None };
        assert!(app_matches("*.docx", &j("carta.DOCX - Pages")));
        assert!(app_matches("*.docx", &j("carta.docx (1)")));
        assert!(!app_matches("*.docx", &j("carta.docxs")));
        assert!(!app_matches("*.docx", &j("notas.txt")));
    }

    #[test]
    fn ipv6_y_mapeadas() {
        let j = |ip: &str| Job { document: "x".into(), app: None, user: None, origin_ip: Some(ip.into()), origin_host: None, site: None };
        assert!(ip_matches("fe80::/10", &j("fe80::1")));
        assert!(ip_matches("::1", &j("::1")));
        assert!(!ip_matches("fe80::/10", &j("2001:db8::1")));
        assert!(ip_matches("192.168.1.0/24", &j("::ffff:192.168.1.9")));
        assert!(!ip_matches("192.168.1.0/24", &j("fe80::1")));
    }

    #[test]
    fn global_y_especifica_bloqueo_gana() {
        let j = |d: &str| Job { document: d.into(), app: None, user: None, origin_ip: None, origin_host: None, site: None };
        let r = |en, m, apps: &[&str]| PrinterRules { enabled: en, mode: Some(m), apps: apps.iter().map(|a| a.to_string()).collect(), sites: vec![], ips: vec![], ..Default::default() };
        let g = r(true, Mode::Block, &["*.docx"]);
        let off = r(false, Mode::Block, &[]);
        // Solo global activa
        assert_eq!(explain_both(&g, &off, &j("a.docx")).0, Decision::Deny);
        assert_eq!(explain_both(&g, &off, &j("a.txt")).0, Decision::Allow);
        // Específica bloquea .txt, global bloquea .docx: ambos se bloquean
        let sp = r(true, Mode::Block, &["*.txt"]);
        assert_eq!(explain_both(&g, &sp, &j("a.docx")).0, Decision::Deny);
        assert_eq!(explain_both(&g, &sp, &j("a.txt")).0, Decision::Deny);
        assert_eq!(explain_both(&g, &sp, &j("a.pdf")).0, Decision::Allow);
        // Global apagada: solo la específica
        let goff = r(false, Mode::Block, &["*.docx"]);
        assert_eq!(explain_both(&goff, &sp, &j("a.docx")).0, Decision::Allow);
    }
}
