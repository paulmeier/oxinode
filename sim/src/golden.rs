//! Golden images: what every screen and every menu is supposed to look like.
//!
//! The pixel assertions in `oxinode-core` say where nothing is drawn. A golden
//! image says what the thing looks like, and a change to it -- a font tweak,
//! an off-by-one in a menu's centring -- fails the comparison and leaves a
//! picture of the difference behind, which is a review artefact rather than a
//! number.
//!
//! The set is generated from the screen list, not written out, so a screen or
//! a menu item added to the core is a golden image *missing* on the next run
//! rather than a screen nobody looks at. Since phase 11 every screen is drawn
//! twice: once from a board that knows nothing, once from one mid-session, so
//! that the empty-state rule -- a dash, never a plausible zero -- is in the
//! pictures as well as in the tests. Since phase 12 every editor is drawn
//! too, open on a standalone board, refused where a refusal can be reached,
//! and as the notice a host on the line turns it into.

use std::fs;
use std::path::{Path, PathBuf};

use oxinode_core::edit::Field;
use oxinode_core::screens::State;
use oxinode_core::ui::{Action, Screen};

use crate::image::Image;
use crate::scene::{self, Scene};
use crate::script;

/// One state worth a picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Case {
    /// The file stem under the golden directory.
    pub name: String,
    /// How to get there from a fresh [`Scene`].
    pub script: String,
    /// What the screens draw from.
    pub state: State,
}

impl Case {
    /// Render this case.
    pub fn render(&self) -> Image {
        let mut scene = Scene::with_state(self.state);
        let inputs = script::parse(&self.script).expect("a golden case's script parses");
        scene.run(&inputs);
        Image::render(&scene.frame())
    }
}

/// Every screen empty and populated, every item of every menu, a scrolled
/// page at each end and in the middle, and the two overlays that are not
/// screens: a pairing in progress, and a refused configuration.
pub fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let populated = scene::populated();
    for screen in Screen::ALL {
        let walk = format!("right*{}", screen.index());
        let title = slug(screen.title());
        cases.push(Case {
            name: title.clone(),
            script: walk.clone(),
            state: State::default(),
        });
        cases.push(Case {
            name: format!("{title}-populated"),
            script: walk.clone(),
            state: populated,
        });
        for (n, item) in screen.menu().iter().enumerate() {
            cases.push(Case {
                name: format!("{title}-menu-{}", slug(item.label)),
                script: format!("{walk} select down*{n}"),
                state: populated,
            });
        }
    }
    // The radio screen is the long one; see `scene::populated`.
    let radio = format!("right*{}", Screen::Radio.index());
    for (name, downs) in [("top", 0), ("middle", 2), ("bottom", 100)] {
        cases.push(Case {
            name: format!("radio-scrolled-{name}"),
            script: format!("{radio} down*{downs}"),
            state: populated,
        });
    }
    cases.push(Case {
        name: "bluetooth-pairing".to_string(),
        script: format!("right*{}", Screen::Bluetooth.index()),
        state: scene::pairing(),
    });
    cases.push(Case {
        name: "radio-refused".to_string(),
        script: radio.clone(),
        state: scene::refused(),
    });

    // Phase 12: the editors. Each one open on a standalone board; the ones
    // that can be driven to a refusal, refused; one after a confirmed change,
    // back on the screen showing it; and the notice a host on the line turns
    // an edit -- or the toggle -- into.
    let standalone = scene::standalone();
    let item_of = |field: Field| {
        Screen::Radio
            .menu()
            .iter()
            .position(|i| i.action == Action::Edit(field))
            .expect("every field has a menu item")
    };
    for field in Field::ALL {
        let open = format!("{radio} select down*{} select", item_of(field));
        cases.push(Case {
            name: format!("radio-edit-{}", slug(field.label())),
            script: open.clone(),
            state: standalone,
        });
        // Every refusal a stepper or the digits can reach, from the fixture:
        // the first digit down takes 915 MHz to 815; two down from 125 kHz
        // is 41.7 kHz; four up from 17 dBm is 21 dBm.
        let refuse = match field {
            Field::Frequency => Some("down select"),
            Field::Bandwidth => Some("down*2 select"),
            Field::TxPower => Some("up*4 select"),
            Field::SpreadingFactor | Field::CodingRate => None,
        };
        if let Some(refuse) = refuse {
            cases.push(Case {
                name: format!("radio-edit-{}-refused", slug(field.label())),
                script: format!("{open} {refuse}"),
                state: standalone,
            });
        }
    }
    cases.push(Case {
        name: "radio-edit-frequency-cursor".to_string(),
        script: format!(
            "{radio} select down*{} select right*4 up",
            item_of(Field::Frequency)
        ),
        state: standalone,
    });
    cases.push(Case {
        name: "radio-after-edit".to_string(),
        script: format!(
            "{radio} select down*{} select up select",
            item_of(Field::TxPower)
        ),
        state: standalone,
    });
    let toggle = Screen::Radio
        .menu()
        .iter()
        .position(|i| i.action == Action::ToggleRadio)
        .expect("the toggle is on the menu");
    cases.push(Case {
        name: "radio-locked-edit".to_string(),
        script: format!("{radio} select down*{} select", item_of(Field::Frequency)),
        state: populated,
    });
    cases.push(Case {
        name: "radio-locked-toggle".to_string(),
        script: format!("{radio} select down*{toggle} select"),
        state: scene::phone(),
    });
    cases
}

/// A label as a file name: lower case, one hyphen per run of anything else.
pub fn slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// One case's verdict.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The rendering matches the committed image.
    Match,
    /// No committed image; the rendering was written beside where it should
    /// be, as `<name>.actual.png`, for inspection and, if right, promotion.
    Missing { actual: PathBuf },
    /// The committed image could not be read as one of ours.
    Unreadable { reason: String },
    /// They differ; the rendering and a picture of the difference were
    /// written.
    Differs {
        pixels: usize,
        actual: PathBuf,
        diff: PathBuf,
    },
}

/// Compare every case against `golden_dir`, writing evidence of any mismatch
/// into `diff_dir`.
///
/// Returns the verdicts in case order. The caller decides what a failure is;
/// the test treats anything but [`Verdict::Match`] as one.
pub fn check(golden_dir: &Path, diff_dir: &Path) -> Vec<(Case, Verdict)> {
    cases()
        .into_iter()
        .map(|case| {
            let verdict = check_one(&case, golden_dir, diff_dir);
            (case, verdict)
        })
        .collect()
}

fn check_one(case: &Case, golden_dir: &Path, diff_dir: &Path) -> Verdict {
    let actual = case.render();
    let golden_path = golden_dir.join(format!("{}.png", case.name));
    let bytes = match fs::read(&golden_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            let path = write(diff_dir, &format!("{}.actual.png", case.name), &actual);
            return Verdict::Missing { actual: path };
        }
    };
    let expected = match Image::from_png(&bytes) {
        Ok(image) => image,
        Err(reason) => return Verdict::Unreadable { reason },
    };
    match Image::diff(&expected, &actual) {
        None => Verdict::Match,
        Some(diff) => Verdict::Differs {
            pixels: Image::differing_pixels(&expected, &actual),
            actual: write(diff_dir, &format!("{}.actual.png", case.name), &actual),
            diff: write(diff_dir, &format!("{}.diff.png", case.name), &diff),
        },
    }
}

/// Render every case into `golden_dir`, replacing what is there.
///
/// Returns the paths written. Stale images -- for a screen that no longer
/// exists -- are left alone rather than deleted, because a directory this
/// function empties is one nobody can keep anything else in.
pub fn update(golden_dir: &Path) -> Vec<PathBuf> {
    cases()
        .iter()
        .map(|case| write(golden_dir, &format!("{}.png", case.name), &case.render()))
        .collect()
}

fn write(dir: &Path, name: &str, image: &Image) -> PathBuf {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
    let path = dir.join(name);
    fs::write(&path, image.to_png()).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxinode_core::ui;
    use std::collections::HashSet;

    /// The case list is the screen list: a picture of every screen empty and
    /// populated, of every item of every menu, and nothing twice.
    #[test]
    fn the_cases_cover_every_screen_and_every_menu_item() {
        let cases = cases();
        let names: HashSet<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names.len(), cases.len(), "a name repeats");
        let items: usize = Screen::ALL.iter().map(|s| s.menu().len()).sum();
        // Screens twice, every menu item, three scroll positions, pairing
        // and refused, then phase 12: five editors, three refusals, the
        // cursor, the screen after an edit, and two notices.
        assert_eq!(
            cases.len(),
            2 * Screen::COUNT + items + 3 + 2 + Field::ALL.len() + 3 + 1 + 1 + 2
        );
        for field in Field::ALL {
            assert!(names.contains(format!("radio-edit-{}", slug(field.label())).as_str()));
        }
        assert!(names.contains("radio-edit-tx-power-refused"));
        assert!(names.contains("radio-locked-edit"));
        for screen in Screen::ALL {
            let title = slug(screen.title());
            assert!(names.contains(title.as_str()), "{screen:?}");
            assert!(names.contains(format!("{title}-populated").as_str()));
            let empty = cases.iter().find(|c| c.name == title).unwrap();
            assert_eq!(empty.state, State::default(), "{screen:?} empty");
            for item in screen.menu() {
                let name = format!("{}-menu-{}", slug(screen.title()), slug(item.label));
                assert!(names.contains(name.as_str()), "{name}");
            }
        }
    }

    /// Each case's script really lands on the state its name claims.
    #[test]
    fn every_case_lands_where_its_name_says() {
        for case in cases() {
            let mut scene = Scene::with_state(case.state);
            scene.run(&script::parse(&case.script).unwrap());
            let screen = scene.nav.screen();
            assert!(
                case.name.starts_with(&slug(screen.title())),
                "{} is on {screen:?}",
                case.name
            );
            match case.name.split("-menu-").nth(1) {
                Some(item) => {
                    let selected = scene.nav.menu_item().expect("menu open");
                    assert_eq!(slug(screen.menu()[selected].label), item, "{}", case.name);
                }
                None => assert!(!scene.nav.menu_is_open(), "{}", case.name),
            }
            // An editor case has its editor open on the field it names,
            // refused exactly when the name says so; a locked case shows
            // the notice; anything else is a plain screen.
            if let Some(rest) = case.name.strip_prefix("radio-edit-") {
                let editor = scene.nav.editor().expect(&case.name);
                assert!(
                    rest.starts_with(&slug(editor.field().label())),
                    "{}",
                    case.name
                );
                assert_eq!(
                    editor.refused().is_some(),
                    rest.ends_with("-refused"),
                    "{}",
                    case.name
                );
                if rest.ends_with("-cursor") {
                    assert!(editor.cursor() > 0 && editor.changed(), "{}", case.name);
                }
            } else if case.name.starts_with("radio-locked-") {
                assert!(scene.nav.notice_shown().is_some(), "{}", case.name);
            } else {
                assert!(!scene.nav.is_editing(), "{}", case.name);
            }
        }
        // The screen after an edit shows the edited value, not the old one.
        let after = cases()
            .into_iter()
            .find(|c| c.name == "radio-after-edit")
            .unwrap();
        let mut scene = Scene::with_state(after.state);
        scene.run(&script::parse(&after.script).unwrap());
        assert_ne!(scene.state.radio.config, scene::standalone().radio.config);
        let mut lines = oxinode_core::screens::Lines::new();
        scene.state.lines(Screen::Radio, &mut lines);
        assert!((0..lines.len()).any(|n| lines.get(n).unwrap().ends_with("18 dBm")));
        // The scrolled cases really scroll: the bottom is past the middle,
        // and the middle is past the top.
        let scroll_of = |name: &str| {
            let case = cases().into_iter().find(|c| c.name == name).unwrap();
            let mut scene = Scene::with_state(case.state);
            scene.run(&script::parse(&case.script).unwrap());
            scene.frame();
            scene.nav.scroll()
        };
        assert_eq!(scroll_of("radio-scrolled-top"), 0);
        assert!(scroll_of("radio-scrolled-middle") > 0);
        assert!(scroll_of("radio-scrolled-bottom") > scroll_of("radio-scrolled-middle"));
        let mut lines = oxinode_core::screens::Lines::new();
        scene::populated().lines(Screen::Radio, &mut lines);
        assert_eq!(
            scroll_of("radio-scrolled-bottom"),
            lines.len() - ui::visible_lines()
        );
    }

    #[test]
    fn slugs_are_file_names() {
        assert_eq!(slug("Radio On/Off"), "radio-on-off");
        assert_eq!(slug("Back"), "back");
        assert_eq!(slug("  Forget  Phones! "), "forget-phones");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxinode-sim-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Freshly written goldens match; a missing one is reported and written
    /// out; a changed one is reported with a diff.
    #[test]
    fn the_check_matches_what_update_wrote_and_reports_what_it_did_not() {
        let golden = scratch("golden");
        let diff = scratch("diff");
        let written = update(&golden);
        assert_eq!(written.len(), cases().len());
        assert!(written.iter().all(|p| p.exists()));

        let verdicts = check(&golden, &diff);
        assert!(
            verdicts.iter().all(|(_, v)| *v == Verdict::Match),
            "{verdicts:?}"
        );
        assert!(!diff.exists(), "nothing to report, nothing written");

        // Remove one, corrupt one, and alter one.
        let first = &cases()[0];
        fs::remove_file(golden.join(format!("{}.png", first.name))).unwrap();
        let second = &cases()[1];
        fs::write(golden.join(format!("{}.png", second.name)), b"nope").unwrap();
        let third = &cases()[2];
        let mut altered = cases()[3].render();
        assert_ne!(altered, third.render());
        altered = Image::diff(&altered, &third.render()).unwrap();
        fs::write(golden.join(format!("{}.png", third.name)), altered.to_png()).unwrap();

        let verdicts = check(&golden, &diff);
        match &verdicts[0].1 {
            Verdict::Missing { actual } => assert!(actual.exists()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(verdicts[1].1, Verdict::Unreadable { .. }));
        match &verdicts[2].1 {
            Verdict::Differs {
                pixels,
                actual,
                diff,
            } => {
                assert!(*pixels > 0);
                assert!(actual.exists() && diff.exists());
                assert!(Image::from_png(&fs::read(diff).unwrap()).is_ok());
            }
            other => panic!("{other:?}"),
        }
        assert!(verdicts[3..].iter().all(|(_, v)| *v == Verdict::Match));

        let _ = fs::remove_dir_all(golden);
        let _ = fs::remove_dir_all(diff);
    }
}
