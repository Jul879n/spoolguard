pub mod backup;
pub mod cups;
pub mod direct;
pub mod discover;
pub mod events;
pub mod extension;
pub mod intents;
pub mod inventory;
pub mod rules;
pub mod update;
#[cfg(windows)]
pub mod spooler;

/// Prefijo del error cuando Windows niega administrar una cola (0x80070005); la interfaz ofrece concederlo.
pub const ACCESS_DENIED: &str = "ACCESS_DENIED";
