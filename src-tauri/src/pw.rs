//! Hash de contraseña. Formato idéntico al del servidor Go (internal/app/password.go):
//! `pbkdf2-sha256$<iteraciones>$<sal hex>$<clave derivada hex>`.
//! Los hashes antiguos (SHA-256 de sal+contraseña, sal aparte) se aceptan y se migran al iniciar sesión.

use pbkdf2::pbkdf2_hmac;
use sha2::{Digest, Sha256};

const SCHEME: &str = "pbkdf2-sha256";
pub const ITER: u32 = 600_000; // recomendación OWASP 2023
const ITER_MAX: u32 = 5_000_000; // una config manipulada no debe colgar la app
const KEY_LEN: usize = 32;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

pub fn is_kdf(stored: &str) -> bool {
    stored.starts_with("pbkdf2-sha256$")
}

pub fn hash_with(pw: &str, salt: &[u8], iter: u32) -> String {
    let mut dk = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(pw.as_bytes(), salt, iter, &mut dk);
    format!("{SCHEME}${iter}${}${}", hex(salt), hex(&dk))
}

/// Hash nuevo con sal aleatoria de 16 bytes.
pub fn hash(pw: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("sin fuente de aleatorios del sistema");
    hash_with(pw, &salt, ITER)
}

fn verify_kdf(pw: &str, stored: &str) -> bool {
    let p: Vec<&str> = stored.split('$').collect();
    if p.len() != 4 || p[0] != SCHEME {
        return false;
    }
    let Ok(iter) = p[1].parse::<u32>() else { return false };
    if !(1..=ITER_MAX).contains(&iter) {
        return false;
    }
    let (Some(salt), Some(want)) = (unhex(p[2]), unhex(p[3])) else { return false };
    if salt.is_empty() || want.len() != KEY_LEN {
        return false;
    }
    let mut dk = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(pw.as_bytes(), &salt, iter, &mut dk);
    ct_eq(&dk, &want)
}

fn legacy(salt: &str, pw: &str) -> String {
    hex(&Sha256::digest(format!("{salt}{pw}").as_bytes()))
}

/// Resultado de comprobar una contraseña.
pub enum Check {
    No,
    Ok,
    /// Correcta con el formato antiguo: conviene reescribirla con `hash`.
    OkMigrate,
}

pub fn check(pw: &str, salt: &str, stored: &str) -> Check {
    if stored.is_empty() {
        return Check::No;
    }
    if is_kdf(stored) {
        return if verify_kdf(pw, stored) { Check::Ok } else { Check::No };
    }
    if ct_eq(legacy(salt, pw).as_bytes(), stored.as_bytes()) {
        Check::OkMigrate
    } else {
        Check::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mismo vector que internal/app/password_test.go (verificado además con Python hashlib).
    const VECTOR: &str = "pbkdf2-sha256$1000$000102030405060708090a0b0c0d0e0f$0bf74a30e91f54e3f085113e2db57b84e1ecaa89d2065a9d77015dee173f4aef";

    #[test]
    fn pw_vector_compartido() {
        let salt: Vec<u8> = (0u8..16).collect();
        assert_eq!(hash_with("contraseña-ñandú", &salt, 1000), VECTOR);
        assert!(matches!(check("contraseña-ñandú", "", VECTOR), Check::Ok));
        assert!(matches!(check("otra", "", VECTOR), Check::No));
    }

    #[test]
    fn pw_rechaza_malformados() {
        for h in ["", "pbkdf2-sha256$", "pbkdf2-sha256$0$00$00", "pbkdf2-sha256$99999999$00$0000000000000000000000000000000000000000000000000000000000000000", "pbkdf2-sha256$1000$zz$00"] {
            assert!(matches!(check("x", "", h), Check::No), "{h}");
        }
    }

    #[test]
    fn pw_migra_hash_antiguo() {
        let old = legacy("1700000000", "vieja-clave");
        assert!(matches!(check("vieja-clave", "1700000000", &old), Check::OkMigrate));
        assert!(matches!(check("mala", "1700000000", &old), Check::No));
        let n = hash("vieja-clave");
        assert!(matches!(check("vieja-clave", "", &n), Check::Ok));
        assert_ne!(n, hash("vieja-clave")); // sal aleatoria
    }
}
