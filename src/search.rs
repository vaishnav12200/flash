use std::time::{Duration, Instant};

use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::terminal::{Cell, SearchContext, Terminal};

pub const MAX_SEARCH_QUERY_BYTES: usize = 4 * 1024;
const MAX_SEARCH_ROWS_PER_SLICE: usize = 16 * 1024;

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
pub(crate) struct VisibleSearchMatch {
    /// Row relative to the currently rendered viewport.
    pub row: usize,
    pub start_cell: usize,
    pub end_cell: usize,
    pub active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchPresentation<'a> {
    pub active: bool,
    pub query: &'a str,
    pub caret: usize,
    pub matches: &'a [VisibleSearchMatch],
    pub row_versions: &'a [u64],
    pub ui_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchInputOutcome {
    Ignored,
    Consumed,
    QueryChanged,
    CaretMoved,
    Navigate(SearchDirection),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchProgress {
    Idle,
    Pending,
    Complete(Option<SearchMatch>),
}

#[derive(Debug, Default)]
pub struct SearchState {
    active: bool,
    query: String,
    caret: usize,
    current_match: Option<SearchMatch>,
    match_context: Option<SearchContext>,
    row_text: String,
    cell_spans: Vec<CellTextSpan>,
    pending_search: Option<PendingSearch>,
    visible_matches: Vec<VisibleSearchMatch>,
    visible_matches_scratch: Vec<VisibleSearchMatch>,
    visible_row_versions: Vec<u64>,
    ui_version: u64,
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn pending_direction(&self) -> Option<SearchDirection> {
        self.pending_search.map(|pending| pending.direction)
    }

    pub fn open(&mut self) -> bool {
        let changed = !self.active;
        self.active = true;
        if changed {
            self.caret = self.query.len();
            self.ui_version = self.ui_version.wrapping_add(1);
        }
        changed
    }

    pub fn close(&mut self) -> bool {
        let changed = self.active;
        self.active = false;
        self.current_match = None;
        self.match_context = None;
        self.pending_search = None;
        if changed {
            self.ui_version = self.ui_version.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn presentation(&self) -> SearchPresentation<'_> {
        SearchPresentation {
            active: self.active,
            query: &self.query,
            caret: self.caret,
            matches: &self.visible_matches,
            row_versions: &self.visible_row_versions,
            ui_version: self.ui_version,
        }
    }

    pub(crate) fn insert_text(&mut self, text: &str) -> bool {
        if !self.active {
            return false;
        }
        let initial_len = self.query.len();
        for character in text.chars().filter(|character| !character.is_control()) {
            if self.query.len() + character.len_utf8() > MAX_SEARCH_QUERY_BYTES {
                break;
            }
            self.query.insert(self.caret, character);
            self.caret += character.len_utf8();
        }
        if self.query.len() == initial_len {
            return false;
        }
        self.current_match = None;
        self.pending_search = None;
        self.ui_version = self.ui_version.wrapping_add(1);
        true
    }

    /// Recomputes only the matches that can currently be rendered. The
    /// retained list is bounded by the visible grid rather than scrollback.
    pub(crate) fn refresh_visible_matches(&mut self, terminal: &Terminal) {
        let started_at = Instant::now();
        let visible_rows = terminal.searchable_visible_rows();
        let visible_row_count = visible_rows.len();
        self.visible_matches_scratch.clear();

        if self.active && !self.query.is_empty() {
            for (viewport_row, searchable_row) in visible_rows.enumerate() {
                collect_matches_in_row(
                    terminal,
                    searchable_row,
                    viewport_row,
                    &self.query,
                    self.current_match,
                    VisibleMatchScratch {
                        row_text: &mut self.row_text,
                        cell_spans: &mut self.cell_spans,
                        output: &mut self.visible_matches_scratch,
                    },
                );
            }
        }

        self.visible_row_versions.resize(visible_row_count, 0);
        for row in 0..visible_row_count {
            let old = self
                .visible_matches
                .iter()
                .filter(|search_match| search_match.row == row);
            let new = self
                .visible_matches_scratch
                .iter()
                .filter(|search_match| search_match.row == row);
            if !old.eq(new) {
                self.visible_row_versions[row] = self.visible_row_versions[row].wrapping_add(1);
            }
        }
        std::mem::swap(&mut self.visible_matches, &mut self.visible_matches_scratch);
        tracing::debug!(
            query_bytes = self.query.len(),
            visible_rows = visible_row_count,
            visible_matches = self.visible_matches.len(),
            visible_scan_us = started_at.elapsed().as_micros(),
            "search visible presentation refreshed"
        );
    }

    #[cfg(test)]
    pub fn find_next(
        &mut self,
        terminal: &Terminal,
        direction: SearchDirection,
    ) -> Option<SearchMatch> {
        self.pending_search = None;
        let started_at = Instant::now();
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
        tracing::debug!(
            query_bytes = self.query.len(),
            searchable_rows = terminal.searchable_row_count(),
            direction = ?direction,
            found = found.is_some(),
            search_us = started_at.elapsed().as_micros(),
            "search synchronous scan complete"
        );
        found
    }

    pub(crate) fn begin_search(&mut self, terminal: &Terminal, direction: SearchDirection) {
        self.rebase_current_match(terminal.search_context(), terminal.searchable_row_count());
        let row_count = terminal.searchable_row_count();
        if !self.active || self.query.is_empty() || row_count == 0 {
            self.current_match = None;
            self.match_context = Some(terminal.search_context());
            self.pending_search = None;
            return;
        }

        let anchor = self
            .current_match
            .filter(|search_match| search_match.row < row_count);
        let visible_rows = terminal.searchable_visible_rows();
        let start_row = match (direction, anchor) {
            (_, Some(current)) => current.row,
            (SearchDirection::Forward, None) => visible_rows.start.min(row_count - 1),
            (SearchDirection::Backward, None) => {
                visible_rows.end.saturating_sub(1).min(row_count - 1)
            }
        };
        self.pending_search = Some(PendingSearch {
            direction,
            anchor,
            context: terminal.search_context(),
            row_count,
            start_row,
            visit_index: 0,
            total_visits: row_count + usize::from(anchor.is_some()),
            rows_scanned: 0,
            started_at: Instant::now(),
        });
    }

    pub(crate) fn continue_search(
        &mut self,
        terminal: &Terminal,
        time_budget: Duration,
    ) -> SearchProgress {
        let Some(mut pending) = self.pending_search.take() else {
            return SearchProgress::Idle;
        };
        if pending.context != terminal.search_context()
            || pending.row_count != terminal.searchable_row_count()
        {
            let direction = pending.direction;
            self.begin_search(terminal, direction);
            let Some(restarted) = self.pending_search.take() else {
                return SearchProgress::Complete(None);
            };
            pending = restarted;
        }

        let slice_started_at = Instant::now();
        let mut slice_rows = 0;
        while slice_rows < MAX_SEARCH_ROWS_PER_SLICE
            && (slice_rows == 0 || slice_started_at.elapsed() < time_budget)
        {
            let Some((row, selection)) = pending.next_visit() else {
                self.current_match = None;
                self.match_context = Some(terminal.search_context());
                tracing::debug!(
                    query_bytes = self.query.len(),
                    direction = ?pending.direction,
                    rows_scanned = pending.rows_scanned,
                    search_us = pending.started_at.elapsed().as_micros(),
                    found = false,
                    "search incremental scan complete"
                );
                return SearchProgress::Complete(None);
            };
            slice_rows += 1;
            pending.rows_scanned += 1;
            if let Some(found) = find_in_row(
                terminal,
                row,
                &self.query,
                selection,
                &mut self.row_text,
                &mut self.cell_spans,
            ) {
                self.current_match = Some(found);
                self.match_context = Some(terminal.search_context());
                tracing::debug!(
                    query_bytes = self.query.len(),
                    direction = ?pending.direction,
                    rows_scanned = pending.rows_scanned,
                    search_us = pending.started_at.elapsed().as_micros(),
                    found = true,
                    "search incremental scan complete"
                );
                return SearchProgress::Complete(Some(found));
            }
        }

        tracing::debug!(
            query_bytes = self.query.len(),
            direction = ?pending.direction,
            slice_rows,
            total_rows_scanned = pending.rows_scanned,
            slice_us = slice_started_at.elapsed().as_micros(),
            "search incremental scan yielded"
        );
        self.pending_search = Some(pending);
        SearchProgress::Pending
    }

    pub(crate) fn revalidate_current(&mut self, terminal: &Terminal) -> Option<SearchMatch> {
        self.pending_search = None;
        if !self.active || self.query.is_empty() {
            self.current_match = None;
            self.match_context = Some(terminal.search_context());
            return None;
        }
        self.rebase_current_match(terminal.search_context(), terminal.searchable_row_count());
        let current = self.current_match?;
        if find_in_row(
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
        self.current_match = None;
        self.match_context = Some(terminal.search_context());
        None
    }

    #[cfg(test)]
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
                if self.delete_before_caret() {
                    self.current_match = None;
                    self.pending_search = None;
                    self.ui_version = self.ui_version.wrapping_add(1);
                    SearchInputOutcome::QueryChanged
                } else {
                    SearchInputOutcome::Consumed
                }
            }
            Key::Named(NamedKey::Delete) => {
                if self.delete_at_caret() {
                    self.current_match = None;
                    self.pending_search = None;
                    self.ui_version = self.ui_version.wrapping_add(1);
                    SearchInputOutcome::QueryChanged
                } else {
                    SearchInputOutcome::Consumed
                }
            }
            Key::Named(NamedKey::ArrowLeft) => self.move_caret_left(),
            Key::Named(NamedKey::ArrowRight) => self.move_caret_right(),
            Key::Named(NamedKey::Home) => self.move_caret_to(0),
            Key::Named(NamedKey::End) => self.move_caret_to(self.query.len()),
            _ if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() => {
                SearchInputOutcome::Consumed
            }
            _ => {
                let Some(text) = text else {
                    return SearchInputOutcome::Consumed;
                };
                if self.insert_text(text) {
                    SearchInputOutcome::QueryChanged
                } else {
                    SearchInputOutcome::Consumed
                }
            }
        }
    }

    fn move_caret_left(&mut self) -> SearchInputOutcome {
        let target = self.query[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.move_caret_to(target)
    }

    fn move_caret_right(&mut self) -> SearchInputOutcome {
        let target = self.query[self.caret..]
            .chars()
            .next()
            .map_or(self.query.len(), |character| {
                self.caret + character.len_utf8()
            });
        self.move_caret_to(target)
    }

    fn move_caret_to(&mut self, target: usize) -> SearchInputOutcome {
        if target == self.caret {
            return SearchInputOutcome::Consumed;
        }
        self.caret = target;
        self.ui_version = self.ui_version.wrapping_add(1);
        SearchInputOutcome::CaretMoved
    }

    fn delete_before_caret(&mut self) -> bool {
        let Some((start, _)) = self.query[..self.caret].char_indices().next_back() else {
            return false;
        };
        self.query.drain(start..self.caret);
        self.caret = start;
        true
    }

    fn delete_at_caret(&mut self) -> bool {
        let Some(character) = self.query[self.caret..].chars().next() else {
            return false;
        };
        self.query
            .drain(self.caret..self.caret + character.len_utf8());
        true
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

struct VisibleMatchScratch<'a> {
    row_text: &'a mut String,
    cell_spans: &'a mut Vec<CellTextSpan>,
    output: &'a mut Vec<VisibleSearchMatch>,
}

#[derive(Debug, Clone, Copy)]
struct PendingSearch {
    direction: SearchDirection,
    anchor: Option<SearchMatch>,
    context: SearchContext,
    row_count: usize,
    start_row: usize,
    visit_index: usize,
    total_visits: usize,
    rows_scanned: usize,
    started_at: Instant,
}

impl PendingSearch {
    fn next_visit(&mut self) -> Option<(usize, RowMatchSelection)> {
        if self.visit_index >= self.total_visits {
            return None;
        }
        let index = self.visit_index;
        self.visit_index += 1;

        match (self.direction, self.anchor, index) {
            (SearchDirection::Forward, Some(anchor), 0) => {
                Some((anchor.row, RowMatchSelection::FirstAfter(anchor)))
            }
            (SearchDirection::Backward, Some(anchor), 0) => {
                Some((anchor.row, RowMatchSelection::LastBefore(anchor)))
            }
            (SearchDirection::Forward, _, _) => Some((
                (self.start_row + index) % self.row_count,
                RowMatchSelection::First,
            )),
            (SearchDirection::Backward, _, _) => Some((
                (self.start_row + self.row_count - index % self.row_count) % self.row_count,
                RowMatchSelection::Last,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RowMatchSelection {
    First,
    Last,
    FirstAfter(SearchMatch),
    LastBefore(SearchMatch),
    Exact(SearchMatch),
}

#[cfg(test)]
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

fn collect_matches_in_row(
    terminal: &Terminal,
    searchable_row: usize,
    viewport_row: usize,
    query: &str,
    current_match: Option<SearchMatch>,
    scratch: VisibleMatchScratch<'_>,
) {
    let Some(cells) = terminal.searchable_row(searchable_row) else {
        return;
    };
    extract_row(cells, scratch.row_text, scratch.cell_spans);

    let mut search_from = 0;
    let mut previous = None;
    while search_from < scratch.row_text.len() {
        let Some(relative_start) = scratch.row_text[search_from..].find(query) else {
            break;
        };
        let byte_start = search_from + relative_start;
        let byte_end = byte_start + query.len();
        let Some(found) = match_to_cells(searchable_row, byte_start, byte_end, scratch.cell_spans)
        else {
            break;
        };
        if previous != Some(found) {
            scratch.output.push(VisibleSearchMatch {
                row: viewport_row,
                start_cell: found.start_cell,
                end_cell: found.end_cell,
                active: current_match == Some(found),
            });
            previous = Some(found);
        }
        let character_len = scratch.row_text[byte_start..]
            .chars()
            .next()
            .expect("a non-empty match starts on a character boundary")
            .len_utf8();
        search_from = byte_start + character_len;
    }
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
        MAX_SEARCH_QUERY_BYTES, MAX_SEARCH_ROWS_PER_SLICE, SearchDirection, SearchInputOutcome,
        SearchMatch, SearchProgress, SearchState,
    };
    use crate::terminal::{Cell, Cursor, GridSize, MouseTracking, Terminal, TerminalParser};

    fn search_state(query: &str) -> SearchState {
        SearchState {
            active: true,
            query: query.to_owned(),
            caret: query.len(),
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

    fn incremental_find(
        search: &mut SearchState,
        terminal: &Terminal,
        direction: SearchDirection,
        budget: std::time::Duration,
    ) -> (Option<SearchMatch>, usize) {
        search.begin_search(terminal, direction);
        let mut slices = 0;
        loop {
            slices += 1;
            match search.continue_search(terminal, budget) {
                SearchProgress::Pending => {}
                SearchProgress::Complete(found) => return (found, slices),
                SearchProgress::Idle => panic!("a started search became idle"),
            }
        }
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

    #[test]
    fn visible_presentation_contains_active_and_secondary_matches_only() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 12);
        parser.process(&mut terminal, b"hit hit\r\nhit");
        let cells_before = terminal.render_snapshot().cells.to_vec();

        let mut search = search_state("hit");
        let active = search
            .find_next(&terminal, SearchDirection::Forward)
            .expect("the visible grid contains matches");
        search.refresh_visible_matches(&terminal);
        let presentation = search.presentation();

        assert!(presentation.active);
        assert_eq!(presentation.matches.len(), 3);
        assert_eq!(
            presentation
                .matches
                .iter()
                .filter(|search_match| search_match.active)
                .count(),
            1
        );
        assert_eq!(
            presentation
                .matches
                .iter()
                .find(|search_match| search_match.active)
                .map(|search_match| (
                    search_match.row,
                    search_match.start_cell,
                    search_match.end_cell,
                )),
            Some((active.row, active.start_cell, active.end_cell))
        );
        assert_eq!(terminal.render_snapshot().cells, cells_before);
    }

    #[test]
    fn navigating_damages_only_previous_and_new_active_match_rows() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(3, 8);
        parser.process(&mut terminal, b"hit\r\nhit\r\nhit");
        let mut search = search_state("hit");
        search.find_next(&terminal, SearchDirection::Forward);
        search.refresh_visible_matches(&terminal);
        let before = search.presentation().row_versions.to_vec();

        search.find_next(&terminal, SearchDirection::Forward);
        search.refresh_visible_matches(&terminal);
        let after = search.presentation().row_versions;

        assert_ne!(after[0], before[0]);
        assert_ne!(after[1], before[1]);
        assert_eq!(after[2], before[2]);
    }

    #[test]
    fn viewport_changes_rederive_bounded_visible_highlights() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"hit old\r\nother\r\nhit new");
        let mut search = search_state("hit");
        search.find_next(&terminal, SearchDirection::Forward);
        search.refresh_visible_matches(&terminal);
        assert_eq!(search.presentation().matches.len(), 1);
        assert_eq!(search.presentation().matches[0].row, 1);

        terminal.scroll_page_up();
        search.refresh_visible_matches(&terminal);
        let presentation = search.presentation();
        assert_eq!(presentation.matches.len(), 1);
        assert_eq!(presentation.matches[0].row, 0);
        assert!(presentation.matches.len() <= terminal.render_snapshot().rows);
    }

    #[test]
    fn closing_search_clears_every_visible_overlay_and_damages_its_rows() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 8);
        parser.process(&mut terminal, b"hit\r\nhit");
        let mut search = search_state("hit");
        search.find_next(&terminal, SearchDirection::Forward);
        search.refresh_visible_matches(&terminal);
        let before = search.presentation().row_versions.to_vec();
        assert_eq!(search.presentation().matches.len(), 2);

        assert!(search.close());
        search.refresh_visible_matches(&terminal);
        let presentation = search.presentation();
        assert!(!presentation.active);
        assert!(presentation.matches.is_empty());
        assert_ne!(presentation.row_versions[0], before[0]);
        assert_ne!(presentation.row_versions[1], before[1]);
    }

    #[test]
    fn clipboard_text_is_local_unicode_safe_and_filters_control_characters() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 16);
        parser.process(&mut terminal, "café日本語".as_bytes());
        let cursor = terminal.render_snapshot().cursor;
        let mut search = SearchState::default();

        assert!(!search.insert_text("ignored"));
        search.open();
        assert!(search.insert_text("café\n日\t本\r語"));
        assert_eq!(search.query(), "café日本語");
        assert_eq!(
            search.find_next(&terminal, SearchDirection::Forward),
            Some(SearchMatch {
                row: 0,
                start_cell: 0,
                end_cell: 10,
            })
        );
        assert_eq!(terminal.render_snapshot().cursor, cursor);
    }

    #[test]
    fn clipboard_insertion_preserves_the_query_byte_limit() {
        let mut search = SearchState::default();
        search.open();
        assert!(search.insert_text(&"é".repeat(MAX_SEARCH_QUERY_BYTES)));
        assert_eq!(search.query().len(), MAX_SEARCH_QUERY_BYTES);
        assert!(search.query().is_char_boundary(search.query().len()));
        assert!(!search.insert_text("more"));
    }

    #[test]
    fn text_inserts_at_the_search_caret() {
        let mut search = SearchState::default();
        search.open();
        assert!(search.insert_text("helo"));
        assert_eq!(
            search.handle_key(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::empty(),
            ),
            SearchInputOutcome::CaretMoved
        );
        assert_eq!(
            search.handle_key(
                &Key::Character("l".into()),
                Some("l"),
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query(), "hello");
        assert_eq!(search.caret, 4);
    }

    #[test]
    fn arrows_home_and_end_move_the_caret_without_changing_the_query() {
        let mut search = SearchState::default();
        search.open();
        search.insert_text("hello 世界");
        let query = search.query().to_owned();

        search.handle_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(search.caret, query.len() - '界'.len_utf8());
        search.handle_key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(search.caret, query.len());
        search.handle_key(&Key::Named(NamedKey::Home), None, ModifiersState::empty());
        assert_eq!(search.caret, 0);
        search.handle_key(&Key::Named(NamedKey::End), None, ModifiersState::empty());
        assert_eq!(search.caret, query.len());
        assert_eq!(search.query(), query);
    }

    #[test]
    fn backspace_and_delete_edit_around_the_caret() {
        let mut search = SearchState::default();
        search.open();
        search.insert_text("ab界cd");
        search.handle_key(&Key::Named(NamedKey::Home), None, ModifiersState::empty());
        search.handle_key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Delete), None, ModifiersState::empty(),),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query(), "a界cd");
        search.handle_key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            search.handle_key(
                &Key::Named(NamedKey::Backspace),
                None,
                ModifiersState::empty(),
            ),
            SearchInputOutcome::QueryChanged
        );
        assert_eq!(search.query(), "acd");
        assert!(search.query().is_char_boundary(search.caret));
    }

    #[test]
    fn unicode_caret_operations_always_remain_on_utf8_boundaries() {
        let mut search = SearchState::default();
        search.open();
        search.insert_text("hello 世界 café 😀 terminal");

        for _ in 0..search.query().chars().count() + 3 {
            search.handle_key(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::empty(),
            );
            assert!(search.query().is_char_boundary(search.caret));
            assert!(search.caret <= search.query().len());
        }
        for _ in 0..search.query().chars().count() + 3 {
            search.handle_key(
                &Key::Named(NamedKey::ArrowRight),
                None,
                ModifiersState::empty(),
            );
            assert!(search.query().is_char_boundary(search.caret));
            assert!(search.caret <= search.query().len());
        }
        assert_eq!(search.caret, search.query().len());
    }

    #[test]
    fn unicode_deletion_never_splits_multibyte_characters() {
        let mut search = SearchState::default();
        search.open();
        search.insert_text("é界😀");
        search.handle_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::empty(),
        );
        search.handle_key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(search.query(), "é😀");
        assert!(search.query().is_char_boundary(search.caret));
        search.handle_key(&Key::Named(NamedKey::Delete), None, ModifiersState::empty());
        assert_eq!(search.query(), "é");
        assert_eq!(search.caret, 'é'.len_utf8());
    }

    #[test]
    fn search_field_editing_never_changes_the_terminal_cursor() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 16);
        parser.process(&mut terminal, b"prompt");
        let terminal_cursor = terminal.render_snapshot().cursor;
        let mut search = SearchState::default();
        search.open();
        search.insert_text("hello 世界");
        for key in [
            NamedKey::ArrowLeft,
            NamedKey::Home,
            NamedKey::Delete,
            NamedKey::End,
            NamedKey::Backspace,
        ] {
            search.handle_key(&Key::Named(key), None, ModifiersState::empty());
        }
        assert_eq!(terminal.render_snapshot().cursor, terminal_cursor);
    }

    #[test]
    fn enter_navigation_preserves_query_and_search_caret() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 12);
        parser.process(&mut terminal, b"hit one\r\nhit two");
        let mut search = SearchState::default();
        search.open();
        search.insert_text("hit");
        search.handle_key(&Key::Named(NamedKey::Home), None, ModifiersState::empty());
        let caret = search.caret;
        let query = search.query().to_owned();

        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),),
            SearchInputOutcome::Navigate(SearchDirection::Forward)
        );
        search.find_next(&terminal, SearchDirection::Forward);
        assert_eq!(
            search.handle_key(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT,),
            SearchInputOutcome::Navigate(SearchDirection::Backward)
        );
        search.find_next(&terminal, SearchDirection::Backward);
        assert_eq!(search.query(), query);
        assert_eq!(search.caret, caret);
    }

    #[test]
    fn incremental_search_matches_synchronous_direction_and_wrapping_semantics() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 12);
        parser.process(&mut terminal, b"hit one\r\nother\r\nhit two hit");
        let mut synchronous = search_state("hit");
        let mut incremental = search_state("hit");

        for direction in [
            SearchDirection::Forward,
            SearchDirection::Forward,
            SearchDirection::Forward,
            SearchDirection::Backward,
            SearchDirection::Backward,
        ] {
            let expected = synchronous.find_next(&terminal, direction);
            let (actual, _) = incremental_find(
                &mut incremental,
                &terminal,
                direction,
                std::time::Duration::ZERO,
            );
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn incremental_search_yields_at_the_bounded_row_limit() {
        let mut terminal = Terminal::new(2, 1);
        terminal.set_scrollback_limit(MAX_SEARCH_ROWS_PER_SLICE + 32);
        terminal.set_cursor(1, 0);
        for _ in 0..MAX_SEARCH_ROWS_PER_SLICE + 32 {
            terminal.line_feed();
        }
        let mut search = search_state("absent");
        search.begin_search(&terminal, SearchDirection::Forward);

        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::MAX),
            SearchProgress::Pending
        );
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::MAX),
            SearchProgress::Complete(None)
        );
    }

    #[test]
    fn incremental_search_yields_to_a_zero_time_budget_and_resumes() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(3, 8);
        parser.process(&mut terminal, b"one\r\ntwo\r\nneedle");
        let mut search = search_state("needle");
        search.begin_search(&terminal, SearchDirection::Forward);

        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::ZERO),
            SearchProgress::Pending
        );
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::ZERO),
            SearchProgress::Pending
        );
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::ZERO),
            SearchProgress::Complete(Some(SearchMatch {
                row: 2,
                start_cell: 0,
                end_cell: 6,
            }))
        );
    }

    #[test]
    fn query_edit_and_close_cancel_incremental_search_work() {
        let mut terminal = Terminal::new(2, 1);
        terminal.set_scrollback_limit(MAX_SEARCH_ROWS_PER_SLICE + 32);
        terminal.set_cursor(1, 0);
        for _ in 0..MAX_SEARCH_ROWS_PER_SLICE + 32 {
            terminal.line_feed();
        }
        let mut search = search_state("absent");
        search.begin_search(&terminal, SearchDirection::Forward);
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::ZERO),
            SearchProgress::Pending
        );

        assert!(search.insert_text("-replacement"));
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::MAX),
            SearchProgress::Idle
        );

        search.begin_search(&terminal, SearchDirection::Forward);
        assert!(search.close());
        assert_eq!(
            search.continue_search(&terminal, std::time::Duration::MAX),
            SearchProgress::Idle
        );
    }
}
