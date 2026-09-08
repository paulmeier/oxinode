//! `oxinode-sim`: look at the panel without a board.
//!
//! ```text
//! oxinode-sim render [--script S] [--state F] [--text] -o out.png
//! oxinode-sim steps  [--script S] [--state F] --out DIR
//! oxinode-sim tty    [--script S] [--state F] [--braille | --half]
//! oxinode-sim golden [--update] [--dir DIR] [--diff DIR]
//! oxinode-sim raw    FILE -o out.png
//! ```
//!
//! No argument-parsing crate: five subcommands with a handful of flags each
//! is less code by hand than the derive attributes would be, and it keeps
//! the crate's one dependency the one that earns it.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use oxinode_core::sh1107;
use oxinode_sim::golden::{self, Verdict};
use oxinode_sim::image::{frame_from_bytes, Image};
use oxinode_sim::scene::{Fixture, Scene};
use oxinode_sim::script;
use oxinode_sim::text::{self, Cells};
use oxinode_sim::tty;

const USAGE: &str = "\
usage: oxinode-sim <command> [options]

  render [--script S] [--state F] [--text] -o FILE.png
      Run a script from a fresh panel and write the final frame as a PNG at
      4x with a pixel grid; --text prints it as braille instead.
  steps  [--script S] [--state F] --out DIR
      The same, writing one PNG per step: 00-start.png, 01-right.png, ...
  tty    [--script S] [--state F] [--braille | --half]
      Drive the real menu tree with the arrow keys, in the terminal.
  golden [--update] [--dir DIR] [--diff DIR]
      Compare every screen and menu against the committed images (default
      sim/golden), writing diffs to --diff (default target/golden-diff).
      --update rewrites the committed images instead.
  raw    FILE -o FILE.png
      Render a 2048-byte controller-layout framebuffer dump.

A script is key names separated by spaces: left right up down select back,
with word*N for repeats and # for comments. --state is the fixture the
screens draw from: `populated` (the default) is a board mid-session with a
host on the line, `empty` one that knows nothing yet, and `standalone` a TNC
with no host attached -- the one whose settings the panel may change.
";

/// What the command line asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Command {
    Render {
        script: String,
        state: Fixture,
        text: bool,
        out: Option<PathBuf>,
    },
    Steps {
        script: String,
        state: Fixture,
        out: PathBuf,
    },
    Tty {
        script: String,
        state: Fixture,
        cells: Option<Cells>,
    },
    Golden {
        update: bool,
        dir: PathBuf,
        diff: PathBuf,
    },
    Raw {
        file: PathBuf,
        out: PathBuf,
    },
    Help,
}

/// Where the golden images live, relative to the crate, wherever it is run
/// from.
fn default_golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden")
}

fn default_diff_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("golden-diff")
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let Some(command) = args.first() else {
        return Ok(Command::Help);
    };
    let mut script = String::new();
    let mut state = Fixture::Populated;
    let mut text = false;
    let mut out = None;
    let mut update = false;
    let mut dir = None;
    let mut diff = None;
    let mut cells = None;
    let mut positional = Vec::new();

    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--script" => script = value("--script")?,
            "--state" => {
                let v = value("--state")?;
                state = Fixture::named(&v).ok_or_else(|| {
                    format!("--state: `{v}` is not `empty`, `populated` or `standalone`")
                })?;
            }
            "--text" => text = true,
            "-o" | "--out" => out = Some(PathBuf::from(value("-o")?)),
            "--update" => update = true,
            "--dir" => dir = Some(PathBuf::from(value("--dir")?)),
            "--diff" => diff = Some(PathBuf::from(value("--diff")?)),
            "--braille" => cells = Some(Cells::Braille),
            "--half" => cells = Some(Cells::HalfBlocks),
            "-h" | "--help" => return Ok(Command::Help),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => positional.push(PathBuf::from(other)),
        }
    }
    // Fail on a bad script before doing anything with it.
    script::parse(&script).map_err(|e| format!("--script: {e}"))?;

    Ok(match command.as_str() {
        "render" => {
            if out.is_none() && !text {
                return Err("render: -o FILE.png or --text is required".into());
            }
            Command::Render {
                script,
                state,
                text,
                out,
            }
        }
        "steps" => Command::Steps {
            script,
            state,
            out: out.ok_or("steps: --out DIR is required")?,
        },
        "tty" => Command::Tty {
            script,
            state,
            cells,
        },
        "golden" => Command::Golden {
            update,
            dir: dir.unwrap_or_else(default_golden_dir),
            diff: diff.unwrap_or_else(default_diff_dir),
        },
        "raw" => Command::Raw {
            file: positional
                .first()
                .cloned()
                .ok_or("raw: a FILE is required")?,
            out: out.ok_or("raw: -o FILE.png is required")?,
        },
        "help" | "-h" | "--help" => Command::Help,
        other => return Err(format!("unknown command {other}")),
    })
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Render {
            script,
            state,
            text,
            out,
        } => {
            let mut scene = Scene::with_state(state.state());
            let actions = scene.run(&script::parse(&script).unwrap());
            for action in actions {
                eprintln!("action: {action:?}");
            }
            let frame = scene.frame();
            if text {
                for line in text::render(&frame, Cells::Braille) {
                    println!("{line}");
                }
            }
            if let Some(out) = out {
                write_png(&out, &Image::render(&frame))?;
                println!("{}", out.display());
            }
            Ok(())
        }
        Command::Steps { script, state, out } => {
            let mut scene = Scene::with_state(state.state());
            let inputs = script::parse(&script).unwrap();
            fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            let path = out.join("00-start.png");
            write_png(&path, &Image::render(&scene.frame()))?;
            println!("{}", path.display());
            for (n, input) in inputs.iter().enumerate() {
                let action = scene.press(*input);
                let path = out.join(format!("{:02}-{}.png", n + 1, script::name_of(*input)));
                write_png(&path, &Image::render(&scene.frame()))?;
                match action {
                    Some(action) => println!("{}  action: {action:?}", path.display()),
                    None => println!("{}", path.display()),
                }
            }
            Ok(())
        }
        Command::Tty {
            script,
            state,
            cells,
        } => {
            let mut scene = Scene::with_state(state.state());
            scene.run(&script::parse(&script).unwrap());
            tty::run(scene, cells).map_err(|e| e.to_string())
        }
        Command::Golden { update, dir, diff } => {
            if update {
                for path in golden::update(&dir) {
                    println!("wrote {}", path.display());
                }
                return Ok(());
            }
            let verdicts = golden::check(&dir, &diff);
            let mut failed = 0;
            for (case, verdict) in &verdicts {
                match verdict {
                    Verdict::Match => println!("ok       {}", case.name),
                    Verdict::Missing { actual } => {
                        failed += 1;
                        println!("MISSING  {}  (rendered to {})", case.name, actual.display());
                    }
                    Verdict::Unreadable { reason } => {
                        failed += 1;
                        println!("BAD      {}  ({reason})", case.name);
                    }
                    Verdict::Differs {
                        pixels,
                        actual,
                        diff,
                    } => {
                        failed += 1;
                        println!(
                            "DIFFERS  {}  {pixels} pixels; see {} and {}",
                            case.name,
                            diff.display(),
                            actual.display()
                        );
                    }
                }
            }
            if failed > 0 {
                return Err(format!(
                    "{failed} of {} golden images do not match; if the change is intended, \
                     run `oxinode-sim golden --update` and commit the result",
                    verdicts.len()
                ));
            }
            println!("{} golden images match", verdicts.len());
            Ok(())
        }
        Command::Raw { file, out } => {
            let bytes = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let bytes: &[u8; sh1107::BUFFER_LEN] = bytes.as_slice().try_into().map_err(|_| {
                format!(
                    "{}: expected {} bytes, got {}",
                    file.display(),
                    sh1107::BUFFER_LEN,
                    bytes.len()
                )
            })?;
            write_png(&out, &Image::render(&frame_from_bytes(bytes)))?;
            println!("{}", out.display());
            Ok(())
        }
    }
}

fn write_png(path: &std::path::Path, image: &Image) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    fs::write(path, image.to_png()).map_err(|e| format!("{}: {e}", path.display()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse_args(&args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("oxinode-sim: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("oxinode-sim: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Command, String> {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        parse_args(&args)
    }

    #[test]
    fn no_arguments_is_help() {
        assert_eq!(parse("").unwrap(), Command::Help);
        assert_eq!(parse("help").unwrap(), Command::Help);
        assert_eq!(parse("render --help").unwrap(), Command::Help);
    }

    #[test]
    fn render_needs_somewhere_to_go() {
        assert!(parse("render").unwrap_err().contains("-o"));
        assert_eq!(
            parse("render --script right --state empty -o x.png").unwrap(),
            Command::Render {
                script: "right".into(),
                state: Fixture::Empty,
                text: false,
                out: Some("x.png".into()),
            }
        );
        assert!(matches!(
            parse("render --text").unwrap(),
            Command::Render {
                text: true,
                out: None,
                ..
            }
        ));
    }

    #[test]
    fn a_bad_script_is_refused_up_front() {
        let err = parse("render --script sideways -o x.png").unwrap_err();
        assert!(err.contains("sideways"), "{err}");
        assert!(parse("steps --script right").unwrap_err().contains("--out"));
        assert!(parse("render --state full -o x.png")
            .unwrap_err()
            .contains("standalone"));
        assert!(matches!(
            parse("render --state standalone -o x.png").unwrap(),
            Command::Render {
                state: Fixture::Standalone,
                ..
            }
        ));
        assert!(parse("render --script").unwrap_err().contains("value"));
        assert!(parse("render --bogus -o x")
            .unwrap_err()
            .contains("--bogus"));
        assert!(parse("fly").unwrap_err().contains("fly"));
    }

    #[test]
    fn golden_defaults_to_the_committed_directory() {
        match parse("golden").unwrap() {
            Command::Golden { update, dir, diff } => {
                assert!(!update);
                assert!(dir.ends_with("golden"));
                assert!(diff.ends_with("golden-diff"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse("golden --update --dir /tmp/g --diff /tmp/d").unwrap(),
            Command::Golden { update: true, .. }
        ));
    }

    #[test]
    fn tty_and_raw() {
        assert!(matches!(
            parse("tty --half").unwrap(),
            Command::Tty {
                cells: Some(Cells::HalfBlocks),
                ..
            }
        ));
        assert!(matches!(
            parse("tty").unwrap(),
            Command::Tty {
                cells: None,
                state: Fixture::Populated,
                ..
            }
        ));
        assert_eq!(
            parse("raw dump.bin -o out.png").unwrap(),
            Command::Raw {
                file: "dump.bin".into(),
                out: "out.png".into()
            }
        );
        assert!(parse("raw -o out.png").unwrap_err().contains("FILE"));
    }
}
