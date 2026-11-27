fn main() {
    // En Windows el MSI empaqueta winguard.dll (print processor de la protección previa a la impresión): se compila aquí, con
    // el perfil `dll` (panic = "unwind", ver Cargo.toml) y en una carpeta de destino propia (el cargo de fuera tiene bloqueada la suya).
    // Así `cargo test`, `cargo clippy` y `tauri build` funcionan sin pasos previos.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_winguard();
    }
    tauri_build::build()
}

fn build_winguard() {
    use std::{env, path::PathBuf, process::Command};
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().expect("raíz del workspace");
    println!("cargo:rerun-if-changed=../winguard/src");
    println!("cargo:rerun-if-changed=../winguard/Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.toml");
    let target = env::var("TARGET").expect("TARGET");
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "winguard", "--profile", "dll", "--target", &target, "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target").join("winguard-build"))
        // Opciones del cargo de fuera (p. ej. `--release` con panic=abort) no deben llegar a la DLL.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .expect("no se pudo ejecutar cargo para compilar winguard");
    assert!(status.success(), "falló la compilación de winguard (print processor)");
}
