//! The golden comparison, as a test: every screen and menu against the
//! committed images. Run by `tools/test.sh`; fails on drift.
//!
//! On failure the rendering and a diff image are left in
//! `target/golden-diff/`. If the change is intended:
//!
//! ```text
//! cargo run -p oxinode-sim --target <host> -- golden --update
//! ```

use std::path::PathBuf;

use oxinode_sim::golden::{self, Verdict};

#[test]
fn every_screen_and_menu_matches_its_golden_image() {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden");
    let diff = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("golden-diff");
    let verdicts = golden::check(&golden, &diff);
    assert!(!verdicts.is_empty());
    let failures: Vec<String> = verdicts
        .iter()
        .filter(|(_, v)| *v != Verdict::Match)
        .map(|(case, v)| format!("{}: {v:?}", case.name))
        .collect();
    assert!(
        failures.is_empty(),
        "{} golden images do not match (evidence in {}); \
         if the change is intended, run `oxinode-sim golden --update`:\n  {}",
        failures.len(),
        diff.display(),
        failures.join("\n  ")
    );
}
