//! Terminal interpretation and viewport snapshots stay in rexetermd.
use avt::{Cell, Color, Pen, Terminal};
use std::fmt::Write as _;

pub(super) const MAX_COLS: u16 = 300;
pub(super) const MAX_ROWS: u16 = 120;

pub(super) struct Screen {
    terminal: Terminal,
    incomplete_utf8: Vec<u8>,
}

/// An owned viewport lets PNG encoding run without holding the task's lock.
pub(super) struct Viewport {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<Cell>>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = dimensions(cols, rows);
        Self {
            terminal: Terminal::new(cols, rows).expect("non-zero dimensions"),
            incomplete_utf8: Vec::new(),
        }
    }

    pub fn size(&self) -> (usize, usize) {
        self.terminal.size()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = dimensions(cols, rows);
        self.terminal.resize(cols, rows);
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // Binary frames can end inside a UTF-8 character or escape sequence.
        // avt keeps its parser state; only incomplete UTF-8 needs reassembly.
        let mut joined;
        let mut bytes = if self.incomplete_utf8.is_empty() {
            bytes
        } else {
            joined = std::mem::take(&mut self.incomplete_utf8);
            joined.extend_from_slice(bytes);
            joined.as_slice()
        };
        loop {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    self.terminal.feed(text);
                    break;
                }
                Err(error) => {
                    let (valid, tail) = bytes.split_at(error.valid_up_to());
                    self.terminal.feed(std::str::from_utf8(valid).unwrap());
                    let Some(invalid) = error.error_len() else {
                        self.incomplete_utf8.extend_from_slice(tail);
                        break;
                    };
                    self.terminal.feed('�');
                    bytes = &tail[invalid..];
                }
            }
        }
    }

    pub fn finish(&mut self) {
        if !self.incomplete_utf8.is_empty() {
            self.terminal.feed('�');
            self.incomplete_utf8.clear();
        }
    }

    pub fn viewport(&self) -> Viewport {
        let (cols, rows) = self.size();
        Viewport {
            cols,
            rows,
            cells: self
                .terminal
                .screen()
                .visible_lines()
                .map(|line| line.cells().to_vec())
                .collect(),
        }
    }
}

fn dimensions(cols: u16, rows: u16) -> (usize, usize) {
    (
        cols.clamp(1, MAX_COLS).into(),
        rows.clamp(1, MAX_ROWS).into(),
    )
}

impl Viewport {
    pub fn text(&self) -> String {
        let mut lines: Vec<_> = self
            .cells
            .iter()
            .map(|line| {
                line.iter()
                    .filter(|cell| cell.width() != 0)
                    .map(Cell::char)
                    .collect::<String>()
            })
            .collect();
        for line in &mut lines {
            line.truncate(line.trim_end().len());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Print the painted cells using only SGR and line breaks. Remote escape
    /// sequences are interpreted by the daemon, never forwarded to the viewer.
    pub fn ansi(&self) -> String {
        let mut result = String::from("\x1b[0m");
        let last = self.cells.iter().rposition(|line| line.iter().any(painted));
        for line in self.cells.iter().take(last.map_or(0, |index| index + 1)) {
            let mut pen = Pen::default();
            if let Some(last) = line.iter().rposition(painted) {
                for cell in line[..=last].iter().filter(|cell| cell.width() != 0) {
                    if cell.pen() != &pen {
                        pen = *cell.pen();
                        sgr(&mut result, pen);
                    }
                    let ch = cell.char();
                    result.push(if ch.is_control() { '�' } else { ch });
                }
            }
            if !pen.is_default() {
                result.push_str("\x1b[0m");
            }
            result.push('\n');
        }
        result
    }
}

fn painted(cell: &Cell) -> bool {
    cell.char() != ' ' || !cell.pen().is_default()
}

fn sgr(result: &mut String, pen: Pen) {
    result.push_str("\x1b[0");
    for (enabled, code) in [
        (pen.is_bold(), 1),
        (pen.is_faint(), 2),
        (pen.is_italic(), 3),
        (pen.is_underline(), 4),
        (pen.is_blink(), 5),
        (pen.is_inverse(), 7),
        (pen.is_strikethrough(), 9),
    ] {
        if enabled {
            let _ = write!(result, ";{code}");
        }
    }
    for (color, code) in [(pen.foreground(), 38), (pen.background(), 48)] {
        match color {
            Some(Color::Indexed(index)) => {
                let _ = write!(result, ";{code};5;{index}");
            }
            Some(Color::RGB(rgb)) => {
                let _ = write!(result, ";{code};2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
            None => {}
        }
    }
    result.push('m');
}
