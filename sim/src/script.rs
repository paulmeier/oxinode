//! Input scripts: `right right select down select`.
//!
//! A fixture is a path through the menus, and a path is best written as the
//! keys you would press. One word per press, whitespace between, `#` to the
//! end of a line for a comment, and `word*3` for a repeat -- because
//! `down down down down down` is easy to miscount and `down*5` is not.

use oxinode_core::ui::Input;

/// A word that is not a key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScriptError {
    /// The offending word.
    pub word: String,
    /// Which word it was, counting from one.
    pub position: usize,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "word {}: `{}` is not a key (left right up down select back, or word*N)",
            self.position, self.word
        )
    }
}

impl std::error::Error for ScriptError {}

/// Parse a script into the inputs it presses, in order.
pub fn parse(script: &str) -> Result<Vec<Input>, ScriptError> {
    let mut inputs = Vec::new();
    let words = script
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace);
    for (n, word) in words.enumerate() {
        let error = || ScriptError {
            word: word.to_string(),
            position: n + 1,
        };
        let (key, count) = match word.split_once('*') {
            Some((key, count)) => (key, count.parse::<usize>().map_err(|_| error())?),
            None => (word, 1),
        };
        let input = key_named(key).ok_or_else(error)?;
        inputs.extend(std::iter::repeat_n(input, count));
    }
    Ok(inputs)
}

/// The input a word names, if it names one.
///
/// Full names first; the single letters are for typing at a prompt, and the
/// synonyms for `select` are the ones people reach for.
pub fn key_named(word: &str) -> Option<Input> {
    Some(match word.to_ascii_lowercase().as_str() {
        "left" | "l" => Input::Left,
        "right" | "r" => Input::Right,
        "up" | "u" => Input::Up,
        "down" | "d" => Input::Down,
        "select" | "ok" | "enter" | "s" => Input::Select,
        "back" | "b" | "esc" => Input::Back,
        _ => return None,
    })
}

/// The word for an input, as [`parse`] reads it. Used to name dumped frames.
pub fn name_of(input: Input) -> &'static str {
    input.name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_read_in_order() {
        assert_eq!(
            parse("right right select down select").unwrap(),
            [
                Input::Right,
                Input::Right,
                Input::Select,
                Input::Down,
                Input::Select
            ]
        );
    }

    #[test]
    fn an_empty_script_presses_nothing() {
        assert_eq!(parse("").unwrap(), []);
        assert_eq!(parse("   \n\t # only a comment\n").unwrap(), []);
    }

    #[test]
    fn comments_run_to_the_end_of_the_line() {
        assert_eq!(
            parse("right # to Radio\nselect # open its menu\n").unwrap(),
            [Input::Right, Input::Select]
        );
    }

    #[test]
    fn repeats_and_case_and_synonyms() {
        assert_eq!(
            parse("Down*3 OK esc").unwrap(),
            [
                Input::Down,
                Input::Down,
                Input::Down,
                Input::Select,
                Input::Back
            ]
        );
        assert_eq!(parse("l r u d s b").unwrap().len(), 6);
        assert_eq!(parse("left*0").unwrap(), []);
    }

    #[test]
    fn a_wrong_word_is_named_and_placed() {
        let err = parse("right sideways select").unwrap_err();
        assert_eq!(err.word, "sideways");
        assert_eq!(err.position, 2);
        assert!(err.to_string().contains("sideways"));
        assert_eq!(parse("down*many").unwrap_err().word, "down*many");
        assert_eq!(parse("*3").unwrap_err().position, 1);
    }

    /// Every input has a name, and every name parses back to it.
    #[test]
    fn names_round_trip() {
        for input in [
            Input::Left,
            Input::Right,
            Input::Up,
            Input::Down,
            Input::Select,
            Input::Back,
        ] {
            assert_eq!(parse(name_of(input)).unwrap(), [input]);
        }
    }
}
