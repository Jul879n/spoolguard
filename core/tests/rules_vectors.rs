//! Vectores compartidos con la implementación Go (`internal/rules`): `tests/vectors/rules.json`.
use control_core::rules::{explain_both, Decision, Job, PrinterRules};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    global: Option<PrinterRules>,
    specific: Option<PrinterRules>,
    job: Job,
    decision: String,
    reason: String,
}

#[derive(Deserialize)]
struct File {
    cases: Vec<Case>,
}

#[test]
fn vectores_de_reglas() {
    let f: File = serde_json::from_str(include_str!("vectors/rules.json")).expect("rules.json inválido");
    assert!(f.cases.len() >= 40);
    for c in f.cases {
        let (g, s) = (c.global.unwrap_or_default(), c.specific.unwrap_or_default());
        let (d, why) = explain_both(&g, &s, &c.job);
        let d = if d == Decision::Allow { "Allow" } else { "Deny" };
        assert_eq!(d, c.decision, "decisión: {}", c.name);
        assert_eq!(why, c.reason, "motivo: {}", c.name);
    }
}
