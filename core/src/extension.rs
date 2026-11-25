//! Archivos de la extensión del navegador: versión empaquetada en la app y copia a `<datos>/extension`.
//! Lo usan la app (`browser::ensure`) y el host de Native Messaging (`get_version`, `update_extension`).
use std::{fs, io, path::{Path, PathBuf}};

/// Archivo de `<datos>` con la ruta de la carpeta `extension` de los recursos de la app (lo escribe `record_source`).
const SOURCE_FILE: &str = "ext_source";

/// Copia `src` en `dst` de forma recursiva (sobrescribe).
pub fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

/// `version` del `manifest.json` de una carpeta de extensión.
pub fn manifest_version(dir: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).ok()?).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

/// Copia la extensión de los recursos a `<datos>/extension` y recuerda de dónde salió.
pub fn install_from(data_dir: &Path, src: &Path) -> io::Result<()> {
    copy_dir(src, &data_dir.join("extension"))?;
    fs::write(data_dir.join(SOURCE_FILE), src.display().to_string())
}

/// Carpeta de la extensión empaquetada en la app, según la última vez que la app la instaló.
pub fn bundled_dir(data_dir: &Path) -> Option<PathBuf> {
    let p = PathBuf::from(fs::read_to_string(data_dir.join(SOURCE_FILE)).ok()?.trim());
    manifest_version(&p).map(|_| p)
}

/// Versiones: la empaquetada en la app y la copiada en `<datos>/extension`.
pub fn versions(data_dir: &Path) -> Result<(String, Option<String>), String> {
    let src = bundled_dir(data_dir).ok_or("La app no tiene la extensión empaquetada: ábrela una vez")?;
    let v = manifest_version(&src).ok_or("manifest.json inválido")?;
    Ok((v, manifest_version(&data_dir.join("extension"))))
}

/// Copia la extensión empaquetada a `<datos>/extension` y devuelve su versión. Nunca ejecuta nada.
pub fn update_installed(data_dir: &Path) -> Result<String, String> {
    let (v, _) = versions(data_dir)?;
    let src = bundled_dir(data_dir).ok_or("sin extensión empaquetada")?;
    copy_dir(&src, &data_dir.join("extension")).map_err(|e| e.to_string())?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ci-ext-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_ext(dir: &Path, version: &str) {
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(dir.join("manifest.json"), format!(r#"{{"version":"{version}"}}"#)).unwrap();
        fs::write(dir.join("icons/a.png"), version).unwrap();
    }

    #[test]
    fn instala_y_actualiza_desde_los_recursos() {
        let root = tmp("flujo");
        let (res, data) = (root.join("res/extension"), root.join("data"));
        fs::create_dir_all(&data).unwrap();
        assert!(versions(&data).is_err(), "sin ext_source no hay versión");
        make_ext(&res, "0.1.0");
        install_from(&data, &res).unwrap();
        assert_eq!(versions(&data).unwrap(), ("0.1.0".into(), Some("0.1.0".into())));
        // La app se actualiza: los recursos traen 0.2.0 pero <datos>/extension sigue en 0.1.0.
        make_ext(&res, "0.2.0");
        assert_eq!(versions(&data).unwrap(), ("0.2.0".into(), Some("0.1.0".into())));
        assert_eq!(update_installed(&data).unwrap(), "0.2.0");
        assert_eq!(manifest_version(&data.join("extension")).as_deref(), Some("0.2.0"));
        assert_eq!(fs::read_to_string(data.join("extension/icons/a.png")).unwrap(), "0.2.0");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn origen_invalido() {
        let root = tmp("inv");
        fs::write(root.join(SOURCE_FILE), root.join("no-existe").display().to_string()).unwrap();
        assert!(bundled_dir(&root).is_none());
        assert!(update_installed(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
