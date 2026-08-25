//! Platform-independent terminal state and ANSI/VT semantic operations.

use std::collections::VecDeque;

mod parser;

pub use parser::TerminalParser;

const TAB_WIDTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags(u8);

impl CellFlags {
    const BOLD: u8 = 1 << 0;
    const ITALIC: u8 = 1 << 1;
    const UNDERLINE: u8 = 1 << 2;
    const INVERSE: u8 = 1 << 3;

    pub fn inverse(self) -> bool {
        self.0 & Self::INVERSE != 0
    }

    pub fn underline(self) -> bool {
        self.0 & Self::UNDERLINE != 0
    }

    fn set(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.0 |= flag
        } else {
            self.0 &= !flag
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
    pub foreground: Color,
    pub background: Color,
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            foreground: Color::Default,
            background: Color::Default,
            flags: CellFlags::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub rows: usize,
    pub columns: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: Cursor,
    pub end: Cursor,
}

impl Selection {
    pub fn contains(self, row: usize, column: usize) -> bool {
        let start = (self.start.row, self.start.column);
        let end = (self.end.row, self.end.column);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        (row, column) >= start && (row, column) <= end
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderSnapshot<'a> {
    pub rows: usize,
    pub columns: usize,
    pub cells: &'a [Cell],
    pub cursor: Cursor,
    pub cursor_visible: bool,
    pub selection: Option<Selection>,
}

#[derive(Debug, Clone)]
struct Screen {
    cells: Vec<Cell>,
    cursor: Cursor,
    saved_cursor: Cursor,
    wrap_pending: bool,
}

impl Screen {
    fn new(cell_count: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cell_count],
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            wrap_pending: false,
        }
    }

    fn resize(&mut self, old_size: GridSize, new_size: GridSize) {
        let mut cells = vec![Cell::default(); new_size.rows * new_size.columns];
        let copied_rows = old_size.rows.min(new_size.rows);
        let copied_columns = old_size.columns.min(new_size.columns);
        for row in 0..copied_rows {
            let old_start = row * old_size.columns;
            let new_start = row * new_size.columns;
            cells[new_start..new_start + copied_columns]
                .copy_from_slice(&self.cells[old_start..old_start + copied_columns]);
        }
        self.cells = cells;
        self.cursor.row = self.cursor.row.min(new_size.rows - 1);
        self.cursor.column = self.cursor.column.min(new_size.columns - 1);
        self.saved_cursor.row = self.saved_cursor.row.min(new_size.rows - 1);
        self.saved_cursor.column = self.saved_cursor.column.min(new_size.columns - 1);
        self.wrap_pending = false;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Attributes {
    foreground: Color,
    background: Color,
    flags: CellFlags,
}

#[derive(Debug)]
pub struct Terminal {
    rows: usize,
    columns: usize,
    primary: Screen,
    alternate: Screen,
    alternate_active: bool,
    attributes: Attributes,
    scroll_top: usize,
    scroll_bottom: usize,
    cursor_visible: bool,
    auto_wrap: bool,
    bracketed_paste: bool,
    history: VecDeque<Vec<Cell>>,
    history_limit: usize,
    viewport_offset: usize,
    viewport_cells: Vec<Cell>,
    selection: Option<Selection>,
}

impl Terminal {
    pub fn new(rows: usize, columns: usize) -> Self {
        assert!(rows > 0, "a terminal grid must have at least one row");
        assert!(columns > 0, "a terminal grid must have at least one column");
        let cell_count = rows * columns;
        Self {
            rows,
            columns,
            primary: Screen::new(cell_count),
            alternate: Screen::new(cell_count),
            alternate_active: false,
            attributes: Attributes::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            auto_wrap: true,
            bracketed_paste: false,
            history: VecDeque::new(),
            history_limit: 10_000,
            viewport_offset: 0,
            viewport_cells: vec![Cell::default(); cell_count],
            selection: None,
        }
    }

    pub fn size(&self) -> GridSize {
        GridSize {
            rows: self.rows,
            columns: self.columns,
        }
    }

    pub fn resize(&mut self, size: GridSize) -> bool {
        if size.rows == 0 || size.columns == 0 || size == self.size() {
            return false;
        }
        let old_size = self.size();
        self.primary.resize(old_size, size);
        self.alternate.resize(old_size, size);
        for line in &mut self.history {
            line.resize(size.columns, Cell::default());
            line.truncate(size.columns);
        }
        self.rows = size.rows;
        self.columns = size.columns;
        self.viewport_cells = vec![Cell::default(); size.rows * size.columns];
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        self.selection = None;
        self.reset_scroll_region();
        self.refresh_viewport();
        true
    }

    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.history_limit = limit;
        while self.history.len() > limit {
            self.history.pop_front();
        }
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        self.refresh_viewport();
    }

    pub fn scroll_viewport(&mut self, lines: isize) {
        if self.alternate_active || self.history.is_empty() {
            return;
        }
        if lines > 0 {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(lines as usize)
                .min(self.history.len());
        } else {
            self.viewport_offset = self.viewport_offset.saturating_sub(lines.unsigned_abs());
        }
        self.selection = None;
        self.refresh_viewport();
    }

    pub fn scroll_page_up(&mut self) {
        self.scroll_viewport((self.rows.saturating_sub(1)) as isize);
    }
    pub fn scroll_page_down(&mut self) {
        self.scroll_viewport(-((self.rows.saturating_sub(1)) as isize));
    }
    pub fn scroll_to_bottom(&mut self) {
        self.viewport_offset = 0;
        self.selection = None;
    }

    pub fn begin_selection(&mut self, cell: Cursor) {
        let cell = self.clamp_cell(cell);
        self.selection = Some(Selection {
            start: cell,
            end: cell,
        });
    }

    pub fn update_selection(&mut self, cell: Cursor) {
        let cell = self.clamp_cell(cell);
        if let Some(selection) = self.selection.as_mut() {
            selection.end = cell;
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let cells = self.visible_cells();
        let (start, end) = if (selection.start.row, selection.start.column)
            <= (selection.end.row, selection.end.column)
        {
            (selection.start, selection.end)
        } else {
            (selection.end, selection.start)
        };
        let mut output = String::new();
        for row in start.row..=end.row {
            let first = if row == start.row { start.column } else { 0 };
            let last = if row == end.row {
                end.column
            } else {
                self.columns - 1
            };
            let line: String = cells[row * self.columns + first..=row * self.columns + last]
                .iter()
                .map(|cell| cell.character)
                .collect();
            output.push_str(line.trim_end());
            if row != end.row {
                output.push('\n');
            }
        }
        Some(output)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub(crate) fn finish_output(&mut self) {
        self.selection = None;
        self.refresh_viewport();
    }

    pub fn print(&mut self, character: char) {
        if self.active().wrap_pending {
            self.line_feed();
            self.carriage_return();
        }
        let cursor = self.active().cursor;
        let index = self.index(cursor.row, cursor.column);
        let attributes = self.attributes;
        self.active_mut().cells[index] = Cell {
            character,
            foreground: attributes.foreground,
            background: attributes.background,
            flags: attributes.flags,
        };
        if cursor.column + 1 == self.columns {
            self.active_mut().wrap_pending = self.auto_wrap;
        } else {
            self.active_mut().cursor.column += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.active_mut().cursor.column = 0;
        self.active_mut().wrap_pending = false;
    }

    pub fn line_feed(&mut self) {
        let row = self.active().cursor.row;
        if row == self.scroll_bottom {
            self.scroll_up(1);
        } else if row + 1 < self.rows {
            self.active_mut().cursor.row += 1;
        }
        self.active_mut().wrap_pending = false;
    }

    pub fn reverse_index(&mut self) {
        let row = self.active().cursor.row;
        if row == self.scroll_top {
            self.scroll_down(1);
        } else {
            self.active_mut().cursor.row = row.saturating_sub(1);
        }
        self.active_mut().wrap_pending = false;
    }

    pub fn backspace(&mut self) {
        let column = self.active().cursor.column;
        self.active_mut().cursor.column = column.saturating_sub(1);
        self.active_mut().wrap_pending = false;
    }

    pub fn tab(&mut self) {
        let column = self.active().cursor.column;
        self.active_mut().cursor.column =
            (((column / TAB_WIDTH) + 1) * TAB_WIDTH).min(self.columns - 1);
        self.active_mut().wrap_pending = false;
    }

    pub fn move_cursor_relative(&mut self, row_delta: isize, column_delta: isize) {
        let cursor = self.active().cursor;
        self.active_mut().cursor = Cursor {
            row: cursor
                .row
                .saturating_add_signed(row_delta)
                .min(self.rows - 1),
            column: cursor
                .column
                .saturating_add_signed(column_delta)
                .min(self.columns - 1),
        };
        self.active_mut().wrap_pending = false;
    }

    pub fn set_cursor(&mut self, row: usize, column: usize) {
        self.active_mut().cursor = Cursor {
            row: row.min(self.rows - 1),
            column: column.min(self.columns - 1),
        };
        self.active_mut().wrap_pending = false;
    }

    pub fn save_cursor(&mut self) {
        self.active_mut().saved_cursor = self.active().cursor;
    }
    pub fn restore_cursor(&mut self) {
        self.active_mut().cursor = self.active().saved_cursor;
        self.active_mut().wrap_pending = false;
    }

    pub fn erase_display(&mut self, mode: u16) {
        let cursor = self.active().cursor;
        let index = self.index(cursor.row, cursor.column);
        let blank = self.blank_cell();
        match mode {
            0 => self.active_mut().cells[index..].fill(blank),
            1 => self.active_mut().cells[..=index].fill(blank),
            2 | 3 => self.active_mut().cells.fill(blank),
            _ => {}
        }
    }

    pub fn erase_line(&mut self, mode: u16) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, 0);
        let end = start + self.columns;
        let blank = self.blank_cell();
        match mode {
            0 => self.active_mut().cells[start + cursor.column..end].fill(blank),
            1 => self.active_mut().cells[start..=start + cursor.column].fill(blank),
            2 => self.active_mut().cells[start..end].fill(blank),
            _ => {}
        }
    }

    pub fn erase_characters(&mut self, count: usize) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, cursor.column);
        let end = (start + count).min(self.index(cursor.row, self.columns - 1) + 1);
        let blank = self.blank_cell();
        self.active_mut().cells[start..end].fill(blank);
    }

    pub fn insert_characters(&mut self, count: usize) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, cursor.column);
        let end = self.index(cursor.row, self.columns - 1) + 1;
        let count = count.min(end - start);
        self.active_mut()
            .cells
            .copy_within(start..end - count, start + count);
        let blank = self.blank_cell();
        self.active_mut().cells[start..start + count].fill(blank);
    }

    pub fn delete_characters(&mut self, count: usize) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, cursor.column);
        let end = self.index(cursor.row, self.columns - 1) + 1;
        let count = count.min(end - start);
        self.active_mut()
            .cells
            .copy_within(start + count..end, start);
        let blank = self.blank_cell();
        self.active_mut().cells[end - count..end].fill(blank);
    }

    pub fn insert_lines(&mut self, count: usize) {
        let row = self.active().cursor.row;
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        self.scroll_region_down(row, self.scroll_bottom, count);
    }

    pub fn delete_lines(&mut self, count: usize) {
        let row = self.active().cursor.row;
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        self.scroll_region_up(row, self.scroll_bottom, count);
    }

    pub fn scroll_up(&mut self, count: usize) {
        self.scroll_region_up(self.scroll_top, self.scroll_bottom, count);
    }
    pub fn scroll_down(&mut self, count: usize) {
        self.scroll_region_down(self.scroll_top, self.scroll_bottom, count);
    }

    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        if top < bottom && bottom < self.rows {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.set_cursor(0, 0);
        }
    }

    pub fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }
    pub fn set_auto_wrap(&mut self, enabled: bool) {
        self.auto_wrap = enabled;
        self.active_mut().wrap_pending = false;
    }
    pub fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }

    pub fn use_alternate_screen(&mut self, enabled: bool, clear: bool) {
        if enabled == self.alternate_active {
            return;
        }
        if enabled {
            self.primary.saved_cursor = self.primary.cursor;
            if clear {
                self.alternate = Screen::new(self.rows * self.columns);
            }
            self.alternate_active = true;
        } else {
            self.alternate_active = false;
            self.primary.cursor = self.primary.saved_cursor;
        }
        self.viewport_offset = 0;
        self.selection = None;
        self.reset_scroll_region();
    }

    pub fn reset(&mut self) {
        let rows = self.rows;
        let columns = self.columns;
        let history_limit = self.history_limit;
        *self = Self::new(rows, columns);
        self.history_limit = history_limit;
    }

    pub fn reset_attributes(&mut self) {
        self.attributes = Attributes::default();
    }
    pub fn set_foreground(&mut self, color: Color) {
        self.attributes.foreground = color;
    }
    pub fn set_background(&mut self, color: Color) {
        self.attributes.background = color;
    }
    pub fn set_bold(&mut self, enabled: bool) {
        self.attributes.flags.set(CellFlags::BOLD, enabled);
    }
    pub fn set_italic(&mut self, enabled: bool) {
        self.attributes.flags.set(CellFlags::ITALIC, enabled);
    }
    pub fn set_underline(&mut self, enabled: bool) {
        self.attributes.flags.set(CellFlags::UNDERLINE, enabled);
    }
    pub fn set_inverse(&mut self, enabled: bool) {
        self.attributes.flags.set(CellFlags::INVERSE, enabled);
    }

    pub(crate) fn render_snapshot(&self) -> RenderSnapshot<'_> {
        let screen = self.active();
        RenderSnapshot {
            rows: self.rows,
            columns: self.columns,
            cells: self.visible_cells(),
            cursor: screen.cursor,
            cursor_visible: self.cursor_visible && self.viewport_offset == 0,
            selection: self.selection,
        }
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom - top + 1);
        let columns = self.columns;
        if !self.alternate_active && top == 0 && bottom + 1 == self.rows {
            for row in 0..count {
                let start = row * columns;
                self.history
                    .push_back(self.primary.cells[start..start + columns].to_vec());
            }
            while self.history.len() > self.history_limit {
                self.history.pop_front();
            }
            if self.viewport_offset > 0 {
                self.viewport_offset = self
                    .viewport_offset
                    .saturating_add(count)
                    .min(self.history.len());
            }
        }
        let source = (top + count) * columns..(bottom + 1) * columns;
        self.active_mut().cells.copy_within(source, top * columns);
        let blank = self.blank_cell();
        self.active_mut().cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(blank);
    }

    fn scroll_region_down(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom - top + 1);
        let columns = self.columns;
        let source = top * columns..(bottom + 1 - count) * columns;
        self.active_mut()
            .cells
            .copy_within(source, (top + count) * columns);
        let blank = self.blank_cell();
        self.active_mut().cells[top * columns..(top + count) * columns].fill(blank);
    }

    fn refresh_viewport(&mut self) {
        if self.viewport_offset == 0 || self.alternate_active {
            return;
        }
        let start_line = self.history.len().saturating_sub(self.viewport_offset);
        for row in 0..self.rows {
            let logical_line = start_line + row;
            let destination = row * self.columns;
            if logical_line < self.history.len() {
                self.viewport_cells[destination..destination + self.columns]
                    .copy_from_slice(&self.history[logical_line]);
            } else {
                let screen_row = logical_line - self.history.len();
                let source = screen_row * self.columns;
                self.viewport_cells[destination..destination + self.columns]
                    .copy_from_slice(&self.primary.cells[source..source + self.columns]);
            }
        }
    }

    fn visible_cells(&self) -> &[Cell] {
        if self.viewport_offset > 0 && !self.alternate_active {
            &self.viewport_cells
        } else {
            &self.active().cells
        }
    }

    fn clamp_cell(&self, cell: Cursor) -> Cursor {
        Cursor {
            row: cell.row.min(self.rows - 1),
            column: cell.column.min(self.columns - 1),
        }
    }

    fn blank_cell(&self) -> Cell {
        Cell {
            background: self.attributes.background,
            ..Cell::default()
        }
    }
    fn active(&self) -> &Screen {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }
    fn active_mut(&mut self) -> &mut Screen {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }
    fn index(&self, row: usize, column: usize) -> usize {
        row * self.columns + column
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Cursor, Terminal};

    fn row_text(terminal: &Terminal, row: usize) -> String {
        let start = terminal.index(row, 0);
        terminal.active().cells[start..start + terminal.columns]
            .iter()
            .map(|cell| cell.character)
            .collect()
    }

    #[test]
    fn preserves_basic_wrapping_and_scrolling() {
        let mut terminal = Terminal::new(2, 3);
        for character in "abcdefg".chars() {
            terminal.print(character);
        }
        assert_eq!(row_text(&terminal, 0), "def");
        assert_eq!(row_text(&terminal, 1), "g  ");
        assert_eq!(terminal.active().cursor, Cursor { row: 1, column: 1 });
    }

    #[test]
    fn erases_line_from_cursor() {
        let mut terminal = Terminal::new(1, 5);
        for character in "abcde".chars() {
            terminal.print(character);
        }
        terminal.set_cursor(0, 2);
        terminal.erase_line(0);
        assert_eq!(row_text(&terminal, 0), "ab   ");
    }

    #[test]
    fn scroll_region_preserves_rows_outside_margins() {
        let mut terminal = Terminal::new(4, 2);
        for (row, text) in ["aa", "bb", "cc", "dd"].iter().enumerate() {
            terminal.set_cursor(row, 0);
            for character in text.chars() {
                terminal.print(character);
            }
        }
        terminal.set_scroll_region(1, 2);
        terminal.set_cursor(2, 0);
        terminal.line_feed();
        assert_eq!(row_text(&terminal, 0), "aa");
        assert_eq!(row_text(&terminal, 1), "cc");
        assert_eq!(row_text(&terminal, 2), "  ");
        assert_eq!(row_text(&terminal, 3), "dd");
    }

    #[test]
    fn alternate_screen_restores_primary_contents_and_cursor() {
        let mut terminal = Terminal::new(2, 3);
        terminal.print('p');
        terminal.use_alternate_screen(true, true);
        terminal.print('a');
        assert_eq!(row_text(&terminal, 0), "a  ");
        terminal.use_alternate_screen(false, false);
        assert_eq!(row_text(&terminal, 0), "p  ");
        assert_eq!(terminal.active().cursor, Cursor { row: 0, column: 1 });
    }

    #[test]
    fn resize_preserves_visible_cells_and_clamps_cursor() {
        let mut terminal = Terminal::new(2, 3);
        for character in "abcdef".chars() {
            terminal.print(character);
        }
        assert!(terminal.resize(super::GridSize {
            rows: 3,
            columns: 2
        }));
        assert_eq!(row_text(&terminal, 0), "ab");
        assert_eq!(row_text(&terminal, 1), "de");
        assert_eq!(terminal.active().cursor, Cursor { row: 1, column: 1 });
        assert!(!terminal.resize(super::GridSize {
            rows: 3,
            columns: 2
        }));
    }

    #[test]
    fn printed_cells_copy_current_colors() {
        let mut terminal = Terminal::new(1, 1);
        terminal.set_foreground(Color::Rgb(1, 2, 3));
        terminal.set_background(Color::Indexed(4));
        terminal.print('x');
        let cell = terminal.active().cells[0];
        assert_eq!(cell.foreground, Color::Rgb(1, 2, 3));
        assert_eq!(cell.background, Color::Indexed(4));
    }

    #[test]
    fn primary_scrollback_is_bounded_and_excludes_alternate_screen() {
        let mut terminal = Terminal::new(2, 2);
        terminal.set_scrollback_limit(2);
        for character in "aabbccddee".chars() {
            terminal.print(character);
        }
        assert_eq!(terminal.history.len(), 2);
        assert_eq!(terminal.history[0][0].character, 'b');

        terminal.use_alternate_screen(true, true);
        for character in "xxyyzz".chars() {
            terminal.print(character);
        }
        assert_eq!(terminal.history.len(), 2);
    }

    #[test]
    fn viewport_scrolls_through_history_and_hides_cursor() {
        let mut terminal = Terminal::new(2, 2);
        for character in "aabbcc".chars() {
            terminal.print(character);
        }
        terminal.finish_output();
        terminal.scroll_viewport(1);
        let snapshot = terminal.render_snapshot();
        assert_eq!(snapshot.cells[0].character, 'a');
        assert!(!snapshot.cursor_visible);
        terminal.scroll_to_bottom();
        assert!(terminal.render_snapshot().cursor_visible);
    }

    #[test]
    fn selection_extracts_multiple_trimmed_rows() {
        let mut terminal = Terminal::new(2, 4);
        for character in "abc".chars() {
            terminal.print(character);
        }
        terminal.set_cursor(1, 0);
        for character in "xy".chars() {
            terminal.print(character);
        }
        terminal.begin_selection(Cursor { row: 0, column: 1 });
        terminal.update_selection(Cursor { row: 1, column: 2 });
        assert_eq!(terminal.selected_text().as_deref(), Some("bc\nxy"));
        terminal.finish_output();
        assert!(terminal.selected_text().is_none());
    }

    #[test]
    fn resize_normalizes_historical_line_widths() {
        let mut terminal = Terminal::new(2, 3);
        for character in "aaabbbccc".chars() {
            terminal.print(character);
        }
        terminal.finish_output();
        terminal.scroll_viewport(1);
        assert!(terminal.resize(super::GridSize {
            rows: 2,
            columns: 2,
        }));
        assert_eq!(terminal.render_snapshot().cells.len(), 4);
        assert!(terminal.history.iter().all(|line| line.len() == 2));
    }
}
