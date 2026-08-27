use vte::{Params, Perform};

use super::{Color, MouseTracking, Terminal};

#[derive(Default)]
pub struct TerminalParser {
    parser: vte::Parser,
}

impl TerminalParser {
    pub fn process(&mut self, terminal: &mut Terminal, bytes: &[u8]) {
        terminal.begin_output();
        let mut performer = PerformerAdapter { terminal };
        self.parser.advance(&mut performer, bytes);
        performer.terminal.finish_output();
    }
}

struct PerformerAdapter<'a> {
    terminal: &'a mut Terminal,
}

impl Perform for PerformerAdapter<'_> {
    fn print(&mut self, character: char) {
        self.terminal.print_output(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\x08' => self.terminal.backspace(),
            b'\t' => self.terminal.tab(),
            b'\n' | b'\x0b' | b'\x0c' => self.terminal.line_feed(),
            b'\r' => self.terminal.carriage_return(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if matches!(params.first(), Some(code) if *code == b"0" || *code == b"2") {
            self.terminal.set_title(&params[1..]);
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let mut value_storage = [0_u16; 32];
        let value_count = params.len().min(value_storage.len());
        for (destination, subparams) in value_storage.iter_mut().zip(params.iter()) {
            *destination = subparams.first().copied().unwrap_or(0);
        }
        let values = &value_storage[..value_count];
        let first = parameter_or(values, 0, 1);
        let private = intermediates.contains(&b'?');

        match action {
            'A' => self.terminal.move_cursor_relative(-(first as isize), 0),
            'B' | 'e' => self.terminal.move_cursor_relative(first as isize, 0),
            'C' | 'a' => self.terminal.move_cursor_relative(0, first as isize),
            'D' => self.terminal.move_cursor_relative(0, -(first as isize)),
            'E' => {
                self.terminal.move_cursor_relative(first as isize, 0);
                self.terminal.carriage_return();
            }
            'F' => {
                self.terminal.move_cursor_relative(-(first as isize), 0);
                self.terminal.carriage_return();
            }
            'G' | '`' => self.terminal.set_cursor(
                self.terminal.render_snapshot().cursor.row,
                first.saturating_sub(1) as usize,
            ),
            'I' => self.terminal.tab_forward(first as usize),
            'Z' => self.terminal.tab_backward(first as usize),
            'H' | 'f' => self.terminal.set_cursor_address(
                parameter_or(values, 0, 1).saturating_sub(1) as usize,
                parameter_or(values, 1, 1).saturating_sub(1) as usize,
            ),
            'J' => self.terminal.erase_display(parameter_or(values, 0, 0)),
            'K' => self.terminal.erase_line(parameter_or(values, 0, 0)),
            '@' => self.terminal.insert_characters(first as usize),
            'P' => self.terminal.delete_characters(first as usize),
            'X' => self.terminal.erase_characters(first as usize),
            'L' => self.terminal.insert_lines(first as usize),
            'M' => self.terminal.delete_lines(first as usize),
            'S' => self.terminal.scroll_up(first as usize),
            'T' => self.terminal.scroll_down(first as usize),
            'm' => apply_sgr(self.terminal, values),
            'r' if !private => {
                if values.is_empty() || values.iter().all(|value| *value == 0) {
                    self.terminal.reset_scroll_region();
                    self.terminal.set_cursor(0, 0);
                } else {
                    let top = parameter_or(values, 0, 1).saturating_sub(1) as usize;
                    let bottom =
                        parameter_or(values, 1, self.terminal.render_snapshot().rows as u16)
                            .saturating_sub(1) as usize;
                    self.terminal.set_scroll_region(top, bottom);
                }
            }
            'g' if !private => self.terminal.clear_tab_stop(parameter_or(values, 0, 0)),
            's' => self.terminal.save_cursor(),
            'u' => self.terminal.restore_cursor(),
            'h' if !private => set_ansi_modes(self.terminal, values, true),
            'l' if !private => set_ansi_modes(self.terminal, values, false),
            'h' if private => set_private_modes(self.terminal, values, true),
            'l' if private => set_private_modes(self.terminal, values, false),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.terminal.save_cursor(),
            b'8' => self.terminal.restore_cursor(),
            b'H' => self.terminal.set_tab_stop(),
            b'=' => self.terminal.set_application_keypad(true),
            b'>' => self.terminal.set_application_keypad(false),
            b'D' => self.terminal.line_feed(),
            b'E' => {
                self.terminal.line_feed();
                self.terminal.carriage_return();
            }
            b'M' => self.terminal.reverse_index(),
            b'c' => self.terminal.reset(),
            _ => {}
        }
    }
}

fn parameter_or(values: &[u16], index: usize, default: u16) -> u16 {
    match values.get(index).copied() {
        Some(0) | None => default,
        Some(value) => value,
    }
}

fn set_private_modes(terminal: &mut Terminal, values: &[u16], enabled: bool) {
    for value in values {
        match value {
            1 => terminal.set_application_cursor_keys(enabled),
            6 => terminal.set_origin_mode(enabled),
            7 => terminal.set_auto_wrap(enabled),
            25 => terminal.set_cursor_visible(enabled),
            1000 => terminal.set_mouse_tracking(MouseTracking::Button, enabled),
            1002 => terminal.set_mouse_tracking(MouseTracking::ButtonMotion, enabled),
            1003 => terminal.set_mouse_tracking(MouseTracking::AnyMotion, enabled),
            1006 => terminal.set_sgr_mouse(enabled),
            47 => terminal.use_alternate_screen(enabled, false),
            1047 => terminal.use_alternate_screen(enabled, enabled),
            1049 => {
                if enabled && !terminal.alternate_screen_active() {
                    terminal.save_cursor();
                    terminal.use_alternate_screen(true, true);
                } else if !enabled && terminal.alternate_screen_active() {
                    terminal.use_alternate_screen(false, false);
                    terminal.restore_cursor();
                }
            }
            2004 => terminal.set_bracketed_paste(enabled),
            _ => {}
        }
    }
}

fn set_ansi_modes(terminal: &mut Terminal, values: &[u16], enabled: bool) {
    for value in values {
        if *value == 4 {
            terminal.set_insert_mode(enabled);
        }
    }
}

fn apply_sgr(terminal: &mut Terminal, values: &[u16]) {
    let values = if values.is_empty() { &[0][..] } else { values };
    let mut index = 0;
    while index < values.len() {
        match values[index] {
            0 => terminal.reset_attributes(),
            1 => terminal.set_bold(true),
            2 => terminal.set_dim(true),
            3 => terminal.set_italic(true),
            4 => terminal.set_underline(true),
            7 => terminal.set_inverse(true),
            22 => {
                terminal.set_bold(false);
                terminal.set_dim(false);
            }
            23 => terminal.set_italic(false),
            24 => terminal.set_underline(false),
            27 => terminal.set_inverse(false),
            30..=37 => terminal.set_foreground(Color::Indexed((values[index] - 30) as u8)),
            39 => terminal.set_foreground(Color::Default),
            40..=47 => terminal.set_background(Color::Indexed((values[index] - 40) as u8)),
            49 => terminal.set_background(Color::Default),
            90..=97 => terminal.set_foreground(Color::Indexed((values[index] - 90 + 8) as u8)),
            100..=107 => terminal.set_background(Color::Indexed((values[index] - 100 + 8) as u8)),
            38 | 48 => {
                let foreground = values[index] == 38;
                if values.get(index + 1) == Some(&5) {
                    if let Some(color) = values.get(index + 2) {
                        if foreground {
                            terminal.set_foreground(Color::Indexed((*color).min(255) as u8));
                        } else {
                            terminal.set_background(Color::Indexed((*color).min(255) as u8));
                        }
                        index += 2;
                    }
                } else if values.get(index + 1) == Some(&2) && index + 4 < values.len() {
                    let color = Color::Rgb(
                        values[index + 2].min(255) as u8,
                        values[index + 3].min(255) as u8,
                        values[index + 4].min(255) as u8,
                    );
                    if foreground {
                        terminal.set_foreground(color);
                    } else {
                        terminal.set_background(color);
                    }
                    index += 4;
                }
            }
            _ => {}
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalParser;
    use crate::terminal::{Color, Terminal};

    #[test]
    fn parses_cursor_erase_and_sgr_sequences() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 5);
        parser.process(&mut terminal, b"abc\x1b[2D\x1b[31;48;2;1;2;3mZ\x1b[K");
        let snapshot = terminal.render_snapshot();
        assert_eq!(snapshot.cells[1].character, 'Z');
        assert_eq!(snapshot.cells[1].foreground, Color::Indexed(1));
        assert_eq!(snapshot.cells[1].background, Color::Rgb(1, 2, 3));
        assert_eq!(snapshot.cells[2].character, ' ');
    }

    #[test]
    fn switches_to_and_from_alternate_screen() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 3);
        parser.process(&mut terminal, b"p\x1b[?1049ha\x1b[?1049l");
        assert_eq!(terminal.render_snapshot().cells[0].character, 'p');
    }

    #[test]
    fn redundant_alternate_screen_reset_does_not_restore_an_unrelated_cursor() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 3);
        parser.process(&mut terminal, b"\x1b[2;2H\x1b[?1049l");
        assert_eq!(
            terminal.render_snapshot().cursor,
            crate::terminal::Cursor { row: 1, column: 1 }
        );
    }

    #[test]
    fn dec_save_restore_recovers_cursor_attributes_and_modes() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 3);
        parser.process(
            &mut terminal,
            b"\x1b[2;2H\x1b[31;1m\x1b7\x1b[1;1H\x1b[0m\x1b[?7l\x1b8X",
        );
        let snapshot = terminal.render_snapshot();
        assert_eq!(snapshot.cells[4].character, 'X');
        assert_eq!(snapshot.cells[4].foreground, Color::Indexed(1));
        assert!(snapshot.cells[4].flags.bold());
    }

    #[test]
    fn parses_private_cursor_visibility_mode() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b[?25l");
        assert!(!terminal.render_snapshot().cursor_visible);
        parser.process(&mut terminal, b"\x1b[?25h");
        assert!(terminal.render_snapshot().cursor_visible);
    }

    #[test]
    fn parses_bracketed_paste_mode() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b[?2004h");
        assert!(terminal.bracketed_paste());
        parser.process(&mut terminal, b"\x1b[?2004l");
        assert!(!terminal.bracketed_paste());
    }

    #[test]
    fn decodes_utf8_sequences_split_across_pty_chunks() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 6);
        let bytes = "é界".as_bytes();
        parser.process(&mut terminal, &bytes[..1]);
        parser.process(&mut terminal, &bytes[1..4]);
        parser.process(&mut terminal, &bytes[4..]);
        let snapshot = terminal.render_snapshot();
        assert_eq!(snapshot.cells[0].character, 'é');
        assert_eq!(snapshot.cells[1].character, '界');
        assert_eq!(
            snapshot.cells[2].width,
            crate::terminal::CellWidth::Continuation
        );
    }

    #[test]
    fn parses_fragmented_csi_exactly_like_a_contiguous_sequence() {
        let mut fragmented_parser = TerminalParser::default();
        let mut fragmented = Terminal::new(1, 2);
        for chunk in [
            &b"\x1b["[..],
            &b"3"[..],
            &b"8;2;"[..],
            &b"255;0;0"[..],
            &b"mX"[..],
        ] {
            fragmented_parser.process(&mut fragmented, chunk);
        }

        let mut contiguous_parser = TerminalParser::default();
        let mut contiguous = Terminal::new(1, 2);
        contiguous_parser.process(&mut contiguous, b"\x1b[38;2;255;0;0mX");
        assert_eq!(
            fragmented.render_snapshot().cells,
            contiguous.render_snapshot().cells
        );
        assert_eq!(
            fragmented.render_snapshot().cells[0].foreground,
            Color::Rgb(255, 0, 0)
        );
    }

    #[test]
    fn incomplete_malformed_and_oversized_sequences_remain_bounded() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(2, 3);
        parser.process(&mut terminal, b"A\x1b[");
        assert_eq!(terminal.render_snapshot().cells[0].character, 'A');
        parser.process(
            &mut terminal,
            b"999999999999999999999999999999999999999999;1H",
        );
        assert_eq!(
            terminal.render_snapshot().cursor,
            crate::terminal::Cursor { row: 1, column: 0 }
        );
        let cursor = terminal.render_snapshot().cursor;
        parser.process(
            &mut terminal,
            b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26;27;28;29;30;31;32;33H",
        );
        assert_eq!(terminal.render_snapshot().cursor, cursor);
        parser.process(&mut terminal, b"\x1b[?9999hB");
        assert_eq!(terminal.render_snapshot().cells[3].character, 'B');
    }

    #[test]
    fn parses_application_cursor_and_mouse_modes() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b[?1;1002;1006h");
        assert!(terminal.application_cursor_keys());
        assert_eq!(
            terminal.mouse_tracking(),
            crate::terminal::MouseTracking::ButtonMotion
        );
        assert!(terminal.sgr_mouse());
        parser.process(&mut terminal, b"\x1b[?1;1002;1006l");
        assert!(!terminal.application_cursor_keys());
        assert_eq!(
            terminal.mouse_tracking(),
            crate::terminal::MouseTracking::None
        );
        assert!(!terminal.sgr_mouse());
    }

    #[test]
    fn sgr_styles_and_extended_colors_do_not_leak_after_reset() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 3);
        parser.process(
            &mut terminal,
            b"\x1b[1;2;3;4;7;38;5;200;48;2;1;2;3mX\x1b[0mY",
        );
        let cells = terminal.render_snapshot().cells;
        assert!(cells[0].flags.bold());
        assert!(cells[0].flags.dim());
        assert!(cells[0].flags.italic());
        assert!(cells[0].flags.underline());
        assert!(cells[0].flags.inverse());
        assert_eq!(cells[0].foreground, Color::Indexed(200));
        assert_eq!(cells[0].background, Color::Rgb(1, 2, 3));
        assert_eq!(cells[1].foreground, Color::Default);
        assert_eq!(cells[1].background, Color::Default);
        assert_eq!(cells[1].flags, crate::terminal::CellFlags::default());
    }

    #[test]
    fn insert_mode_shifts_existing_cells() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 5);
        parser.process(&mut terminal, b"ABC\x1b[2G\x1b[4hX\x1b[4l");
        let text: String = terminal.render_snapshot().cells[..4]
            .iter()
            .map(|cell| cell.character)
            .collect();
        assert_eq!(text, "AXBC");
    }

    #[test]
    fn origin_mode_addresses_and_clamps_within_scroll_margins() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(5, 4);
        parser.process(&mut terminal, b"\x1b[2;4r\x1b[?6h\x1b[1;1H\x1b[20AZ");
        assert_eq!(terminal.render_snapshot().cursor.row, 1);
        assert_eq!(terminal.render_snapshot().cells[4].character, 'Z');
        parser.process(&mut terminal, b"\x1b[?6l");
        assert_eq!(
            terminal.render_snapshot().cursor,
            crate::terminal::Cursor::default()
        );
    }

    #[test]
    fn tab_stop_controls_replace_the_default_stops() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 12);
        parser.process(&mut terminal, b"\x1b[3g\x1b[4G\x1bH\r\tX");
        assert_eq!(terminal.render_snapshot().cells[3].character, 'X');

        parser.process(&mut terminal, b"\x1b[12G\x1b[2ZY");
        assert_eq!(terminal.render_snapshot().cells[0].character, 'Y');
    }

    #[test]
    fn parses_application_keypad_mode() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b=");
        assert!(terminal.application_keypad());
        parser.process(&mut terminal, b"\x1b>");
        assert!(!terminal.application_keypad());
    }

    #[test]
    fn parses_fragmented_bounded_window_titles() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b]2;Flash");
        parser.process(&mut terminal, b"; audit\x07");
        assert_eq!(terminal.title(), "Flash; audit");
        let version = terminal.title_version();
        parser.process(&mut terminal, b"\x1b]2;Flash; audit\x07");
        assert_eq!(terminal.title_version(), version);

        let oversized = format!("\x1b]2;{}\x07", "x".repeat(2048));
        parser.process(&mut terminal, oversized.as_bytes());
        assert!(terminal.title().len() <= 1024);
    }
}
