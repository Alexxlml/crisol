//! Boxed tables and coloured marks for a terminal (ADR-0028). Pure drawing: it takes the rows
//! and a width and gives text, so the tests need no terminal.

use std::fmt::Write as _;

/// The narrowest window that still gets a table; below it the list form is used.
pub const MIN_TABLE_WIDTH: usize = 60;
/// The least room the last column needs to be worth a table.
const MIN_LAST_COLUMN: usize = 12;

/// How a piece of text is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// The terminal's own colour.
    #[default]
    Plain,
    /// Something is fine.
    Good,
    /// Something deserves a look.
    Warn,
    /// Something is wrong.
    Bad,
    /// Less important.
    Dim,
    /// A heading.
    Bold,
}

impl Tone {
    fn code(self) -> Option<&'static str> {
        match self {
            Tone::Plain => None,
            Tone::Good => Some("32"),
            Tone::Warn => Some("33"),
            Tone::Bad => Some("31"),
            Tone::Dim => Some("2"),
            Tone::Bold => Some("1"),
        }
    }
}

/// `text` painted in `tone` (the text itself when it has no colour).
#[must_use]
pub fn paint(text: &str, tone: Tone) -> String {
    match tone.code() {
        Some(code) if !text.is_empty() => format!("\x1b[{code}m{text}\x1b[0m"),
        _ => text.to_owned(),
    }
}

/// One cell: its text (lines separated by `\n`) and how it is painted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub tone: Tone,
}

impl Cell {
    /// A cell in the terminal's colour.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Plain,
        }
    }

    /// A cell painted in `tone`.
    #[must_use]
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// Splits `text` into lines of at most `width` characters, breaking at spaces; a word longer
/// than a line is cut without losing a character. Newlines in `text` are kept.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut len = 0;
        for word in paragraph.split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            loop {
                let room = if len == 0 {
                    width
                } else {
                    width.saturating_sub(len + 1)
                };
                if word.len() <= room {
                    if len > 0 {
                        line.push(' ');
                        len += 1;
                    }
                    len += word.len();
                    line.extend(word.iter());
                    break;
                }
                if len > 0 {
                    // Start the word on a fresh line when it fits there whole.
                    lines.push(std::mem::take(&mut line));
                    len = 0;
                    continue;
                }
                // Longer than a whole line: cut it.
                let rest = word.split_off(width);
                lines.push(word.iter().collect());
                word = rest;
            }
        }
        lines.push(line);
    }
    lines
}

/// Draws a table that fits in `width` characters, or `None` when it cannot (the window is
/// narrower than [`MIN_TABLE_WIDTH`], or the first columns leave too little for the last).
///
/// The last column takes whatever room is left and wraps; the others are as wide as their
/// longest line. With `color` off the borders and text are the same, just unpainted.
#[must_use]
pub fn draw(headers: &[&str], rows: &[Vec<Cell>], width: usize, color: bool) -> Option<String> {
    let n = headers.len();
    if n == 0 || width < MIN_TABLE_WIDTH {
        return None;
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(n - 1) {
            let longest = cell.text.split('\n').map(|l| l.chars().count()).max();
            widths[i] = widths[i].max(longest.unwrap_or(0));
        }
    }
    // "│ " + cells joined by " │ " + " │"
    let fixed = 3 * n + 1;
    let others: usize = widths[..n - 1].iter().sum();
    let last = width.checked_sub(fixed + others)?;
    if last < MIN_LAST_COLUMN {
        return None;
    }
    let natural = rows
        .iter()
        .filter_map(|r| r.get(n - 1))
        .flat_map(|c| c.text.split('\n').map(|l| l.chars().count()))
        .chain(std::iter::once(headers[n - 1].chars().count()))
        .max()
        .unwrap_or(0);
    widths[n - 1] = natural.min(last);

    let rule = |left: &str, mid: &str, right: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{left}{}{right}\n", parts.join(mid))
    };
    let mut out = rule("┌", "┬", "┐");
    let head: Vec<Cell> = headers.iter().map(|h| Cell::new(*h, Tone::Bold)).collect();
    push_row(&mut out, &head, &widths, color);
    out.push_str(&rule("├", "┼", "┤"));
    for row in rows {
        push_row(&mut out, row, &widths, color);
    }
    out.push_str(&rule("└", "┴", "┘"));
    Some(out)
}

fn push_row(out: &mut String, row: &[Cell], widths: &[usize], color: bool) {
    let wrapped: Vec<Vec<String>> = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let text = row.get(i).map_or("", |c| c.text.as_str());
            if i + 1 == widths.len() {
                wrap(text, *w)
            } else {
                text.split('\n').map(str::to_owned).collect()
            }
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    for line in 0..height {
        out.push('│');
        for (i, w) in widths.iter().enumerate() {
            let text = wrapped[i].get(line).map_or("", String::as_str);
            let pad = w - text.chars().count();
            let tone = row.get(i).map_or(Tone::Plain, |c| c.tone);
            let shown = if color {
                paint(text, tone)
            } else {
                text.to_owned()
            };
            let _ = write!(out, " {shown}{} │", " ".repeat(pad));
        }
        out.push('\n');
    }
}
