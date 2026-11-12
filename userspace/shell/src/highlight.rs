//! Colours for one line of an AXIS script: what an editor needs to show
//! comments, strings, numbers, `$variables`, the block keywords and the
//! command a line runs.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Comment,
    String,
    Number,
    /// `if`, `else`, `for`, `in`.
    Keyword,
    /// The first word of a command, also after `|`.
    Command,
    /// `$name`, `$1`, `$?`, `$#`, `$@`.
    Variable,
    Operator,
}

const KEYWORDS: [&str; 4] = ["if", "else", "for", "in"];

/// The coloured stretches of `line`: `(first column, end column, kind)`
/// with columns counted in characters, in order and not overlapping. What
/// lies between them is plain text.
pub fn highlight(line: &str) -> Vec<(usize, usize, Kind)> {
    let c: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    // The next word is a command: at the start of the line and after `|`,
    // unless the line turns out to be an assignment or a block statement.
    let mut command_next = true;
    let assignment = {
        let t = line.trim_start();
        let name_end = t.find(|ch: char| !(ch.is_alphanumeric() || ch == '_')).unwrap_or(t.len());
        name_end > 0 && t[name_end..].trim_start().starts_with('=') && !t[name_end..].trim_start().starts_with("==")
    };
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        let start = i;
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '#' {
            out.push((i, c.len(), Kind::Comment));
            break;
        } else if ch == '"' || ch == '\'' {
            i += 1;
            while i < c.len() && c[i] != ch {
                i += 1;
            }
            i = (i + 1).min(c.len());
            out.push((start, i, Kind::String));
            command_next = false;
        } else if ch == '$' {
            i += 1;
            if i < c.len() && matches!(c[i], '?' | '#' | '@') || i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            } else {
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
            }
            if i > start + 1 {
                out.push((start, i, Kind::Variable));
            }
            command_next = false;
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.' || c[i] == '_') {
                i += 1;
            }
            // `2nd` or `3.txt` is a word, not a number.
            if c[start..i].iter().all(|d| d.is_ascii_digit() || *d == '.') && !command_next {
                out.push((start, i, Kind::Number));
            }
            command_next = false;
        } else if ch.is_alphabetic() || ch == '_' || ch == '/' || ch == '.' {
            while i < c.len() && !(c[i].is_whitespace() || matches!(c[i], '|' | '<' | '>' | '=' | '(' | ')' | '{' | '}' | ',' | '"' | '\'' | '#' | '$')) {
                i += 1;
            }
            let word: alloc::string::String = c[start..i].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                out.push((start, i, Kind::Keyword));
                // `for name in words`: neither is a command. The condition
                // of `if` is an expression.
                command_next = false;
            } else if command_next && !assignment {
                // `print(...)` is a statement too.
                out.push((start, i, Kind::Command));
                command_next = false;
            } else {
                command_next = false;
            }
        } else {
            i += 1;
            if matches!(ch, '|' | '<' | '>' | '=' | '!' | '+' | '-' | '*' | '(' | ')' | '{' | '}' | ',') {
                // Runs of operator characters are one stretch.
                match out.last_mut() {
                    Some((_, end, Kind::Operator)) if *end == start => *end = i,
                    _ => out.push((start, i, Kind::Operator)),
                }
            }
            if ch == '|' || ch == '{' {
                command_next = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    /// The coloured words of `line` with their kinds.
    fn parts(line: &str) -> Vec<(String, Kind)> {
        let c: Vec<char> = line.chars().collect();
        highlight(line).into_iter().map(|(a, b, k)| (c[a..b].iter().collect(), k)).collect()
    }

    fn s(text: &str, k: Kind) -> (String, Kind) {
        (String::from(text), k)
    }

    #[test]
    fn commands_strings_variables_and_comments() {
        assert_eq!(
            parts("echo \"hi $name\" $1 > out.txt # done"),
            [s("echo", Kind::Command), s("\"hi $name\"", Kind::String), s("$1", Kind::Variable), s(">", Kind::Operator), s("# done", Kind::Comment)]
        );
        assert_eq!(parts("ls -l /osyx-seed | cat"), [s("ls", Kind::Command), s("-", Kind::Operator), s("|", Kind::Operator), s("cat", Kind::Command)]);
        assert_eq!(parts("  # only a comment"), [s("# only a comment", Kind::Comment)]);
        assert_eq!(parts("/bin/worker exit 3"), [s("/bin/worker", Kind::Command), s("3", Kind::Number)]);
    }

    #[test]
    fn statements_are_not_commands() {
        assert_eq!(parts("count = 2 + 3"), [s("=", Kind::Operator), s("2", Kind::Number), s("+", Kind::Operator), s("3", Kind::Number)]);
        assert_eq!(parts("name = \"OSyx\""), [s("=", Kind::Operator), s("\"OSyx\"", Kind::String)]);
        assert_eq!(
            parts("for a in $@ {"),
            [s("for", Kind::Keyword), s("in", Kind::Keyword), s("$@", Kind::Variable), s("{", Kind::Operator)]
        );
        assert_eq!(
            parts("if $# == 2 { echo two }"),
            [
                s("if", Kind::Keyword),
                s("$#", Kind::Variable),
                s("==", Kind::Operator),
                s("2", Kind::Number),
                s("{", Kind::Operator),
                s("echo", Kind::Command),
                s("}", Kind::Operator)
            ]
        );
        assert_eq!(parts("} else {"), [s("}", Kind::Operator), s("else", Kind::Keyword), s("{", Kind::Operator)]);
    }

    #[test]
    fn columns_count_characters_and_nothing_overlaps() {
        let line = "echo \"zażółć\" $gęś # ąę";
        let spans = highlight(line);
        assert_eq!(spans[1], (5, 13, Kind::String));
        assert_eq!(spans[2], (14, 18, Kind::Variable));
        let n = line.chars().count();
        let mut at = 0;
        for (a, b, _) in spans {
            assert!(at <= a && a < b && b <= n);
            at = b;
        }
        // An open quote colours the rest of the line; odd input does not panic.
        assert_eq!(parts("echo \"open"), [s("echo", Kind::Command), s("\"open", Kind::String)]);
        for odd in ["", "$", "$$$", "\"", "|||", "= =", "{{{}}}", "1.2.3", "é"] {
            let _ = highlight(odd);
        }
    }
}
