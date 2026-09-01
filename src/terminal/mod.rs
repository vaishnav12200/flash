//! Platform-independent terminal state and ANSI/VT semantic operations.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

mod parser;

pub use parser::TerminalParser;

const TAB_WIDTH: usize = 8;
const MAX_COMBINING_CHARACTERS: usize = 8;
const MAX_TITLE_BYTES: usize = 1024;

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
    const DIM: u8 = 1 << 4;

    pub fn inverse(self) -> bool {
        self.0 & Self::INVERSE != 0
    }

    pub fn underline(self) -> bool {
        self.0 & Self::UNDERLINE != 0
    }

    pub fn bold(self) -> bool {
        self.0 & Self::BOLD != 0
    }

    pub fn italic(self) -> bool {
        self.0 & Self::ITALIC != 0
    }

    pub fn dim(self) -> bool {
        self.0 & Self::DIM != 0
    }

    fn set(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.0 |= flag
        } else {
            self.0 &= !flag
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellWidth {
    #[default]
    Single,
    Wide,
    Continuation,
}

impl CellWidth {
    pub fn columns(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Wide => 2,
            Self::Continuation => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
    combining: [char; MAX_COMBINING_CHARACTERS],
    combining_len: u8,
    pub width: CellWidth,
    pub foreground: Color,
    pub background: Color,
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            combining: ['\0'; MAX_COMBINING_CHARACTERS],
            combining_len: 0,
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            flags: CellFlags::default(),
        }
    }
}

impl Cell {
    pub fn characters(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.character).chain(
            self.combining[..usize::from(self.combining_len)]
                .iter()
                .copied(),
        )
    }

    pub fn is_continuation(&self) -> bool {
        self.width == CellWidth::Continuation
    }

    fn append(&mut self, character: char) {
        let index = usize::from(self.combining_len);
        if index < self.combining.len() {
            self.combining[index] = character;
            self.combining_len += 1;
        }
    }

    fn sequence_ends_with(&self, character: char) -> bool {
        if self.combining_len == 0 {
            self.character == character
        } else {
            self.combining[usize::from(self.combining_len) - 1] == character
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    None,
    Button,
    ButtonMotion,
    AnyMotion,
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
    pub row_versions: &'a [u64],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SearchContext {
    pub alternate_active: bool,
    pub screen_generation: u64,
    pub row_origin: u64,
}

#[derive(Debug, Clone)]
struct Screen {
    cells: Vec<Cell>,
    wrapped: Vec<bool>,
    cursor: Cursor,
    saved_cursor: SavedCursor,
    wrap_pending: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct SavedCursor {
    cursor: Cursor,
    attributes: Attributes,
    auto_wrap: bool,
    origin_mode: bool,
}

impl Screen {
    fn new(cell_count: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cell_count],
            wrapped: vec![false; rows],
            cursor: Cursor::default(),
            saved_cursor: SavedCursor {
                auto_wrap: true,
                ..SavedCursor::default()
            },
            wrap_pending: false,
        }
    }

    fn resize(&mut self, old_size: GridSize, new_size: GridSize) {
        if self.wrapped.len() != old_size.rows {
            self.wrapped.resize(old_size.rows, false);
        }
        let mut cells = vec![Cell::default(); new_size.rows * new_size.columns];
        let copied_rows = old_size.rows.min(new_size.rows);
        let copied_columns = old_size.columns.min(new_size.columns);
        for row in 0..copied_rows {
            let old_start = row * old_size.columns;
            let new_start = row * new_size.columns;
            cells[new_start..new_start + copied_columns]
                .copy_from_slice(&self.cells[old_start..old_start + copied_columns]);
        }
        for row in 0..new_size.rows {
            normalize_row(&mut cells, new_size.columns, row, Cell::default());
        }
        self.cells = cells;
        self.wrapped.resize(new_size.rows, false);
        self.cursor.row = self.cursor.row.min(new_size.rows - 1);
        self.cursor.column = self.cursor.column.min(new_size.columns - 1);
        self.saved_cursor.cursor.row = self.saved_cursor.cursor.row.min(new_size.rows - 1);
        self.saved_cursor.cursor.column = self.saved_cursor.cursor.column.min(new_size.columns - 1);
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
    origin_mode: bool,
    insert_mode: bool,
    application_cursor_keys: bool,
    application_keypad: bool,
    bracketed_paste: bool,
    mouse_tracking: MouseTracking,
    sgr_mouse: bool,
    history: VecDeque<Vec<Cell>>,
    history_wrapped: VecDeque<bool>,
    history_limit: usize,
    search_row_origin: u64,
    search_screen_generation: u64,
    viewport_offset: usize,
    viewport_cells: Vec<Cell>,
    viewport_wrapped: Vec<bool>,
    selection: Option<Selection>,
    selection_active: bool,
    tab_stops: Vec<bool>,
    title: String,
    title_version: u64,
    row_versions: Vec<u64>,
    next_damage_version: u64,
    damage_batching: bool,
    pending_damage: Option<(usize, usize)>,
}

impl Terminal {
    pub fn new(rows: usize, columns: usize) -> Self {
        assert!(rows > 0, "a terminal grid must have at least one row");
        assert!(columns > 0, "a terminal grid must have at least one column");
        let cell_count = rows * columns;
        Self {
            rows,
            columns,
            primary: Screen::new(cell_count, rows),
            alternate: Screen::new(cell_count, rows),
            alternate_active: false,
            attributes: Attributes::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            auto_wrap: true,
            origin_mode: false,
            insert_mode: false,
            application_cursor_keys: false,
            application_keypad: false,
            bracketed_paste: false,
            mouse_tracking: MouseTracking::None,
            sgr_mouse: false,
            history: VecDeque::new(),
            history_wrapped: VecDeque::new(),
            history_limit: 10_000,
            search_row_origin: 0,
            search_screen_generation: 0,
            viewport_offset: 0,
            viewport_cells: vec![Cell::default(); cell_count],
            viewport_wrapped: vec![false; rows],
            selection: None,
            selection_active: false,
            tab_stops: default_tab_stops(columns),
            title: String::new(),
            title_version: 0,
            row_versions: vec![1; rows],
            next_damage_version: 2,
            damage_batching: false,
            pending_damage: None,
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
            normalize_row(line, size.columns, 0, Cell::default());
        }
        self.rows = size.rows;
        self.columns = size.columns;
        self.viewport_cells = vec![Cell::default(); size.rows * size.columns];
        self.viewport_wrapped = vec![false; size.rows];
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        self.selection = None;
        self.selection_active = false;
        let old_columns = old_size.columns;
        self.tab_stops.resize(size.columns, false);
        for column in old_columns..size.columns {
            self.tab_stops[column] = column % TAB_WIDTH == 0;
        }
        self.row_versions = vec![0; size.rows];
        self.damage_all();
        self.reset_scroll_region();
        self.refresh_viewport();
        true
    }

    pub fn set_scrollback_limit(&mut self, limit: usize) {
        let old_offset = self.viewport_offset;
        self.history_limit = limit;
        let mut removed = 0;
        while self.history.len() > limit {
            self.history.pop_front();
            self.history_wrapped.pop_front();
            removed += 1;
        }
        self.advance_search_row_origin(removed);
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        self.refresh_viewport();
        if self.viewport_offset != old_offset {
            self.damage_all();
        }
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
        self.selection_active = false;
        self.refresh_viewport();
        self.damage_all();
    }

    pub fn scroll_page_up(&mut self) {
        self.scroll_viewport((self.rows.saturating_sub(1)) as isize);
    }
    pub fn scroll_page_down(&mut self) {
        self.scroll_viewport(-((self.rows.saturating_sub(1)) as isize));
    }
    pub fn scroll_to_bottom(&mut self) {
        let changed = self.viewport_offset != 0 || self.selection.is_some();
        self.viewport_offset = 0;
        self.selection = None;
        self.selection_active = false;
        if changed {
            self.damage_all();
        }
    }

    pub fn begin_selection(&mut self, cell: Cursor) {
        if let Some(selection) = self.selection {
            self.damage_selection(selection);
        }
        let cell = self.clamp_cell(cell);
        self.selection = Some(Selection {
            start: cell,
            end: cell,
        });
        self.selection_active = false;
        self.damage_row(cell.row);
    }

    pub fn update_selection(&mut self, cell: Cursor) {
        let cell = self.clamp_cell(cell);
        if let Some(old_selection) = self.selection {
            self.damage_selection(old_selection);
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.end = cell;
            self.selection_active = true;
        }
        if let Some(selection) = self.selection {
            self.damage_selection(selection);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_active = false;
        if let Some(selection) = self.selection.take() {
            self.damage_selection(selection);
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        if !self.selection_active {
            return None;
        }
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
            let mut line = String::new();
            for cell in &cells[row * self.columns + first..=row * self.columns + last] {
                if !cell.is_continuation() {
                    line.extend(cell.characters());
                }
            }
            output.push_str(line.trim_end());
            if row != end.row && !self.visible_row_wrapped(row) {
                output.push('\n');
            }
        }
        Some(output)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    pub fn application_keypad(&self) -> bool {
        self.application_keypad
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.alternate_active
    }

    pub(crate) fn searchable_row_count(&self) -> usize {
        if self.alternate_active {
            self.rows
        } else {
            self.history.len() + self.rows
        }
    }

    pub(crate) fn search_context(&self) -> SearchContext {
        SearchContext {
            alternate_active: self.alternate_active,
            screen_generation: self.search_screen_generation,
            row_origin: self.search_row_origin,
        }
    }

    pub(crate) fn searchable_visible_rows(&self) -> std::ops::Range<usize> {
        if self.alternate_active {
            return 0..self.rows;
        }
        let start = self.history.len().saturating_sub(self.viewport_offset);
        start..(start + self.rows).min(self.searchable_row_count())
    }

    pub(crate) fn reveal_search_row(&mut self, row: usize) -> bool {
        if row >= self.searchable_row_count() || self.alternate_active {
            return false;
        }
        let visible = self.searchable_visible_rows();
        let new_offset = if row < visible.start {
            self.history.len().saturating_sub(row)
        } else if row >= visible.end {
            self.history
                .len()
                .saturating_add(self.rows.saturating_sub(1))
                .saturating_sub(row)
        } else {
            self.viewport_offset
        }
        .min(self.history.len());
        if new_offset == self.viewport_offset {
            return false;
        }
        self.viewport_offset = new_offset;
        self.selection = None;
        self.selection_active = false;
        self.refresh_viewport();
        self.damage_all();
        true
    }

    /// Returns an immutable physical row in search order. Primary-screen rows
    /// begin with the oldest retained history row and end with the live grid;
    /// the alternate screen exposes only its own live grid.
    pub(crate) fn searchable_row(&self, row: usize) -> Option<&[Cell]> {
        if self.alternate_active {
            let start = row.checked_mul(self.columns)?;
            return self
                .alternate
                .cells
                .get(start..start.saturating_add(self.columns));
        }
        if row < self.history.len() {
            return self.history.get(row).map(Vec::as_slice);
        }
        let screen_row = row.checked_sub(self.history.len())?;
        let start = screen_row.checked_mul(self.columns)?;
        self.primary
            .cells
            .get(start..start.saturating_add(self.columns))
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn title_version(&self) -> u64 {
        self.title_version
    }

    pub fn set_title(&mut self, parts: &[&[u8]]) {
        let mut title = String::with_capacity(
            parts
                .iter()
                .map(|part| part.len())
                .sum::<usize>()
                .min(MAX_TITLE_BYTES),
        );
        for (index, part) in parts.iter().enumerate() {
            if index > 0 && title.len() < MAX_TITLE_BYTES {
                title.push(';');
            }
            for character in String::from_utf8_lossy(part).chars() {
                if character.is_control()
                    || title.len().saturating_add(character.len_utf8()) > MAX_TITLE_BYTES
                {
                    continue;
                }
                title.push(character);
            }
        }
        if title != self.title {
            self.title = title;
            self.title_version = self.title_version.wrapping_add(1);
        }
    }

    pub fn mouse_tracking(&self) -> MouseTracking {
        self.mouse_tracking
    }

    pub fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }

    pub(crate) fn begin_output(&mut self) {
        debug_assert!(
            !self.damage_batching,
            "terminal output batches must not nest"
        );
        self.damage_batching = true;
        self.pending_damage = None;
        self.damage_row(self.active().cursor.row);
    }

    pub(crate) fn finish_output(&mut self) {
        self.clear_selection();
        self.refresh_viewport();
        self.damage_batching = false;
        if let Some((first, last)) = self.pending_damage.take() {
            self.apply_damage_rows(first, last);
        }
    }

    #[cfg(test)]
    pub fn print(&mut self, character: char) {
        self.print_impl(character, true);
    }

    pub(crate) fn print_output(&mut self, character: char) {
        self.print_impl(character, false);
    }

    #[inline]
    fn print_impl(&mut self, character: char, track_damage: bool) {
        let unicode_width = UnicodeWidthChar::width(character).unwrap_or(0).min(2);
        if unicode_width == 0 || self.should_join_previous(character) {
            self.append_to_previous(character, track_damage);
            return;
        }

        if self.active().wrap_pending {
            self.line_feed_impl(true);
            self.carriage_return();
        }

        let mut width = unicode_width;
        let mut cursor = self.active().cursor;
        if width == 2 && cursor.column + 1 >= self.columns {
            if self.auto_wrap && self.columns > 1 {
                self.line_feed_impl(true);
                self.carriage_return();
                cursor = self.active().cursor;
            } else {
                width = 1;
            }
        }

        if self.insert_mode {
            self.insert_characters(width);
        }

        self.clear_cell_span(cursor.row, cursor.column);
        let index = self.index(cursor.row, cursor.column);
        let attributes = self.attributes;
        self.active_mut().cells[index] = Cell {
            character,
            width: if width == 2 {
                CellWidth::Wide
            } else {
                CellWidth::Single
            },
            foreground: attributes.foreground,
            background: attributes.background,
            flags: attributes.flags,
            ..Cell::default()
        };
        if width == 2 {
            self.active_mut().cells[index + 1] = Cell {
                width: CellWidth::Continuation,
                foreground: attributes.foreground,
                background: attributes.background,
                flags: attributes.flags,
                ..Cell::default()
            };
        }

        if cursor.column + width >= self.columns {
            self.active_mut().cursor.column = self.columns - 1;
            self.active_mut().wrap_pending = self.auto_wrap;
        } else {
            self.active_mut().cursor.column += width;
        }
        if track_damage {
            self.damage_row(cursor.row);
        }
    }

    pub fn carriage_return(&mut self) {
        let cursor = self.active().cursor;
        let changed = cursor.column != 0 || self.active().wrap_pending;
        self.active_mut().cursor.column = 0;
        self.active_mut().wrap_pending = false;
        if changed && !self.damage_batching {
            self.damage_row(cursor.row);
        }
    }

    pub fn line_feed(&mut self) {
        self.line_feed_impl(false);
    }

    fn line_feed_impl(&mut self, soft_wrap: bool) {
        let row = self.active().cursor.row;
        self.active_mut().wrapped[row] = soft_wrap;
        if row == self.scroll_bottom {
            self.scroll_up(1);
            self.active_mut().wrap_pending = false;
            return;
        } else if row + 1 < self.rows {
            self.active_mut().cursor.row += 1;
        }
        self.active_mut().wrap_pending = false;
        self.damage_row(row);
        self.damage_row(self.active().cursor.row);
    }

    pub fn reverse_index(&mut self) {
        let row = self.active().cursor.row;
        if row == self.scroll_top {
            self.scroll_down(1);
            self.active_mut().wrap_pending = false;
            return;
        } else {
            self.active_mut().cursor.row = row.saturating_sub(1);
        }
        self.active_mut().wrap_pending = false;
        self.damage_row(row);
        self.damage_row(self.active().cursor.row);
    }

    pub fn backspace(&mut self) {
        let cursor = self.active().cursor;
        let changed = cursor.column != 0 || self.active().wrap_pending;
        self.active_mut().cursor.column = cursor.column.saturating_sub(1);
        self.active_mut().wrap_pending = false;
        if changed && !self.damage_batching {
            self.damage_row(cursor.row);
        }
    }

    pub fn tab(&mut self) {
        let cursor = self.active().cursor;
        self.active_mut().cursor.column = self.tab_stops[cursor.column + 1..]
            .iter()
            .position(|stop| *stop)
            .map_or(self.columns - 1, |offset| cursor.column + 1 + offset);
        self.active_mut().wrap_pending = false;
        if self.active().cursor != cursor && !self.damage_batching {
            self.damage_row(cursor.row);
        }
    }

    pub fn tab_forward(&mut self, count: usize) {
        for _ in 0..count.min(self.columns) {
            self.tab();
        }
    }

    pub fn tab_backward(&mut self, count: usize) {
        let old_cursor = self.active().cursor;
        let mut column = old_cursor.column;
        for _ in 0..count.min(self.columns) {
            column = self.tab_stops[..column]
                .iter()
                .rposition(|stop| *stop)
                .unwrap_or(0);
        }
        self.active_mut().cursor.column = column;
        self.active_mut().wrap_pending = false;
        if column != old_cursor.column && !self.damage_batching {
            self.damage_row(old_cursor.row);
        }
    }

    pub fn move_cursor_relative(&mut self, row_delta: isize, column_delta: isize) {
        let cursor = self.active().cursor;
        let (minimum_row, maximum_row) = if self.origin_mode {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        let new_cursor = Cursor {
            row: cursor
                .row
                .saturating_add_signed(row_delta)
                .clamp(minimum_row, maximum_row),
            column: cursor
                .column
                .saturating_add_signed(column_delta)
                .min(self.columns - 1),
        };
        self.active_mut().cursor = new_cursor;
        self.active_mut().wrap_pending = false;
        if new_cursor != cursor {
            self.damage_row(cursor.row);
            self.damage_row(new_cursor.row);
        }
    }

    pub fn set_cursor(&mut self, row: usize, column: usize) {
        let old_cursor = self.active().cursor;
        let new_cursor = Cursor {
            row: row.min(self.rows - 1),
            column: column.min(self.columns - 1),
        };
        self.active_mut().cursor = new_cursor;
        self.active_mut().wrap_pending = false;
        if new_cursor != old_cursor {
            self.damage_row(old_cursor.row);
            self.damage_row(new_cursor.row);
        }
    }

    pub fn set_cursor_address(&mut self, row: usize, column: usize) {
        let row = if self.origin_mode {
            self.scroll_top.saturating_add(row).min(self.scroll_bottom)
        } else {
            row
        };
        self.set_cursor(row, column);
    }

    pub fn save_cursor(&mut self) {
        let saved_cursor = SavedCursor {
            cursor: self.active().cursor,
            attributes: self.attributes,
            auto_wrap: self.auto_wrap,
            origin_mode: self.origin_mode,
        };
        self.active_mut().saved_cursor = saved_cursor;
    }
    pub fn restore_cursor(&mut self) {
        let old_row = self.active().cursor.row;
        let saved_cursor = self.active().saved_cursor;
        self.active_mut().cursor = saved_cursor.cursor;
        self.active_mut().wrap_pending = false;
        self.attributes = saved_cursor.attributes;
        self.auto_wrap = saved_cursor.auto_wrap;
        self.origin_mode = saved_cursor.origin_mode;
        self.damage_row(old_row);
        self.damage_row(self.active().cursor.row);
    }

    pub fn erase_display(&mut self, mode: u16) {
        let cursor = self.active().cursor;
        let index = self.index(cursor.row, cursor.column);
        let blank = self.blank_cell();
        match mode {
            0 => {
                self.active_mut().cells[index..].fill(blank);
                self.active_mut().wrapped[cursor.row..].fill(false);
                self.damage_rows(cursor.row, self.rows - 1);
            }
            1 => {
                self.active_mut().cells[..=index].fill(blank);
                if cursor.row > 0 {
                    self.active_mut().wrapped[..cursor.row].fill(false);
                }
                self.damage_rows(0, cursor.row);
            }
            2 => {
                self.active_mut().cells.fill(blank);
                self.active_mut().wrapped.fill(false);
                self.damage_all();
            }
            3 if !self.alternate_active => {
                let removed = self.history.len();
                self.history.clear();
                self.history_wrapped.clear();
                self.advance_search_row_origin(removed);
                self.viewport_offset = 0;
                self.selection = None;
                self.selection_active = false;
                self.damage_all();
            }
            _ => {}
        }
        self.normalize_active_rows(0, self.rows - 1);
    }

    pub fn erase_line(&mut self, mode: u16) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, 0);
        let end = start + self.columns;
        let blank = self.blank_cell();
        match mode {
            0 => {
                self.active_mut().cells[start + cursor.column..end].fill(blank);
                self.active_mut().wrapped[cursor.row] = false;
            }
            1 => self.active_mut().cells[start..=start + cursor.column].fill(blank),
            2 => {
                self.active_mut().cells[start..end].fill(blank);
                self.active_mut().wrapped[cursor.row] = false;
            }
            _ => {}
        }
        self.normalize_active_rows(cursor.row, cursor.row);
        self.damage_row(cursor.row);
    }

    pub fn erase_characters(&mut self, count: usize) {
        let cursor = self.active().cursor;
        let start = self.index(cursor.row, cursor.column);
        let end = (start + count).min(self.index(cursor.row, self.columns - 1) + 1);
        let blank = self.blank_cell();
        self.active_mut().cells[start..end].fill(blank);
        self.normalize_active_rows(cursor.row, cursor.row);
        self.damage_row(cursor.row);
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
        self.normalize_active_rows(cursor.row, cursor.row);
        self.damage_row(cursor.row);
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
        self.normalize_active_rows(cursor.row, cursor.row);
        self.damage_row(cursor.row);
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
            self.home_cursor();
        }
    }

    pub fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }
    pub fn set_cursor_visible(&mut self, visible: bool) {
        if self.cursor_visible != visible {
            self.damage_row(self.active().cursor.row);
        }
        self.cursor_visible = visible;
    }
    pub fn set_auto_wrap(&mut self, enabled: bool) {
        self.auto_wrap = enabled;
        self.active_mut().wrap_pending = false;
    }
    pub fn set_origin_mode(&mut self, enabled: bool) {
        self.origin_mode = enabled;
        self.home_cursor();
    }
    pub fn set_insert_mode(&mut self, enabled: bool) {
        self.insert_mode = enabled;
    }
    pub fn set_application_cursor_keys(&mut self, enabled: bool) {
        self.application_cursor_keys = enabled;
    }
    pub fn set_application_keypad(&mut self, enabled: bool) {
        self.application_keypad = enabled;
    }
    pub fn set_tab_stop(&mut self) {
        let column = self.active().cursor.column;
        self.tab_stops[column] = true;
    }
    pub fn clear_tab_stop(&mut self, mode: u16) {
        match mode {
            0 => {
                let column = self.active().cursor.column;
                self.tab_stops[column] = false;
            }
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }
    pub fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }
    pub fn set_mouse_tracking(&mut self, mode: MouseTracking, enabled: bool) {
        if enabled {
            self.mouse_tracking = mode;
        } else if self.mouse_tracking == mode {
            self.mouse_tracking = MouseTracking::None;
        }
    }
    pub fn set_sgr_mouse(&mut self, enabled: bool) {
        self.sgr_mouse = enabled;
    }

    pub fn use_alternate_screen(&mut self, enabled: bool, clear: bool) {
        if enabled == self.alternate_active {
            return;
        }
        if enabled {
            if clear {
                self.alternate = Screen::new(self.rows * self.columns, self.rows);
            }
            self.alternate_active = true;
            if clear {
                self.home_cursor();
            }
        } else {
            self.alternate_active = false;
        }
        self.search_screen_generation = self.search_screen_generation.wrapping_add(1);
        self.viewport_offset = 0;
        self.selection = None;
        self.selection_active = false;
        self.damage_all();
    }

    pub fn reset(&mut self) {
        let rows = self.rows;
        let columns = self.columns;
        let history_limit = self.history_limit;
        let next_damage_version = self.next_damage_version;
        let damage_batching = self.damage_batching;
        let search_screen_generation = self.search_screen_generation.wrapping_add(1);
        *self = Self::new(rows, columns);
        self.history_limit = history_limit;
        self.row_versions.fill(0);
        self.next_damage_version = next_damage_version;
        self.damage_batching = damage_batching;
        self.search_screen_generation = search_screen_generation;
        self.damage_all();
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
    pub fn set_dim(&mut self, enabled: bool) {
        self.attributes.flags.set(CellFlags::DIM, enabled);
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
            // A press establishes an invisible drag anchor. Only a subsequent
            // pointer move activates the overlay, even within the same cell.
            selection: self.selection.filter(|_| self.selection_active),
            row_versions: &self.row_versions,
        }
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        let columns = self.columns;
        if !self.alternate_active && top == 0 && bottom + 1 == self.rows {
            if self.history_limit == 0 {
                self.advance_search_row_origin(count);
            } else {
                for row in 0..count {
                    let start = row * columns;
                    let mut line = if self.history.len() >= self.history_limit {
                        self.history_wrapped.pop_front();
                        self.advance_search_row_origin(1);
                        self.history
                            .pop_front()
                            .expect("full scrollback has a reusable row")
                    } else {
                        Vec::with_capacity(columns)
                    };
                    line.clear();
                    line.extend_from_slice(&self.primary.cells[start..start + columns]);
                    self.history.push_back(line);
                    self.history_wrapped.push_back(self.primary.wrapped[row]);
                }
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
        self.active_mut()
            .wrapped
            .copy_within(top + count..bottom + 1, top);
        let blank = self.blank_cell();
        self.active_mut().cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(blank);
        self.active_mut().wrapped[bottom + 1 - count..=bottom].fill(false);
        self.damage_rows(top, bottom);
    }

    fn scroll_region_down(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        let columns = self.columns;
        let source = top * columns..(bottom + 1 - count) * columns;
        self.active_mut()
            .cells
            .copy_within(source, (top + count) * columns);
        self.active_mut()
            .wrapped
            .copy_within(top..bottom + 1 - count, top + count);
        let blank = self.blank_cell();
        self.active_mut().cells[top * columns..(top + count) * columns].fill(blank);
        self.active_mut().wrapped[top..top + count].fill(false);
        self.damage_rows(top, bottom);
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
                self.viewport_wrapped[row] = self.history_wrapped[logical_line];
            } else {
                let screen_row = logical_line - self.history.len();
                let source = screen_row * self.columns;
                self.viewport_cells[destination..destination + self.columns]
                    .copy_from_slice(&self.primary.cells[source..source + self.columns]);
                self.viewport_wrapped[row] = self.primary.wrapped[screen_row];
            }
        }
    }

    fn advance_search_row_origin(&mut self, count: usize) {
        self.search_row_origin = self.search_row_origin.wrapping_add(count as u64);
    }

    fn visible_cells(&self) -> &[Cell] {
        if self.viewport_offset > 0 && !self.alternate_active {
            &self.viewport_cells
        } else {
            &self.active().cells
        }
    }

    fn visible_row_wrapped(&self, row: usize) -> bool {
        if self.viewport_offset > 0 && !self.alternate_active {
            self.viewport_wrapped[row]
        } else {
            self.active().wrapped[row]
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
    fn home_cursor(&mut self) {
        self.set_cursor(if self.origin_mode { self.scroll_top } else { 0 }, 0);
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

    fn previous_cell_index(&self) -> Option<usize> {
        let cursor = self.active().cursor;
        let column = if self.active().wrap_pending {
            cursor.column
        } else {
            cursor.column.checked_sub(1)?
        };
        let mut index = self.index(cursor.row, column);
        if self.active().cells[index].is_continuation() && column > 0 {
            index -= 1;
        }
        Some(index)
    }

    fn should_join_previous(&self, character: char) -> bool {
        let Some(index) = self.previous_cell_index() else {
            return false;
        };
        let previous = &self.active().cells[index];
        previous.sequence_ends_with('\u{200d}')
            || is_emoji_modifier(character)
            || (is_regional_indicator(previous.character)
                && is_regional_indicator(character)
                && !previous.combining[..usize::from(previous.combining_len)]
                    .iter()
                    .copied()
                    .any(is_regional_indicator))
    }

    fn append_to_previous(&mut self, character: char, track_damage: bool) {
        if let Some(index) = self.previous_cell_index() {
            if (is_regional_indicator(character)
                && is_regional_indicator(self.active().cells[index].character))
                || character == '\u{20e3}'
            {
                self.promote_cell_to_wide(index);
            }
            self.active_mut().cells[index].append(character);
            if track_damage {
                self.damage_row(index / self.columns);
            }
            return;
        }

        // A leading combining mark has no base. A dotted circle makes the
        // otherwise invisible sequence inspectable without consuming extra cells.
        self.print_impl('\u{25cc}', track_damage);
        if let Some(index) = self.previous_cell_index() {
            self.active_mut().cells[index].append(character);
            if track_damage {
                self.damage_row(index / self.columns);
            }
        }
    }

    fn promote_cell_to_wide(&mut self, index: usize) {
        let column = index % self.columns;
        if self.active().cells[index].width != CellWidth::Single || column + 1 >= self.columns {
            return;
        }
        let attributes = self.active().cells[index];
        self.clear_cell_span(index / self.columns, column + 1);
        self.active_mut().cells[index].width = CellWidth::Wide;
        self.active_mut().cells[index + 1] = Cell {
            width: CellWidth::Continuation,
            foreground: attributes.foreground,
            background: attributes.background,
            flags: attributes.flags,
            ..Cell::default()
        };

        let cursor = self.active().cursor;
        if cursor.row == index / self.columns && !self.active().wrap_pending {
            if cursor.column + 1 >= self.columns {
                self.active_mut().cursor.column = self.columns - 1;
                self.active_mut().wrap_pending = self.auto_wrap;
            } else {
                self.active_mut().cursor.column += 1;
            }
        }
    }

    fn clear_cell_span(&mut self, row: usize, column: usize) {
        let blank = self.blank_cell();
        let index = self.index(row, column);
        match self.active().cells[index].width {
            CellWidth::Wide => {
                self.active_mut().cells[index] = blank;
                if column + 1 < self.columns {
                    self.active_mut().cells[index + 1] = blank;
                }
            }
            CellWidth::Continuation => {
                self.active_mut().cells[index] = blank;
                if column > 0 {
                    self.active_mut().cells[index - 1] = blank;
                }
            }
            CellWidth::Single => self.active_mut().cells[index] = blank,
        }
    }

    fn normalize_active_rows(&mut self, first: usize, last: usize) {
        let columns = self.columns;
        let blank = self.blank_cell();
        let cells = &mut self.active_mut().cells;
        for row in first..=last {
            normalize_row(cells, columns, row, blank);
        }
    }

    fn damage_selection(&mut self, selection: Selection) {
        self.damage_rows(
            selection.start.row.min(selection.end.row),
            selection.start.row.max(selection.end.row),
        );
    }

    fn damage_all(&mut self) {
        self.damage_rows(0, self.rows - 1);
    }

    #[inline]
    fn damage_row(&mut self, row: usize) {
        if self.damage_batching && self.pending_damage == Some((0, self.rows - 1)) {
            return;
        }
        self.damage_rows(row, row);
    }

    #[inline]
    fn damage_rows(&mut self, first: usize, last: usize) {
        if self.row_versions.is_empty() {
            return;
        }
        let first = first.min(self.row_versions.len() - 1);
        let last = last.min(self.row_versions.len() - 1);
        if first > last {
            return;
        }
        if self.damage_batching {
            if self.pending_damage == Some((0, self.rows - 1)) {
                return;
            }
            self.pending_damage = Some(
                self.pending_damage
                    .map_or((first, last), |(pending_first, pending_last)| {
                        (pending_first.min(first), pending_last.max(last))
                    }),
            );
            return;
        }
        self.apply_damage_rows(first, last);
    }

    fn apply_damage_rows(&mut self, first: usize, last: usize) {
        let version = self.next_damage_version;
        if version == u64::MAX {
            self.row_versions.fill(1);
            self.next_damage_version = 2;
            return;
        }
        self.row_versions[first..=last].fill(version);
        self.next_damage_version += 1;
    }
}

fn normalize_row(cells: &mut [Cell], columns: usize, row: usize, blank: Cell) {
    let start = row * columns;
    for column in 0..columns {
        let index = start + column;
        match cells[index].width {
            CellWidth::Wide
                if column + 1 == columns || cells[index + 1].width != CellWidth::Continuation =>
            {
                cells[index] = blank;
            }
            CellWidth::Continuation if column == 0 || cells[index - 1].width != CellWidth::Wide => {
                cells[index] = blank;
            }
            _ => {}
        }
    }
}

fn default_tab_stops(columns: usize) -> Vec<bool> {
    (0..columns).map(|column| column % TAB_WIDTH == 0).collect()
}

fn is_regional_indicator(character: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&character)
}

fn is_emoji_modifier(character: char) -> bool {
    ('\u{1f3fb}'..='\u{1f3ff}').contains(&character)
}

#[cfg(test)]
mod tests {
    use super::{CellWidth, Color, Cursor, Terminal};

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
    fn insert_delete_and_erase_character_operations_fill_vacated_cells() {
        let mut terminal = Terminal::new(1, 5);
        for character in "abcde".chars() {
            terminal.print(character);
        }
        terminal.set_background(Color::Indexed(4));
        terminal.set_cursor(0, 1);
        terminal.insert_characters(2);
        assert_eq!(row_text(&terminal, 0), "a  bc");
        assert_eq!(terminal.active().cells[1].background, Color::Indexed(4));
        terminal.delete_characters(1);
        assert_eq!(row_text(&terminal, 0), "a bc ");
        assert_eq!(terminal.active().cells[4].background, Color::Indexed(4));
        terminal.erase_characters(2);
        assert_eq!(row_text(&terminal, 0), "a  c ");
    }

    #[test]
    fn insert_and_delete_lines_only_mutate_the_scroll_region() {
        let mut terminal = Terminal::new(4, 2);
        for (row, text) in ["aa", "bb", "cc", "dd"].iter().enumerate() {
            terminal.set_cursor(row, 0);
            for character in text.chars() {
                terminal.print(character);
            }
        }
        terminal.set_scroll_region(1, 2);
        terminal.set_cursor(1, 0);
        terminal.insert_lines(1);
        assert_eq!(row_text(&terminal, 0), "aa");
        assert_eq!(row_text(&terminal, 1), "  ");
        assert_eq!(row_text(&terminal, 2), "bb");
        assert_eq!(row_text(&terminal, 3), "dd");
        terminal.delete_lines(1);
        assert_eq!(row_text(&terminal, 1), "bb");
        assert_eq!(row_text(&terminal, 2), "  ");
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
    fn alternate_screen_does_not_overwrite_saved_or_current_primary_cursor() {
        let mut terminal = Terminal::new(2, 4);
        terminal.set_cursor(0, 1);
        terminal.save_cursor();
        terminal.set_cursor(1, 3);
        terminal.use_alternate_screen(true, false);
        terminal.set_cursor(0, 0);
        terminal.use_alternate_screen(false, false);
        assert_eq!(terminal.active().cursor, Cursor { row: 1, column: 3 });
        terminal.restore_cursor();
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
    fn erase_saved_lines_clears_history_without_erasing_the_screen() {
        let mut terminal = Terminal::new(1, 2);
        for character in "abcd".chars() {
            terminal.print(character);
        }
        assert!(!terminal.history.is_empty());
        let visible_before = row_text(&terminal, 0);
        let version_before = terminal.render_snapshot().row_versions[0];
        terminal.erase_display(3);
        assert!(terminal.history.is_empty());
        assert!(terminal.history_wrapped.is_empty());
        assert_eq!(row_text(&terminal, 0), visible_before);
        assert_ne!(terminal.render_snapshot().row_versions[0], version_before);
    }

    #[test]
    fn alternate_screen_switch_preserves_scroll_margins() {
        let mut terminal = Terminal::new(4, 2);
        terminal.set_scroll_region(1, 2);
        terminal.use_alternate_screen(true, true);
        terminal.use_alternate_screen(false, false);
        assert_eq!((terminal.scroll_top, terminal.scroll_bottom), (1, 2));
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
    fn selection_joins_soft_wrapped_rows_and_does_not_move_cursor() {
        let mut terminal = Terminal::new(2, 3);
        for character in "abcdef".chars() {
            terminal.print(character);
        }
        let cursor_before_click = terminal.active().cursor;
        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 1, column: 2 });
        assert_eq!(terminal.active().cursor, cursor_before_click);
        assert_eq!(terminal.selected_text().as_deref(), Some("abcdef"));
    }

    #[test]
    fn normal_left_click_anchor_does_not_change_or_visually_replace_cursor() {
        let mut terminal = Terminal::new(3, 8);
        for character in "echo hi".chars() {
            terminal.print(character);
        }
        let cursor = terminal.active().cursor;

        terminal.begin_selection(Cursor { row: 1, column: 2 });

        let snapshot = terminal.render_snapshot();
        assert_eq!(snapshot.cursor, cursor);
        assert!(snapshot.selection.is_none());
        assert!(terminal.selected_text().is_none());
    }

    #[test]
    fn drag_selection_does_not_change_application_cursor() {
        let mut terminal = Terminal::new(3, 8);
        for character in "echo hi".chars() {
            terminal.print(character);
        }
        let cursor = terminal.active().cursor;

        terminal.begin_selection(Cursor { row: 0, column: 1 });
        terminal.update_selection(Cursor { row: 1, column: 4 });

        assert_eq!(terminal.active().cursor, cursor);
        assert!(terminal.render_snapshot().selection.is_some());
    }

    #[test]
    fn click_after_selection_clears_overlay_without_changing_cursor() {
        let mut terminal = Terminal::new(3, 8);
        for character in "echo hi".chars() {
            terminal.print(character);
        }
        let cursor = terminal.active().cursor;
        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 0, column: 3 });
        assert!(terminal.render_snapshot().selection.is_some());

        terminal.begin_selection(Cursor { row: 2, column: 6 });

        assert_eq!(terminal.active().cursor, cursor);
        assert!(terminal.render_snapshot().selection.is_none());
    }

    #[test]
    fn scrollback_mouse_selection_does_not_change_application_cursor() {
        let mut terminal = Terminal::new(2, 4);
        for character in "aaaabbbbcccc".chars() {
            terminal.print(character);
        }
        let cursor = terminal.active().cursor;
        terminal.scroll_viewport(1);
        assert!(!terminal.render_snapshot().cursor_visible);

        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 1, column: 2 });

        assert_eq!(terminal.active().cursor, cursor);
        assert!(terminal.render_snapshot().selection.is_some());
    }

    #[test]
    fn hard_line_feed_remains_a_newline_in_selection() {
        let mut terminal = Terminal::new(2, 3);
        terminal.print('a');
        terminal.line_feed();
        terminal.carriage_return();
        terminal.print('b');
        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 1, column: 1 });
        assert_eq!(terminal.selected_text().as_deref(), Some("a\nb"));
    }

    #[test]
    fn delayed_wrap_is_cancelled_by_carriage_return_and_backspace() {
        let mut terminal = Terminal::new(2, 3);
        for character in "abc".chars() {
            terminal.print(character);
        }
        assert_eq!(terminal.active().cursor, Cursor { row: 0, column: 2 });
        terminal.carriage_return();
        terminal.print('x');
        assert_eq!(row_text(&terminal, 0), "xbc");

        terminal.set_cursor(0, 2);
        terminal.print('z');
        terminal.backspace();
        terminal.print('y');
        assert_eq!(terminal.active().cursor.row, 0);
        assert_eq!(row_text(&terminal, 0), "xyz");
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

    #[test]
    fn repeated_resize_keeps_both_screens_history_and_cursors_valid() {
        let mut terminal = Terminal::new(4, 8);
        for character in "café界\r\n日本語\r\n".chars() {
            match character {
                '\r' => terminal.carriage_return(),
                '\n' => terminal.line_feed(),
                character => terminal.print(character),
            }
        }
        terminal.use_alternate_screen(true, true);
        for character in "한국어🚀".chars() {
            terminal.print(character);
        }

        for size in [
            super::GridSize {
                rows: 2,
                columns: 3,
            },
            super::GridSize {
                rows: 8,
                columns: 16,
            },
            super::GridSize {
                rows: 3,
                columns: 5,
            },
            super::GridSize {
                rows: 4,
                columns: 8,
            },
        ] {
            assert!(terminal.resize(size));
            let snapshot = terminal.render_snapshot();
            assert_eq!(snapshot.cells.len(), size.rows * size.columns);
            assert!(snapshot.cursor.row < size.rows);
            assert!(snapshot.cursor.column < size.columns);
            assert!(terminal.primary.cursor.row < size.rows);
            assert!(terminal.primary.cursor.column < size.columns);
        }

        terminal.use_alternate_screen(false, false);
        assert_eq!(terminal.render_snapshot().cells.len(), 4 * 8);
    }

    #[test]
    fn wide_characters_occupy_two_cells_and_wrap_atomically() {
        let mut terminal = Terminal::new(2, 3);
        terminal.print('a');
        terminal.print('界');
        assert_eq!(terminal.active().cells[1].width, CellWidth::Wide);
        assert_eq!(terminal.active().cells[2].width, CellWidth::Continuation);
        assert_eq!(terminal.active().cursor, Cursor { row: 0, column: 2 });

        terminal.print('語');
        assert_eq!(terminal.active().cells[3].character, '語');
        assert_eq!(terminal.active().cells[3].width, CellWidth::Wide);
        assert_eq!(terminal.active().cells[4].width, CellWidth::Continuation);
    }

    #[test]
    fn combining_marks_attach_without_advancing_the_cursor() {
        let mut terminal = Terminal::new(1, 4);
        terminal.print('e');
        terminal.print('\u{301}');
        assert_eq!(terminal.active().cursor, Cursor { row: 0, column: 1 });
        assert_eq!(
            terminal.active().cells[0].characters().collect::<String>(),
            "e\u{301}"
        );

        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 0, column: 0 });
        assert_eq!(terminal.selected_text().as_deref(), Some("e\u{301}"));
    }

    #[test]
    fn overwriting_a_wide_continuation_clears_the_whole_old_glyph() {
        let mut terminal = Terminal::new(1, 4);
        terminal.print('界');
        terminal.set_cursor(0, 1);
        terminal.print('x');
        assert_eq!(row_text(&terminal, 0), " x  ");
        assert_eq!(terminal.active().cells[0].width, CellWidth::Single);
    }

    #[test]
    fn emoji_clusters_keep_a_single_logical_cell_span() {
        let mut terminal = Terminal::new(1, 8);
        for character in "🇮🇳👩\u{200d}💻".chars() {
            terminal.print(character);
        }
        assert_eq!(terminal.active().cursor.column, 4);
        assert_eq!(
            terminal.active().cells[0].characters().collect::<String>(),
            "🇮🇳"
        );
        assert_eq!(
            terminal.active().cells[2].characters().collect::<String>(),
            "👩\u{200d}💻"
        );
        assert_eq!(terminal.active().cells[1].width, CellWidth::Continuation);
        assert_eq!(terminal.active().cells[3].width, CellWidth::Continuation);
    }

    #[test]
    fn damage_versions_identify_only_changed_rows() {
        let mut terminal = Terminal::new(3, 4);
        let initial = terminal.render_snapshot().row_versions.to_vec();
        terminal.set_cursor(1, 0);
        terminal.print('x');
        let changed = terminal.render_snapshot().row_versions;
        assert_ne!(changed[0], initial[0]);
        assert_ne!(changed[1], initial[1]);
        assert_eq!(changed[2], initial[2]);
    }

    #[test]
    fn reset_and_resize_force_fresh_full_grid_damage() {
        let mut terminal = Terminal::new(2, 3);
        let initial = terminal.render_snapshot().row_versions.to_vec();
        terminal.reset();
        assert!(
            terminal
                .render_snapshot()
                .row_versions
                .iter()
                .zip(&initial)
                .all(|(reset, old)| reset != old)
        );

        let reset = terminal.render_snapshot().row_versions.to_vec();
        terminal.resize(super::GridSize {
            rows: 2,
            columns: 4,
        });
        assert!(
            terminal
                .render_snapshot()
                .row_versions
                .iter()
                .zip(&reset)
                .all(|(resized, old)| resized != old)
        );
    }
}
