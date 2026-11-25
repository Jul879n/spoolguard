#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Modo host de Native Messaging: el navegador lanza este mismo ejecutable con --native-host.
#[allow(dead_code)]
#[path = "../../core/src/bin/native_host.rs"]
mod native_host;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--native-host") {
        native_host::main()
    } else if let Some(i) = args.iter().position(|a| a == "--guardian") {
        control_impresion_lib::guardian(&args[i + 1..])
    } else {
        control_impresion_lib::run()
    }
}
