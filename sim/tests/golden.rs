//! The golden comparison, as a test: every screen and menu against the
//! committed images. Run by `tools/test.sh`; fails on drift.
//!
//! On failure the rendering and a diff image are left in
//! `target/golden-diff/`. If the change is intended:
//!
//! ```text
//! cargo run -p oxinode-sim --target <host> -- golden --update
//! cargo run -p oxinode-sim --target <host> --features node -- golden --update
//! ```
//!
//! The second is the node image's set, under `golden/node/`; see
//! `oxinode_sim::golden::dir`.

use oxinode_sim::golden::{self, Verdict};

#[test]
fn every_screen_and_menu_matches_its_golden_image() {
    let golden = golden::dir();
    let diff = golden::diff_dir();
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
