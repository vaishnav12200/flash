//! Headless terminal state for the initial fixed-grid milestone.
//!
//! This module deliberately implements only printable ASCII and the basic C0
//! controls needed to establish grid semantics. ANSI/VT parsing is introduced
//! in a later phase and will call these semantic operations rather than own the
//! grid directly.

const TAB_WIDTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
}

impl Cell {
    const EMPTY: Self = Self { character: ' ' };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug)]
pub struct Terminal {
    rows: usize,
    columns: usize,
    cells: Vec<Cell>,
    cursor: Cursor,
    wrap_pending: bool,
}

impl Terminal {
    /// Creates a fixed-size terminal grid.
    ///
    /// Grid dimensions are fixed for this phase. Dynamic resizing and PTY size
    /// propagation are intentionally deferred to the resize-and-scale phase.
    pub fn new(rows: usize, columns: usize) -> Self {
        assert!(rows > 0, "a terminal grid must have at least one row");
        assert!(columns > 0, "a terminal grid must have at least one column");

        Self {
            rows,
            columns,
            cells: vec![Cell::EMPTY; rows * columns],
            cursor: Cursor { row: 0, column: 0 },
            wrap_pending: false,
        }
    }

    /// Processes the basic byte subset supported by this milestone.
    ///
    /// Non-ASCII and escape-sequence bytes are left for UTF-8 and ANSI/VT
    /// phases; ignoring them here prevents unsupported bytes from corrupting
    /// grid or cursor state.
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match byte {
                b'\r' => self.carriage_return(),
                b'\n' => self.line_feed(),
                b'\x08' => self.backspace(),
                b'\t' => self.tab(),
                0x20..=0x7e => self.print(char::from(byte)),
                _ => {}
            }
        }
    }

    pub fn print(&mut self, character: char) {
        if self.wrap_pending {
            self.line_feed();
            self.carriage_return();
        }

        let index = self.index(self.cursor.row, self.cursor.column);
        self.cells[index] = Cell { character };

        if self.cursor.column + 1 == self.columns {
            self.wrap_pending = true;
        } else {
            self.cursor.column += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor.column = 0;
        self.wrap_pending = false;
    }

    pub fn line_feed(&mut self) {
        if self.cursor.row + 1 == self.rows {
            self.scroll_up();
        } else {
            self.cursor.row += 1;
        }
        self.wrap_pending = false;
    }

    pub fn backspace(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
        self.wrap_pending = false;
    }

    pub fn tab(&mut self) {
        let spaces = TAB_WIDTH - (self.cursor.column % TAB_WIDTH);
        for _ in 0..spaces {
            self.print(' ');
        }
    }

    fn scroll_up(&mut self) {
        self.cells.copy_within(self.columns.., 0);
        let final_row_start = self.index(self.rows - 1, 0);
        self.cells[final_row_start..].fill(Cell::EMPTY);
    }

    fn index(&self, row: usize, column: usize) -> usize {
        row * self.columns + column
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Terminal};

    fn row_text(terminal: &Terminal, row: usize) -> String {
        let start = terminal.index(row, 0);
        terminal.cells[start..start + terminal.columns]
            .iter()
            .map(|cell| cell.character)
            .collect()
    }

    #[test]
    fn prints_characters_and_advances_the_cursor() {
        let mut terminal = Terminal::new(2, 4);
        terminal.process_bytes(b"abc");

        assert_eq!(row_text(&terminal, 0), "abc ");
        assert_eq!(terminal.cursor, Cursor { row: 0, column: 3 });
    }

    #[test]
    fn wraps_only_when_the_next_printable_character_arrives() {
        let mut terminal = Terminal::new(2, 3);
        terminal.process_bytes(b"abcdef");

        assert_eq!(row_text(&terminal, 0), "abc");
        assert_eq!(row_text(&terminal, 1), "def");
        assert_eq!(terminal.cursor, Cursor { row: 1, column: 2 });

        terminal.process_bytes(b"g");
        assert_eq!(row_text(&terminal, 0), "def");
        assert_eq!(row_text(&terminal, 1), "g  ");
        assert_eq!(terminal.cursor, Cursor { row: 1, column: 1 });
    }

    #[test]
    fn carriage_return_overwrites_from_the_start_of_the_line() {
        let mut terminal = Terminal::new(1, 4);
        terminal.process_bytes(b"abc\rZ");

        assert_eq!(row_text(&terminal, 0), "Zbc ");
        assert_eq!(terminal.cursor, Cursor { row: 0, column: 1 });
    }

    #[test]
    fn line_feed_scrolls_when_the_cursor_reaches_the_bottom() {
        let mut terminal = Terminal::new(2, 3);
        terminal.process_bytes(b"abc\r\ndef\r\nghi");

        assert_eq!(row_text(&terminal, 0), "def");
        assert_eq!(row_text(&terminal, 1), "ghi");
    }

    #[test]
    fn backspace_moves_the_cursor_without_erasing_the_cell() {
        let mut terminal = Terminal::new(1, 4);
        terminal.process_bytes(b"ab\x08Z");

        assert_eq!(row_text(&terminal, 0), "aZ  ");
        assert_eq!(terminal.cursor, Cursor { row: 0, column: 2 });
    }

    #[test]
    fn tab_fills_cells_up_to_the_next_eight_column_stop() {
        let mut terminal = Terminal::new(1, 12);
        terminal.process_bytes(b"a\tb");

        assert_eq!(row_text(&terminal, 0), "a       b   ");
        assert_eq!(terminal.cursor, Cursor { row: 0, column: 9 });
    }
}
