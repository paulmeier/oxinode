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
//! rather than a screen nobody looks at.

use std::fs;
use std::path::{Path, PathBuf};

use oxinode_core::ui::{self, Screen};

use crate::image::Image;
use crate::scene::Scene;
use crate::script;

/// One state worth a picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Case {
    /// The file stem under the golden directory.
    pub name: String,
    /// How to get there from a fresh [`Scene`].
    pub script: String,
    /// How many sample lines the screen shows; zero for the bare chrome.
    pub sample_lines: usize,
}

impl Case {
    /// Render this case.
    pub fn render(&self) -> Image {
        let mut scene = Scene::new().with_sample_lines(self.sample_lines);
        let inputs = script::parse(&self.script).expect("a golden case's script parses");
        scene.run(&inputs);
        Image::render(&scene.frame())
    }
}

/// Every screen, every menu, every menu selection, and a scrolled page at
/// each end and in the middle.
pub fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for screen in Screen::ALL {
        let walk = format!("right*{}", screen.index());
        let title = slug(screen.title());
        cases.push(Case {
            name: title.clone(),
            script: walk.clone(),
            sample_lines: 0,
        });
        for (n, item) in screen.menu().iter().enumerate() {
            cases.push(Case {
                name: format!("{title}-menu-{}", slug(item.label)),
                script: format!("{walk} select down*{n}"),
                sample_lines: 0,
            });
        }
    }
    let long = ui::visible_lines() + 6;
    for (name, downs) in [("top", 0), ("middle", 3), ("bottom", long)] {
        cases.push(Case {
            name: format!("home-scrolled-{name}"),
            script: format!("down*{downs}"),
            sample_lines: long,
        });
    }
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
    use std::collections::HashSet;

    /// The case list is the screen list: a picture of every screen, of every
    /// item of every menu, and nothing twice.
    #[test]
    fn the_cases_cover_every_screen_and_every_menu_item() {
        let cases = cases();
        let names: HashSet<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names.len(), cases.len(), "a name repeats");
        let items: usize = Screen::ALL.iter().map(|s| s.menu().len()).sum();
        assert_eq!(cases.len(), Screen::COUNT + items + 3);
        for screen in Screen::ALL {
            assert!(names.contains(slug(screen.title()).as_str()), "{screen:?}");
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
            let mut scene = Scene::new().with_sample_lines(case.sample_lines);
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
        }
        let mut bottom = Scene::new().with_sample_lines(ui::visible_lines() + 6);
        bottom.run(&script::parse("down*100").unwrap());
        assert_eq!(bottom.nav.scroll(), 6);
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
