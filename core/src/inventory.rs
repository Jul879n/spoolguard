//! Inventario de impresoras y puertos: clasificación del tipo de conexión a partir de lo que informa el
//! sistema (device-uri de CUPS o puerto del spooler de Windows). Solo se informa lo que la fuente entrega;
//! lo que no se sabe queda vacío o `desconocido`.

use serde::Serialize;

/// Una fila del inventario (impresora o cola) con estructura estable.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct InventoryRow {
    pub name: String,
    /// Modelo/driver que informa la cola (vacío = no informado).
    pub driver: String,
    /// device-uri (CUPS) o puerto (Windows), sin usuario, contraseña ni consulta.
    pub uri: String,
    /// `usb` | `red` | `compartida` | `otra`.
    pub kind: String,
    /// `usb`, `socket`, `ipp`, `lpd`, `wsd`, `red`, `compartida`, `archivo`, `local` o `desconocido`.
    pub conn_type: String,
    /// IP/host del dispositivo si es de red.
    pub host: String,
    /// `None` = no informado.
    pub shared: Option<bool>,
    pub status: String,
    /// Retención soportada.
    pub hold_supported: bool,
    /// Control activo (regla propia o global).
    pub controlled: bool,
}

/// Tipo de conexión según el device-uri de CUPS.
pub fn conn_type_uri(uri: &str) -> &'static str {
    let Some((scheme, _)) = uri.trim().split_once(':') else { return "desconocido" };
    match scheme.to_ascii_lowercase().as_str() {
        "usb" | "hp" | "hpfax" | "ippusb" => "usb",
        "socket" => "socket",
        "ipp" | "ipps" | "http" | "https" => "ipp",
        "lpd" => "lpd",
        "dnssd" | "mdns" => "red",
        "smb" => "compartida",
        "file" | "cups-pdf" => "archivo",
        "serial" | "parallel" | "scsi" | "bluetooth" => "local",
        _ => "desconocido",
    }
}

/// Host/IP y puerto de una URI de red directa (`scheme://[user@]host[:port]/...`), con el puerto por defecto del esquema.
pub fn host_port(uri: &str) -> Option<(String, u16)> {
    let (scheme, rest) = uri.split_once("://")?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !h.ends_with(':') => (h, p.parse().ok()?),
        _ => (auth, match scheme.to_ascii_lowercase().as_str() { "socket" => 9100, "lpd" => 515, "ipps" | "https" => 443, _ => 631 }),
    };
    Some((host.trim_matches(['[', ']']).to_string(), port))
}

/// Host del dispositivo cuando la URI es de red directa (socket, ipp, lpd, smb); vacío si no.
pub fn net_host(uri: &str) -> String {
    match conn_type_uri(uri) {
        "socket" | "ipp" | "lpd" | "compartida" => host_port(uri).map(|(h, _)| h).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Tipo de conexión según el puerto del spooler de Windows. `host_address` es `PrinterHostAddress` del
/// puerto TCP/IP estándar y `port_number` su `PortNumber` (0 = no informado).
pub fn conn_type_port(port: &str, host_address: &str, port_number: u32) -> &'static str {
    let up = port.trim().to_uppercase();
    let is_path = up.contains(":\\") || up.ends_with(".PRN") || up.ends_with(".PDF") || up.ends_with(".XPS") || up.ends_with(".OXPS");
    if up.starts_with("USB") {
        "usb"
    } else if up.starts_with("WSD") {
        "wsd"
    } else if up.starts_with("\\\\") {
        "compartida"
    } else if ["HTTP://", "HTTPS://", "IPP://", "IPPS://"].iter().any(|p| up.starts_with(p)) {
        "ipp"
    } else if up.starts_with("FILE:") || up.starts_with("PORTPROMPT") || up == "XPSPORT:" || up == "NUL:" || up == "NUL" || is_path {
        "archivo"
    } else if up.starts_with("LPT") || up.starts_with("COM") || up.starts_with("SHRFAX") {
        "local"
    } else if !host_address.trim().is_empty() || up.starts_with("IP_") || up.parse::<std::net::Ipv4Addr>().is_ok() || up.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        let p = if port_number != 0 { port_number } else { up.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(0) };
        match p {
            9100 => "socket",
            515 => "lpd",
            631 => "ipp",
            _ => "red",
        }
    } else {
        "desconocido"
    }
}

/// Host de un puerto de Windows: `PrinterHostAddress` si existe; si no, de `IP_x`, `x.x.x.x` o `host:puerto`.
pub fn host_of_port(port: &str, host_address: &str) -> String {
    let ha = host_address.trim();
    if !ha.is_empty() {
        return ha.to_string();
    }
    let p = port.trim();
    if let Some(r) = p.strip_prefix("IP_").or_else(|| p.strip_prefix("ip_")) {
        return r.to_string();
    }
    if p.parse::<std::net::Ipv4Addr>().is_ok() {
        return p.to_string();
    }
    if let Some((h, n)) = p.rsplit_once(':') {
        if !h.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && h.parse::<std::net::Ipv4Addr>().is_ok() {
            return h.to_string();
        }
    }
    if let Some((_, rest)) = p.split_once("://") {
        return host_port(&format!("x://{rest}")).map(|(h, _)| h).unwrap_or_default();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tipo_por_uri_de_cups() {
        for (u, t) in [
            ("usb://HP/x?serial=1", "usb"), ("ippusb://HP/ipp", "usb"), ("socket://1.2.3.4:9100", "socket"), ("ipp://h/ipp/print", "ipp"),
            ("ipps://h/x", "ipp"), ("http://h:631/printers/x", "ipp"), ("lpd://1.2.3.4/q", "lpd"), ("dnssd://Foo._ipp._tcp.local/", "red"),
            ("smb://srv/p", "compartida"), ("file:///dev/null", "archivo"), ("cups-pdf:/", "archivo"), ("serial:/dev/ttyS0", "local"),
            ("", "desconocido"), ("algo-raro://x", "desconocido"),
        ] {
            assert_eq!(conn_type_uri(u), t, "{u}");
        }
    }

    #[test]
    fn host_solo_si_es_red_directa() {
        assert_eq!(net_host("socket://admin:pw@10.0.0.5:9100?x=1"), "10.0.0.5");
        assert_eq!(net_host("ipp://[fe80::1]:631/ipp"), "fe80::1");
        assert_eq!(net_host("lpd://impresora.local/cola"), "impresora.local");
        assert_eq!(net_host("usb://HP/x"), "");
        assert_eq!(net_host("dnssd://Foo._ipp._tcp.local/"), "");
        assert_eq!(host_port("socket://10.0.0.5"), Some(("10.0.0.5".into(), 9100)));
        assert_eq!(host_port("ipp://10.0.0.5/ipp"), Some(("10.0.0.5".into(), 631)));
    }

    #[test]
    fn tipo_por_puerto_de_windows() {
        for (p, h, n, t) in [
            ("USB001", "", 0, "usb"), ("WSD-1234-abcd", "", 0, "wsd"), (r"\\srv\hp", "", 0, "compartida"), ("http://10.0.0.2:631/ipp/print", "", 0, "ipp"),
            ("IP_10.0.0.5", "10.0.0.5", 9100, "socket"), ("IP_10.0.0.5", "10.0.0.5", 515, "lpd"), ("IP_10.0.0.5", "10.0.0.5", 0, "red"),
            ("192.168.1.5:9100", "", 0, "socket"), ("192.168.1.5", "", 0, "red"), ("impresora01:515", "", 0, "lpd"),
            ("FILE:", "", 0, "archivo"), ("PORTPROMPT:", "", 0, "archivo"), ("XPSPort:", "", 0, "archivo"), ("nul:", "", 0, "archivo"),
            (r"C:\e2e\out\prx.prn", "", 0, "archivo"), ("LPT1:", "", 0, "local"), ("COM1:", "", 0, "local"), ("SHRFAX:", "", 0, "local"),
            ("salida.v2.prn", "", 0, "archivo"), ("algo", "", 0, "desconocido"),
        ] {
            assert_eq!(conn_type_port(p, h, n), t, "{p}");
        }
    }

    #[test]
    fn host_del_puerto() {
        assert_eq!(host_of_port("IP_10.0.0.5", ""), "10.0.0.5");
        assert_eq!(host_of_port("COM1_net", "10.0.0.7"), "10.0.0.7");
        assert_eq!(host_of_port("192.168.1.5:9100", ""), "192.168.1.5");
        assert_eq!(host_of_port("http://10.0.0.2:631/ipp", ""), "10.0.0.2");
        assert_eq!(host_of_port("USB001", ""), "");
    }
}
