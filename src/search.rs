use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::terminal::{Cell, SearchContext, Terminal};

pub const MAX_SEARCH_QUERY_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchMatch {
    /// Index in the terminal's current searchable row space. Primary rows are
    /// ordered from the oldest history row through the live grid; the
    /// alternate screen contains only its live grid.
    pub row: usize,
    /// Inclusive leading cell column.
    pub start_cell: usize,
    /// Exclusive cell column, including a wide glyph's continuation column.
    pub end_cell: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchInputOutcome {
    Ignored,
    Consumed,
    QueryChanged,
    Navigate(SearchDirection),
    Closed,
}

impl SearchInputOutcome {
    pub fn needs_redraw(self) -> bool {
        matches!(self, Self::QueryChanged | Self::Navigate(_) | Self::Closed)
    }
}

#[derive(Debug, Default)]
pub struct SearchState {
    active: bool,
    query: String,
    current_match: Option<SearchMatch>,
    match_context: Option<SearchContext>,
    row_text: String,
    cell_spans: Vec<CellTextSpan>,
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn open(&mut self) -> bool {
        let changed = !self.active;
        self.active = true;
        changed
    }

    pub fn close(&mut self) -> bool {
        let changed = self.active;
        self.active = false;
        self.current_match = None;
        self.match_context = None;
        changed
    }

    pub fn find_next(
        &mut self,
        terminal: &Terminal,
        direction: SearchDirection,
    ) -> Option<SearchMatch> {
        self.rebase_current_match(terminal.search_context(), terminal.searchable_row_count());
        let found = find_match(
            terminal,
            &self.query,
            self.current_match,
            direction,
            &mut self.row_text,
            &mut self.cell_spans,
        );
        self.current_match = found;
        self.match_context = Some(terminal.search_context());
        found
    }

    pub fn refresh_current(&mut self, terminal: &Terminal) -> Option<SearchMatch> {
        if !self.active || self.query.is_empty() {
            self.current_match = None;
            self.match_context = Some(terminal.search_context());
            return None;
        }
        self.rebase_current_match(terminal.search_context(), terminal.searchable_row_count());
        if let Some(current) = self.current_match
            && find_in_row(
                terminal,
                current.row,
                &self.query,
                RowMatchSelection::Exact(current),
                &mut self.row_text,
                &mut self.cell_spans,
            ) == Some(current)
        {
            self.match_context = Some(terminal.search_context());
            return Some(current);
        }

        let found = find_match(
            terminal,
            &self.query,
            self.current_match,
            SearchDirection::Forward,
            &mut self.row_text,
            &mut self.cell_spans,
        );
        self.current_match = found;
        self.match_context = Some(terminal.search_context());
        found
    }

    pub fn handle_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        modifiers: ModifiersState,
    ) -> SearchInputOutcome {
        if !self.active {
            return SearchInputOutcome::Ignored;
        }

        match key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                SearchInputOutcome::Closed
            }
            Key::Named(NamedKey::Enter) => {
                let direction = if modifiers.shift_key() {
                    SearchDirection::Backward
                } else {
                    SearchDirection::Forward
                };
                SearchInputOutcome::Navigate(direction)
            }
            Key::Named(NamedKey::Backspace) => {
                if self.query.pop().is_some() {
                    self.current_match = None;
                    SearchInputOutcome::QueryChanged
                } else {
                    SearchInputOutcome::Consumed
                }
            }
            _ if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() => {
                SearchInputOutcome::Consumed
            }
            _ => {
                let Some(text) = text else {
                    return SearchInputOutcome::Consumed;
                };
                if self.append_text(text) {
                    self.current_match = None;
                    SearchInputOutcome::QueryChanged
                } else {
                    SearchInputOutcome::Consumed
                }
            }
        }
    }

    fn append_text(&mut self, text: &str) -> bool {
        let initial_len = self.query.len();
        for character in text.chars().filter(|character| !character.is_control()) {
            if self.query.len() + character.len_utf8() > MAX_SEARCH_QUERY_BYTES {
                break;
            }
            self.query.push(character);
        }
        self.query.len() != initial_len
    }

    fn rebase_current_match(&mut self, context: SearchContext, row_count: usize) {
        let Some(previous_context) = self.match_context else {
            self.match_context = Some(context);
            return;
        };
        if previous_context.screen_generation != context.screen_generation
            || previous_context.alternate_active != context.alternate_active
        {
            self.current_match = None;
            self.match_context = Some(context);
            return;
        }
        if !context.alternate_active && context.row_origin != previous_context.row_origin {
            let Some(discarded) = context
                .row_origin
                .checked_sub(previous_context.row_origin)
                .and_then(|count| usize::try_from(count).ok())
            else {
                self.current_match = None;
                self.match_context = Some(context);
                return;
            };
            self.current_match = self.current_match.and_then(|mut search_match| {
                search_match.row = search_match.row.checked_sub(discarded)?;
                Some(search_match)
            });
        }
        if self
            .current_match
            .is_some_and(|search_match| search_match.row >= row_count)
        {
            self.current_match = None;
        }
        self.match_context = Some(context);
    }
}

#[derive(Debug, Clone, Copy)]
struct CellTextSpan {
    byte_start: usize,
    byte_end: usize,
    start_cell: usize,
    end_cell: usize,
}

#[derive(Debug, Clone, Copy)]
enum RowMatchSelection {
    First,
    Last,
    FirstAfter(SearchMatch),
    LastBefore(SearchMatch),
    Exact(SearchMatch),
}

fn find_match(
    terminal: &Terminal,
    query: &str,
    current_match: Option<SearchMatch>,
    direction: SearchDirection,
    row_text: &mut String,
    cell_spans: &mut Vec<CellTextSpan>,
) -> Option<SearchMatch> {
    let row_count = terminal.searchable_row_count();
    if query.is_empty() || row_count == 0 {
        return None;
    }
    let current_match = current_match.filter(|search_match| search_match.row < row_count);
    let visible_rows = terminal.searchable_visible_rows();

    match (direction, current_match) {
        (SearchDirection::Forward, None) => {
            for row in visible_rows.start..row_count {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::First,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            for row in 0..visible_rows.start {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::First,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            None
        }
        (SearchDirection::Backward, None) => {
            for row in (0..visible_rows.end).rev() {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::Last,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            for row in (visible_rows.end..row_count).rev() {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::Last,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            None
        }
        (SearchDirection::Forward, Some(current)) => {
            if let Some(found) = find_in_row(
                terminal,
                current.row,
                query,
                RowMatchSelection::FirstAfter(current),
                row_text,
                cell_spans,
            ) {
                return Some(found);
            }
            for row in current.row + 1..row_count {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::First,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            for row in 0..current.row {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::First,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            find_in_row(
                terminal,
                current.row,
                query,
                RowMatchSelection::First,
                row_text,
                cell_spans,
            )
        }
        (SearchDirection::Backward, Some(current)) => {
            if let Some(found) = find_in_row(
                terminal,
                current.row,
                query,
                RowMatchSelection::LastBefore(current),
                row_text,
                cell_spans,
            ) {
                return Some(found);
            }
            for row in (0..current.row).rev() {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::Last,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            for row in (current.row + 1..row_count).rev() {
                if let Some(found) = find_in_row(
                    terminal,
                    row,
                    query,
                    RowMatchSelection::Last,
                    row_text,
                    cell_spans,
                ) {
                    return Some(found);
                }
            }
            find_in_row(
                terminal,
                current.row,
                query,
                RowMatchSelection::Last,
                row_text,
                cell_spans,
            )
        }
    }
}

fn find_in_row(
    terminal: &Terminal,
    row: usize,
    query: &str,
    selection: RowMatchSelection,
    row_text: &mut String,
    cell_spans: &mut Vec<CellTextSpan>,
) -> Option<SearchMatch> {
    let cells = terminal.searchable_row(row)?;
    extract_row(cells, row_text, cell_spans);

    let mut search_from = 0;
    let mut selected = None;
    let mut previous = None;
    while search_from < row_text.len() {
        let Some(relative_start) = row_text[search_from..].find(query) else {
            break;
        };
        let byte_start = search_from + relative_start;
        let byte_end = byte_start + query.len();
        let Some(found) = match_to_cells(row, byte_start, byte_end, cell_spans) else {
            break;
        };

        if previous != Some(found) {
            match selection {
                RowMatchSelection::First => return Some(found),
                RowMatchSelection::Last => selected = Some(found),
                RowMatchSelection::FirstAfter(anchor) if found > anchor => return Some(found),
                RowMatchSelection::LastBefore(anchor) if found < anchor => selected = Some(found),
                RowMatchSelection::LastBefore(anchor) if found >= anchor => break,
                RowMatchSelection::Exact(anchor) if found == anchor => return Some(found),
                RowMatchSelection::Exact(anchor) if found > anchor => break,
                RowMatchSelection::FirstAfter(_)
                | RowMatchSelection::LastBefore(_)
                | RowMatchSelection::Exact(_) => {}
            }
            previous = Some(found);
        }

        let character_len = row_text[byte_start..]
            .chars()
            .next()
            .expect("a non-empty match starts on a character boundary")
            .len_utf8();
        search_from = byte_start + character_len;
    }
    selected
}

fn extract_row(cells: &[Cell], row_text: &mut String, cell_spans: &mut Vec<CellTextSpan>) {
    row_text.clear();
    cell_spans.clear();
    for (column, cell) in cells.iter().enumerate() {
        if cell.is_continuation() {
            continue;
        }
        let byte_start = row_text.len();
        row_text.extend(cell.characters());
        let byte_end = row_text.len();
        cell_spans.push(CellTextSpan {
            byte_start,
            byte_end,
            start_cell: column,
            end_cell: (column + cell.width.columns()).min(cells.len()),
        });
    }
}

fn match_to_cells(
    row: usize,
    byte_start: usize,
    byte_end: usize,
    cell_spans: &[CellTextSpan],
) -> Option<SearchMatch> {
    let first = cell_spans.iter().find(|span| span.byte_end > byte_start)?;
    let last = cell_spans
        .iter()
        .rev()
        .find(|span| span.byte_start < byte_end)?;
    Some(SearchMatch {
        row,
        start_cell: first.start_cell,
        end_cell: last.end_cell,
    })
}

#[cfg(test)]
mod tests {
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    use super::{
        MAX_SEARCH_QUERY_BYTES, SearchDirection, SearchInputOutcome, SearchMatch, SearchState,
    };
    use crate::terminal::{Cell, Cursor, GridSize, MouseTracking, Terminal, TerminalParser};

    fn search_state(query: &str) -> SearchState {
        SearchState {
            active: true,
            query: query.to_owned(),
            ..SearchState::default()
        }
    }

    fn visible_row_text(terminal: &Terminal, row: usize) -> String {
        let snapshot = terminal.render_snapshot();
        snapshot.cells[row * snapshot.columns..(row + 1) * snapshot.columns]
            .iter()
            .filter(|cell| !cell.is_continuation())
            .flat_map(Cell::characters)
            .collect()
    }

    #[test]
    fn inactive_search_does_not_consume_input() {
        let mut search = SearchState::default();
        assert_eq!(
            search.handle_key(
                &Key::Character("x".into()),
                Some("x"),
                ModifiersState::empty(),
            ),
            SearchInputOutcome::Ignored
        );
        assert!(search.query.is_empty());
    }

    #[test]
    fn search_edits_unicode_query_locally_and_closes_with_escape() {
        let mut search = SearchState::default();
        assert!(search.open());
        assert!(!search.open());
        assert_eq!(
            search.handle_key(
                &Key::Character("é".into()),
                Some("é日"),
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query, "é日");
        assert_eq!(
            search.handle_key(
                &Key::Named(NamedKey::Backspace),
                None,
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query, "é");
        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(),),
            SearchInputOutcome::Closed
        );
        assert!(!search.is_active());
        assert_eq!(search.query, "é");
    }

    #[test]
    fn enter_produces_directional_navigation_intents() {
        let mut search = SearchState::default();
        search.open();
        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),),
            SearchInputOutcome::Navigate(SearchDirection::Forward)
        );
        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT,),
            SearchInputOutcome::Navigate(SearchDirection::Backward)
        );
    }

    #[test]
    fn modified_shortcuts_are_consumed_without_entering_the_query() {
        let mut search = SearchState::default();
        search.open();
        assert_eq!(
            search.handle_key(
                &Key::Character("c".into()),
                Some("c"),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
            SearchInputOutcome::Consumed
        );
        assert!(search.query.is_empty());
    }

    #[test]
    fn query_storage_is_bounded_at_a_utf8_boundary() {
        let mut search = SearchState::default();
        search.open();
        let oversized = "é".repeat(MAX_SEARCH_QUERY_BYTES);
        assert_eq!(
            search.handle_key(
                &Key::Character("é".into()),
                Some(&oversized),
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query.len(), MAX_SEARCH_QUERY_BYTES);
        assert!(search.query.is_char_boundary(search.query.len()));
    }

    #[test]
    fn local_search_input_does_not_mutate_the_terminal_cursor() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(3, 12);
        parser.process(&mut terminal, b"prompt");
        let cursor = terminal.render_snapshot().cursor;

        let mut search = SearchState::default();
        search.open();
        assert_eq!(
            search.handle_key(
                &Key::Character("x".into()),
                Some("x"),
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );

        assert_eq!(terminal.render_snapshot().cursor, cursor);
    }

    #[test]
    fn exact_search_maps_wide_and_combining_text_to_terminal_cells() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 8);
        parser.process(&mut terminal, "a界e\u{301}".as_bytes());

        let mut search = search_state("界e\u{301}");
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(SearchMatch {
                row: 0,
                start_cell: 1,
                end_cell: 4,
            })
        );
    }

    #[test]
    fn matching_ignores_cell_styles_and_remains_case_sensitive() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 16);
        parser.process(&mut terminal, b"\x1b[31mFl\x1b[1;34mash\x1b[0m flash");

        let mut exact = search_state("Flash");
        assert_eq!(
            exact.find_next(&terminal, SearchDirection::Forward),
            Some(SearchMatch {
                row: 0,
                start_cell: 0,
                end_cell: 5,
            })
        );
        let mut wrong_case = search_state("FLASH");
        assert_eq!(
            wrong_case.find_next(&terminal, SearchDirection::Forward),
            None
        );
    }

    #[test]
    fn forward_and_backward_navigation_wrap_across_overlapping_matches() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 8);
        parser.process(&mut terminal, b"banana");
        let first = SearchMatch {
            row: 0,
            start_cell: 1,
            end_cell: 4,
        };
        let second = SearchMatch {
            row: 0,
            start_cell: 3,
            end_cell: 6,
        };

        let mut search = search_state("ana");
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(first)
        );
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(second)
        );
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(first)
        );
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Backward),
            Some(second)
        );
    }

    #[test]
    fn search_traverses_primary_history_and_live_rows_without_collecting_matches() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"hit old\r\nmiss\r\nhit new");
        assert_eq!(terminal.searchable_row_count(), 3);

        let mut search = search_state("hit");
        let history_match = SearchMatch {
            row: 0,
            start_cell: 0,
            end_cell: 3,
        };
        let live_match = SearchMatch {
            row: 2,
            start_cell: 0,
            end_cell: 3,
        };
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(live_match)
        );
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(history_match)
        );
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Backward),
            Some(live_match)
        );
        assert_eq!(search.current_match, Some(live_match));
    }

    #[test]
    fn alternate_screen_search_is_isolated_from_primary_history() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 12);
        parser.process(&mut terminal, b"primary");

        let mut primary_search = search_state("primary");
        assert!(
            primary_search
                .find_next(&terminal, SearchDirection::Forward)
                .is_some()
        );

        terminal.use_alternate_screen(true, true);
        parser.process(&mut terminal, b"alternate");
        let mut hidden_primary_search = search_state("primary");
        assert_eq!(
            hidden_primary_search.find_next(&terminal, SearchDirection::Forward),
            None
        );
        let mut alternate_search = search_state("alternate");
        assert_eq!(
            alternate_search.find_next(&terminal, SearchDirection::Forward),
            Some(SearchMatch {
                row: 0,
                start_cell: 0,
                end_cell: 9,
            })
        );

        terminal.use_alternate_screen(false, false);
        let mut restored_primary_search = search_state("primary");
        assert!(
            restored_primary_search
                .find_next(&terminal, SearchDirection::Forward)
                .is_some()
        );
    }

    #[test]
    fn matching_does_not_mutate_terminal_cursor_or_selection() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 12);
        parser.process(&mut terminal, b"find this");
        terminal.begin_selection(crate::terminal::Cursor { row: 0, column: 0 });
        terminal.update_selection(crate::terminal::Cursor { row: 0, column: 3 });
        let before = terminal.render_snapshot();
        let cursor = before.cursor;
        let selection = before.selection;

        let mut search = search_state("this");
        let found = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("visible row contains the query");
        assert!(!terminal.reveal_search_row(found.row));

        let after = terminal.render_snapshot();
        assert_eq!(after.cursor, cursor);
        assert_eq!(after.selection, selection);
    }

    #[test]
    fn revealing_a_history_match_uses_the_viewport_without_moving_the_cursor() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"needle\r\nother\r\nlive");
        let cursor = terminal.render_snapshot().cursor;
        terminal.begin_selection(Cursor { row: 0, column: 0 });
        terminal.update_selection(Cursor { row: 0, column: 2 });

        let mut search = search_state("needle");
        let found = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("history contains the query");
        assert_eq!(found.row, 0);
        assert!(terminal.reveal_search_row(found.row));

        assert_eq!(terminal.render_snapshot().cursor, cursor);
        assert!(terminal.render_snapshot().selection.is_none());
        assert!(visible_row_text(&terminal, 0).starts_with("needle"));
        assert!(terminal.searchable_visible_rows().contains(&found.row));
    }

    #[test]
    fn initial_search_begins_at_the_current_viewport_and_then_wraps() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"hit old\r\nother\r\nhit new");

        let mut search = search_state("hit");
        let live = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("live grid contains the query");
        assert_eq!(live.row, 2);
        let wrapped = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("history contains the query");
        assert_eq!(wrapped.row, 0);
    }

    #[test]
    fn active_live_match_keeps_its_identity_when_output_scrolls_into_history() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"other\r\nneedle");
        let mut search = search_state("needle");
        let original = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("live grid contains the query");
        assert_eq!(original.row, 1);

        terminal.set_cursor(1, 0);
        terminal.line_feed();
        let cursor = terminal.render_snapshot().cursor;
        let refreshed = search
            .refresh_current(&terminal)
            .expect("the same line moved into history-backed row space");

        assert_eq!(refreshed, original);
        assert_eq!(terminal.render_snapshot().cursor, cursor);
    }

    #[test]
    fn zero_scrollback_rebases_a_live_match_when_the_top_row_is_discarded() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        terminal.set_scrollback_limit(0);
        parser.process(&mut terminal, b"other\r\nneedle");
        let mut search = search_state("needle");
        let before = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("bottom row contains the query");
        assert_eq!(before.row, 1);

        terminal.set_cursor(1, 0);
        terminal.line_feed();
        let after = search
            .refresh_current(&terminal)
            .expect("the bottom row moved up while the top row was discarded");

        assert_eq!(after.row, 0);
        assert_eq!(terminal.searchable_row_count(), 2);
    }

    #[test]
    fn history_clear_rebases_a_surviving_live_match() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"old\r\nother\r\nneedle");
        let mut search = search_state("needle");
        let before = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("live grid contains the query");
        assert_eq!(before.row, 2);
        let origin = terminal.search_context().row_origin;

        terminal.erase_display(3);
        let after = search
            .refresh_current(&terminal)
            .expect("clearing history preserves live grid contents");

        assert_eq!(after.row, 1);
        assert_eq!(after.start_cell, before.start_cell);
        assert_eq!(terminal.search_context().row_origin, origin.wrapping_add(1));
    }

    #[test]
    fn history_eviction_invalidates_a_discarded_active_match() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        terminal.set_scrollback_limit(1);
        parser.process(&mut terminal, b"needle\r\nother\r\nlive");
        let mut search = search_state("needle");
        assert_eq!(
            search
                .find_next(&terminal, SearchDirection::Forward)
                .map(|found| found.row),
            Some(0)
        );
        let origin = terminal.search_context().row_origin;

        terminal.set_cursor(1, 0);
        terminal.line_feed();

        assert_eq!(terminal.search_context().row_origin, origin.wrapping_add(1));
        assert_eq!(search.refresh_current(&terminal), None);
        assert_eq!(search.current_match, None);
    }

    #[test]
    fn reducing_scrollback_rebases_a_surviving_live_match() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"old one\r\nold two\r\nother\r\nneedle");
        assert_eq!(terminal.searchable_row_count(), 4);
        let mut search = search_state("needle");
        let before = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("live grid contains the query");
        assert_eq!(before.row, 3);

        terminal.set_scrollback_limit(1);
        let after = search
            .refresh_current(&terminal)
            .expect("the live match survives history truncation");

        assert_eq!(after.row, 2);
        assert_eq!(after.start_cell, before.start_cell);
        assert_eq!(terminal.searchable_row_count(), 3);
    }

    #[test]
    fn resize_revalidates_cell_spans_and_drops_truncated_matches() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"needle");
        let mut search = search_state("needle");
        assert!(
            search
                .find_next(&terminal, SearchDirection::Forward)
                .is_some()
        );

        assert!(terminal.resize(GridSize {
            rows: 2,
            columns: 4,
        }));

        assert_eq!(search.refresh_current(&terminal), None);
    }

    #[test]
    fn screen_switch_and_reset_invalidate_the_previous_search_context() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"primary");
        let mut search = search_state("primary");
        assert!(
            search
                .find_next(&terminal, SearchDirection::Forward)
                .is_some()
        );
        let primary_generation = search
            .match_context
            .expect("a match records its search context")
            .screen_generation;

        terminal.use_alternate_screen(true, true);
        assert_eq!(search.refresh_current(&terminal), None);
        assert_ne!(
            search
                .match_context
                .expect("refresh records the alternate context")
                .screen_generation,
            primary_generation
        );

        terminal.reset();
        assert_eq!(search.refresh_current(&terminal), None);
    }

    #[test]
    fn search_navigation_preserves_mouse_reporting_modes() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"needle\r\nother\r\nlive\x1b[?1002;1006h");
        assert_eq!(terminal.mouse_tracking(), MouseTracking::ButtonMotion);
        assert!(terminal.sgr_mouse());

        let mut search = search_state("needle");
        let found = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("history contains the query");
        terminal.reveal_search_row(found.row);

        assert_eq!(terminal.mouse_tracking(), MouseTracking::ButtonMotion);
        assert!(terminal.sgr_mouse());
    }
}
