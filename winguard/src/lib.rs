//! SpoolGuard `winguard`: print processor de Windows que envuelve a `winprint.dll`.
//!
//! Cada trabajo espera, **antes de enviar datos a la impresora**, un veredicto `allow`/`deny` de SpoolGuard en
//! `%ProgramData%\SpoolGuard\verdicts\<cola>-<id>` (mismo modelo que el filtro de CUPS, ver docs/proteccion-previa.md).
//! `deny` descarta los datos; `allow`, o sin veredicto a tiempo (falla abierta), delega en `winprint.dll`.
//!
//! La DLL corre dentro de `spoolsv.exe` como SYSTEM: sin dependencias, sin `unwrap`/índices que puedan hacer panic, y
//! cada exportación atrapa los panics (`catch_unwind`). Para que eso funcione hay que compilar con `panic = "unwind"`:
//! `cargo build -p winguard --profile dll` (el perfil `release` del proyecto usa `abort`).
#![cfg_attr(not(windows), allow(dead_code))]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

#[cfg(all(windows, panic = "abort"))]
compile_error!("winguard necesita panic = \"unwind\" (catch_unwind): compila con `cargo build -p winguard --profile dll`");

pub mod verdict;

#[cfg(windows)]
mod exports;
#[cfg(windows)]
mod sys;
