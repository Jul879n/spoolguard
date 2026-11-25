//! PoC: aplica reglas a los trabajos retenidos de una cola CUPS.
//! Uso: cups-poc <impresora> <work_dir> <block|allow> <patron>...
use control_core::{cups, rules::*};
use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("uso: cups-poc <impresora> <work_dir> <block|allow> <patron>...");
        std::process::exit(2);
    }
    let rules = PrinterRules {
        enabled: true,
        mode: Some(if a[3] == "allow" { Mode::Allow } else { Mode::Block }),
        apps: a[4..].to_vec(),
        ..Default::default()
    };
    for id in cups::held_job_ids(&a[1]).expect("lpstat") {
        let h = cups::job_info(Path::new(&a[2]), id).expect("ipptool");
        if !h.held {
            continue;
        }
        let d = rules.decide(&h.job);
        println!("trabajo {id}: {:?} user={:?} -> {d:?}", h.job.document, h.job.user);
        match d {
            Decision::Allow => cups::release(id).expect("release"),
            Decision::Deny => drop(cups::cancel(id).expect("cancel")),
        }
    }
}
