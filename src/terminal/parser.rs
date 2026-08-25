use vte::{Params, Perform};

use super::{Color, Terminal};

#[derive(Default)]
pub struct TerminalParser {
    parser: vte::Parser,
}

impl TerminalParser {
    pub fn process(&mut self, terminal: &mut Terminal, bytes: &[u8]) {
        let mut performer = PerformerAdapter { terminal };
        self.parser.advance(&mut performer, bytes);
    }
}

struct PerformerAdapter<'a> {
    terminal: &'a mut Terminal,
}

impl Perform for PerformerAdapter<'_> {
    fn print(&mut self, character: char) {
        self.terminal.print(character);
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

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let values = parameter_values(params);
        let first = parameter_or(&values, 0, 1);
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
            'H' | 'f' => self.terminal.set_cursor(
                parameter_or(&values, 0, 1).saturating_sub(1) as usize,
                parameter_or(&values, 1, 1).saturating_sub(1) as usize,
            ),
            'J' => self.terminal.erase_display(parameter_or(&values, 0, 0)),
            'K' => self.terminal.erase_line(parameter_or(&values, 0, 0)),
            '@' => self.terminal.insert_characters(first as usize),
            'P' => self.terminal.delete_characters(first as usize),
            'X' => self.terminal.erase_characters(first as usize),
            'L' => self.terminal.insert_lines(first as usize),
            'M' => self.terminal.delete_lines(first as usize),
            'S' => self.terminal.scroll_up(first as usize),
            'T' => self.terminal.scroll_down(first as usize),
            'm' => apply_sgr(self.terminal, &values),
            'r' if !private => {
                if values.is_empty() || values.iter().all(|value| *value == 0) {
                    self.terminal.reset_scroll_region();
                    self.terminal.set_cursor(0, 0);
                } else {
                    let top = parameter_or(&values, 0, 1).saturating_sub(1) as usize;
                    let bottom =
                        parameter_or(&values, 1, self.terminal.render_snapshot().rows as u16)
                            .saturating_sub(1) as usize;
                    self.terminal.set_scroll_region(top, bottom);
                }
            }
            's' => self.terminal.save_cursor(),
            'u' => self.terminal.restore_cursor(),
            'h' if private => set_private_modes(self.terminal, &values, true),
            'l' if private => set_private_modes(self.terminal, &values, false),
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

fn parameter_values(params: &Params) -> Vec<u16> {
    params
        .iter()
        .map(|subparams| subparams.first().copied().unwrap_or(0))
        .collect()
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
            7 => terminal.set_auto_wrap(enabled),
            25 => terminal.set_cursor_visible(enabled),
            47 | 1047 => terminal.use_alternate_screen(enabled, enabled),
            1049 => {
                if enabled {
                    terminal.save_cursor();
                }
                terminal.use_alternate_screen(enabled, enabled);
                if !enabled {
                    terminal.restore_cursor();
                }
            }
            _ => {}
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
            3 => terminal.set_italic(true),
            4 => terminal.set_underline(true),
            7 => terminal.set_inverse(true),
            22 => terminal.set_bold(false),
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
    fn parses_private_cursor_visibility_mode() {
        let mut parser = TerminalParser::default();
        let mut terminal = Terminal::new(1, 1);
        parser.process(&mut terminal, b"\x1b[?25l");
        assert!(!terminal.render_snapshot().cursor_visible);
        parser.process(&mut terminal, b"\x1b[?25h");
        assert!(terminal.render_snapshot().cursor_visible);
    }
}
