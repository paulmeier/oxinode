//! Golden images: what every screen and every menu is supposed to look like.
//!
//! The pixel assertions in `oxinode-core` and `monopanel` say where nothing
//! is drawn. A golden image says what the thing looks like, and a change to
//! it -- a font tweak, an off-by-one in a menu's centring -- fails the
//! comparison and leaves a picture of the difference behind, which is a
//! review artefact rather than a number.
//!
//! The set is generated from the screen list, not written out, so a screen or
//! a menu item added to the core is a golden image *missing* on the next run
//! rather than a screen nobody looks at. Since phase 11 every screen is drawn
//! twice: once from a board that knows nothing, once from one mid-session, so
//! that the empty-state rule -- a dash, never a plausible zero -- is in the
//! pictures as well as in the tests. Since phase 12 every editor is drawn
//! too, open on a standalone board, refused where a refusal can be reached,
//! and as the notice a host on the line turns it into.
//!
//! Since phase 13 there is a second set, under `128x64/`, of the same
//! screens on the other common panel: every screen populated, the long menu
//! windowed, a scrolled page, an editor, a notice and a pairing. That is
//! what proves the interface's layout is derived from the canvas rather
//! than assumed, and it is drawn from the same scenes and the same scripts.

use std::fs;
use std::path::{Path, PathBuf};

use oxinode_core::edit::Field;
use oxinode_core::screens::State;
use oxinode_core::ui::{Action, Screen};

use crate::image::Image;
use crate::panel::Size;
use crate::scene::{self, Scene};
use crate::script;

/// One state worth a picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Case {
    /// The file stem under the golden directory; the second panel's cases
    /// are under a `128x64/` subdirectory.
    pub name: String,
    /// How to get there from a fresh [`Scene`].
    pub script: String,
    /// What the screens draw from.
    pub state: State,
    /// The panel it is drawn on.
    pub size: Size,
}

impl Case {
    /// Render this case.
    pub fn render(&self) -> Image {
        let mut scene = self.play();
        Image::render(&scene.panel(self.size))
    }

    /// The scene at the end of this case's script.
    pub fn play(&self) -> Scene {
        let mut scene = Scene::with_state(self.state);
        let inputs = script::parse(&self.script).expect("a golden case's script parses");
        scene.run_on(self.size, &inputs);
        scene
    }

    /// The name without the panel subdirectory.
    pub fn stem(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

/// Every screen empty and populated, every item of every menu, a scrolled
/// page at each end and in the middle, and the two overlays that are not
/// screens: a pairing in progress, and a refused configuration. Then the
/// editors, and then the second panel.
pub fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let populated = scene::populated();
    let square = Size::SQUARE;
    for screen in Screen::ALL {
        let walk = format!("right*{}", screen.index());
        let title = slug(screen.title());
        cases.push(Case {
            name: title.clone(),
            script: walk.clone(),
            state: State::default(),
            size: square,
        });
        cases.push(Case {
            name: format!("{title}-populated"),
            script: walk.clone(),
            state: populated,
            size: square,
        });
        for (n, item) in screen.menu().iter().enumerate() {
            cases.push(Case {
                name: format!("{title}-menu-{}", slug(item.label)),
                script: format!("{walk} select down*{n}"),
                state: populated,
                size: square,
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
            size: square,
        });
    }
    cases.push(Case {
        name: "bluetooth-pairing".to_string(),
        script: format!("right*{}", Screen::Bluetooth.index()),
        state: scene::pairing(),
        size: square,
    });
    cases.push(Case {
        name: "radio-refused".to_string(),
        script: radio.clone(),
        state: scene::refused(),
        size: square,
    });
    // Phase 14: the receiver on and looking, which is neither of the two
    // states every screen is drawn in and the one the power question is
    // about.
    cases.push(Case {
        name: "position-searching".to_string(),
        script: format!("right*{}", Screen::Position.index()),
        state: scene::searching(),
        size: square,
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
            .position(|i| i.action == Some(Action::Edit(field)))
            .expect("every field has a menu item")
    };
    for field in Field::ALL {
        let open = format!("{radio} select down*{} select", item_of(field));
        cases.push(Case {
            name: format!("radio-edit-{}", slug(field.label())),
            script: open.clone(),
            state: standalone,
            size: square,
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
                size: square,
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
        size: square,
    });
    cases.push(Case {
        name: "radio-after-edit".to_string(),
        script: format!(
            "{radio} select down*{} select up select",
            item_of(Field::TxPower)
        ),
        state: standalone,
        size: square,
    });
    let toggle = Screen::Radio
        .menu()
        .iter()
        .position(|i| i.action == Some(Action::ToggleRadio))
        .expect("the toggle is on the menu");
    cases.push(Case {
        name: "radio-locked-edit".to_string(),
        script: format!("{radio} select down*{} select", item_of(Field::Frequency)),
        state: populated,
        size: square,
    });
    cases.push(Case {
        name: "radio-locked-toggle".to_string(),
        script: format!("{radio} select down*{toggle} select"),
        state: scene::phone(),
        size: square,
    });

    // Phase 13: the second panel. Every screen populated; the radio menu at
    // its top, in its middle and at its bottom, because nine items do not
    // fit and the window has to slide; the radio screen scrolled to its
    // end; an editor with its refusal; the notice; and a pairing, whose box
    // has to fit the shorter content area.
    let wide = Size::WIDE;
    let prefix = wide.name();
    for screen in Screen::ALL {
        cases.push(Case {
            name: format!("{prefix}/{}-populated", slug(screen.title())),
            script: format!("right*{}", screen.index()),
            state: populated,
            size: wide,
        });
    }
    for (name, downs) in [("top", 0), ("middle", 4), ("bottom", 8)] {
        cases.push(Case {
            name: format!("{prefix}/radio-menu-{name}"),
            script: format!("{radio} select down*{downs}"),
            state: populated,
            size: wide,
        });
    }
    cases.push(Case {
        name: format!("{prefix}/radio-scrolled-bottom"),
        script: format!("{radio} down*100"),
        state: populated,
        size: wide,
    });
    cases.push(Case {
        name: format!("{prefix}/radio-edit-tx-power-refused"),
        script: format!(
            "{radio} select down*{} select up*4 select",
            item_of(Field::TxPower)
        ),
        state: standalone,
        size: wide,
    });
    cases.push(Case {
        name: format!("{prefix}/radio-locked-edit"),
        script: format!("{radio} select down*{} select", item_of(Field::Frequency)),
        state: populated,
        size: wide,
    });
    cases.push(Case {
        name: format!("{prefix}/bluetooth-pairing"),
        script: format!("right*{}", Screen::Bluetooth.index()),
        state: scene::pairing(),
        size: wide,
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
    if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        return Verdict::Unreadable {
            reason: format!(
                "expected {}x{}, got {}x{}",
                actual.width(),
                actual.height(),
                expected.width(),
                expected.height()
            ),
        };
    }
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
    let path = dir.join(name);
    let parent = path.parent().unwrap_or(dir);
    fs::create_dir_all(parent).unwrap_or_else(|e| panic!("creating {}: {e}", parent.display()));
    fs::write(&path, image.to_png()).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use monopanel::Readable;
    use oxinode_core::ui::{self, NavExt};
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
        // cursor, the screen after an edit, and two notices. Then phase 13:
        // every screen, three menu windows, a scroll, an editor, a notice
        // and a pairing on the second panel. Then phase 14: the receiver
        // searching.
        let square = 2 * Screen::COUNT + items + 3 + 2 + Field::ALL.len() + 3 + 1 + 1 + 2 + 1;
        let wide = Screen::COUNT + 3 + 1 + 1 + 1 + 1;
        assert_eq!(cases.len(), square + wide);
        assert_eq!(cases.iter().filter(|c| c.size == Size::WIDE).count(), wide);
        for field in Field::ALL {
            assert!(names.contains(format!("radio-edit-{}", slug(field.label())).as_str()));
        }
        assert!(names.contains("radio-edit-tx-power-refused"));
        assert!(names.contains("radio-locked-edit"));
        for screen in Screen::ALL {
            let title = slug(screen.title());
            assert!(names.contains(title.as_str()), "{screen:?}");
            assert!(names.contains(format!("{title}-populated").as_str()));
            assert!(names.contains(format!("128x64/{title}-populated").as_str()));
            let empty = cases.iter().find(|c| c.name == title).unwrap();
            assert_eq!(empty.state, State::default(), "{screen:?} empty");
            for item in screen.menu() {
                let name = format!("{}-menu-{}", slug(screen.title()), slug(item.label));
                assert!(names.contains(name.as_str()), "{name}");
            }
        }
        // The second panel's cases are all under its directory, and only
        // they are.
        for case in &cases {
            assert_eq!(
                case.name.starts_with("128x64/"),
                case.size == Size::WIDE,
                "{}",
                case.name
            );
            assert!(!case.stem().contains('/'));
        }
    }

    /// Each case's script really lands on the state its name claims.
    #[test]
    fn every_case_lands_where_its_name_says() {
        for case in cases() {
            let scene = case.play();
            let screen = scene.nav.current();
            let stem = case.stem();
            assert!(
                stem.starts_with(&slug(screen.title())),
                "{} is on {screen:?}",
                case.name
            );
            match stem.split("-menu-").nth(1) {
                Some(item) => {
                    let selected = scene.nav.menu_item().expect("menu open");
                    // The second panel names its menu cases by position.
                    if case.size == Size::SQUARE {
                        assert_eq!(slug(screen.menu()[selected].label), item, "{}", case.name);
                    }
                }
                None => assert!(!scene.nav.menu_is_open(), "{}", case.name),
            }
            // An editor case has its editor open on the field it names,
            // refused exactly when the name says so; a locked case shows
            // the notice; anything else is a plain screen.
            if let Some(rest) = stem.strip_prefix("radio-edit-") {
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
            } else if stem.starts_with("radio-locked-") {
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
        let scene = after.play();
        assert_ne!(scene.state.radio.config, scene::standalone().radio.config);
        let mut lines = oxinode_core::screens::Lines::new();
        scene.state.lines(Screen::Radio, &mut lines);
        assert!((0..lines.len()).any(|n| lines.get(n).unwrap().ends_with("18 dBm")));
        // The scrolled cases really scroll: the bottom is past the middle,
        // and the middle is past the top.
        let scroll_of = |name: &str| {
            let case = cases().into_iter().find(|c| c.name == name).unwrap();
            let mut scene = case.play();
            scene.panel(case.size);
            scene.nav.scroll()
        };
        assert_eq!(scroll_of("radio-scrolled-top"), 0);
        assert!(scroll_of("radio-scrolled-middle") > 0);
        assert!(scroll_of("radio-scrolled-bottom") > scroll_of("radio-scrolled-middle"));
        let mut lines = oxinode_core::screens::Lines::new();
        scene::populated().lines(Screen::Radio, &mut lines);
        assert_eq!(
            scroll_of("radio-scrolled-bottom"),
            lines.len() - ui::LAYOUT.visible_lines()
        );
        // And on the short panel the bottom is further down, because fewer
        // lines fit.
        assert_eq!(
            scroll_of("128x64/radio-scrolled-bottom"),
            lines.len() - monopanel::Layout::of(128, 64).visible_lines()
        );
        assert!(scroll_of("128x64/radio-scrolled-bottom") > scroll_of("radio-scrolled-bottom"));
    }

    /// The second panel's menu cases show different windows of the same
    /// menu, and each has its highlight on the panel.
    #[test]
    fn the_short_panels_menu_windows_slide() {
        let windows: Vec<(usize, std::ops::Range<usize>)> = ["top", "middle", "bottom"]
            .iter()
            .map(|pos| {
                let case = cases()
                    .into_iter()
                    .find(|c| c.name == format!("128x64/radio-menu-{pos}"))
                    .unwrap();
                let scene = case.play();
                let selected = scene.nav.menu_item().expect("menu open");
                let layout = monopanel::Layout::of(128, 64);
                (
                    selected,
                    monopanel::menu_window(&layout, Screen::Radio.menu().len(), selected),
                )
            })
            .collect();
        assert_eq!(windows[0].0, 0);
        assert_eq!(windows[2].0, Screen::Radio.menu().len() - 1);
        for (selected, window) in &windows {
            assert!(window.contains(selected));
            assert!(window.len() < Screen::Radio.menu().len(), "windowed");
        }
        assert_ne!(windows[0].1, windows[1].1);
        assert_ne!(windows[1].1, windows[2].1);
    }

    /// The second panel's pictures are the second panel's size, and every
    /// one draws inside it: nothing below the icon strip, which on a 128 x
    /// 64 is where a hard-coded 128 x 128 layout would have put things.
    #[test]
    fn the_short_panel_is_drawn_at_its_own_size() {
        let layout = monopanel::Layout::of(128, 64);
        for case in cases().into_iter().filter(|c| c.size == Size::WIDE) {
            let image = case.render();
            assert_eq!((image.width(), image.height()), (512, 256), "{}", case.name);
            let panel = case.play().panel(Size::WIDE);
            // The strip's hairline is the top row of the bar, and it is
            // solid: nothing is drawn over the chrome.
            let hairline = (0..128)
                .filter(|&x| panel.pixel(x, 64 - layout.bar_h))
                .count();
            assert_eq!(hairline, 128, "{}: the strip's hairline", case.name);
            assert!(
                (0..128).all(|x| panel.pixel(x, 0)),
                "{}: the title bar",
                case.name
            );
        }
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
    /// out; a changed one is reported with a diff; one of the wrong size is
    /// reported as unreadable.
    #[test]
    fn the_check_matches_what_update_wrote_and_reports_what_it_did_not() {
        let golden = scratch("golden");
        let diff = scratch("diff");
        let written = update(&golden);
        assert_eq!(written.len(), cases().len());
        assert!(written.iter().all(|p| p.exists()));
        assert!(
            golden.join("128x64").is_dir(),
            "the second panel's directory"
        );

        let verdicts = check(&golden, &diff);
        assert!(
            verdicts.iter().all(|(_, v)| *v == Verdict::Match),
            "{verdicts:?}"
        );
        assert!(!diff.exists(), "nothing to report, nothing written");

        // Remove one, corrupt one, alter one, and swap one for the other
        // panel's.
        let all = cases();
        let first = &all[0];
        fs::remove_file(golden.join(format!("{}.png", first.name))).unwrap();
        let second = &all[1];
        fs::write(golden.join(format!("{}.png", second.name)), b"nope").unwrap();
        let third = &all[2];
        let mut altered = all[3].render();
        assert_ne!(altered, third.render());
        altered = Image::diff(&altered, &third.render()).unwrap();
        fs::write(golden.join(format!("{}.png", third.name)), altered.to_png()).unwrap();
        let wide = all.iter().position(|c| c.size == Size::WIDE).unwrap();
        fs::write(
            golden.join(format!("{}.png", all[wide].name)),
            all[4].render().to_png(),
        )
        .unwrap();

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
        match &verdicts[wide].1 {
            Verdict::Unreadable { reason } => assert!(reason.contains("512x256"), "{reason}"),
            other => panic!("{other:?}"),
        }
        for (n, (_, v)) in verdicts.iter().enumerate() {
            if ![0, 1, 2, wide].contains(&n) {
                assert_eq!(*v, Verdict::Match, "case {n}");
            }
        }

        let _ = fs::remove_dir_all(golden);
        let _ = fs::remove_dir_all(diff);
    }
}
