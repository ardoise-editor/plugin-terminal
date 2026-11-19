//! What a terminal shows, as the screen node the editor draws.

use ardoise_plugin::ui::{MouseMode, Run, Screen, TermColor};

fn color(c: vt100::Color) -> Option<TermColor> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(n) => Some(TermColor::Idx(n)),
        vt100::Color::Rgb(r, g, b) => Some(TermColor::Rgb(r, g, b)),
    }
}

fn same(run: &Run, cell: &vt100::Cell) -> bool {
    run.fg == color(cell.fgcolor())
        && run.bg == color(cell.bgcolor())
        && run.bold == cell.bold()
        && run.italic == cell.italic()
        && run.underline == cell.underline()
        && run.dim == cell.dim()
        && run.inverse == cell.inverse()
}

/// The visible lines of `screen` as runs of cells that look the same.
/// Blanks with no background end a run (nothing is drawn for them).
pub fn lines(screen: &vt100::Screen) -> Vec<Vec<Run>> {
    let (rows, cols) = (screen.size().0.get(), screen.size().1.get());
    (0..rows)
        .map(|row| {
            let mut runs: Vec<Run> = Vec::new();
            let mut open = false;
            let mut col = 0;
            while col < cols {
                let Some(cell) = screen.cell(row, col) else {
                    col += 1;
                    continue;
                };
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                let glyph = cell.contents();
                let blank = glyph.is_empty() || glyph == " ";
                let plain_blank =
                    blank && cell.bgcolor() == vt100::Color::Default && !cell.inverse();
                if plain_blank {
                    // Kept inside a run (the columns follow), dropped at its end.
                    if open {
                        if let Some(run) = runs.last_mut() {
                            run.text.push(' ');
                        }
                    }
                    col += 1;
                    continue;
                }
                let wide = cell.is_wide();
                let text = if blank {
                    " ".to_string()
                } else {
                    glyph.to_string()
                };
                let joins = open && !wide && runs.last().is_some_and(|r| !r.wide && same(r, cell));
                if joins {
                    if let Some(run) = runs.last_mut() {
                        run.text.push_str(&text);
                    }
                } else {
                    if let Some(run) = runs.last_mut() {
                        let kept = run.text.trim_end().len();
                        if !run.wide && run.bg.is_none() && !run.inverse {
                            run.text.truncate(kept);
                        }
                    }
                    runs.push(Run {
                        col,
                        text,
                        fg: color(cell.fgcolor()),
                        bg: color(cell.bgcolor()),
                        bold: cell.bold(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                        dim: cell.dim(),
                        inverse: cell.inverse(),
                        wide,
                    });
                }
                open = !wide;
                col += if wide { 2 } else { 1 };
            }
            if let Some(run) = runs.last_mut() {
                if !run.wide && run.bg.is_none() && !run.inverse {
                    let kept = run.text.trim_end().len();
                    run.text.truncate(kept);
                }
            }
            runs.retain(|r| !r.text.is_empty());
            runs
        })
        .collect()
}

/// The whole screen node.
pub fn screen(id: &str, screen: &vt100::Screen, dir: &std::path::Path, highlight: &str) -> Screen {
    let cursor =
        (!screen.hide_cursor() && screen.scrollback() == 0).then(|| screen.cursor_position());
    let mouse = match screen.mouse_protocol_mode() {
        vt100::MouseProtocolMode::None => MouseMode::None,
        vt100::MouseProtocolMode::Press => MouseMode::Press,
        vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
        vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
        vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
    };
    Screen {
        id: id.to_string(),
        size: (screen.size().0.get(), screen.size().1.get()),
        lines: lines(screen),
        cursor,
        app_cursor: screen.application_cursor(),
        bracketed_paste: screen.bracketed_paste(),
        mouse,
        mouse_sgr: screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr,
        alternate: screen.alternate_screen(),
        dir: (!dir.as_os_str().is_empty()).then(|| dir.to_path_buf()),
        highlight: highlight.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    fn parsed(bytes: &[u8]) -> vt100::Parser {
        let n = |v| NonZeroU16::new(v).unwrap();
        let mut p = vt100::Parser::new(n(3), n(20), 0);
        p.process(bytes);
        p
    }

    #[test]
    fn cells_that_look_the_same_make_one_run_and_blanks_keep_the_columns() {
        let p = parsed(b"ab  cd\r\n\x1b[31mred\x1b[0m ok");
        let lines = lines(p.screen());
        assert_eq!(lines[0].len(), 1);
        assert_eq!((lines[0][0].col, lines[0][0].text.as_str()), (0, "ab  cd"));
        assert_eq!(lines[1][0].fg, Some(TermColor::Idx(1)));
        assert_eq!((lines[1][1].col, lines[1][1].text.as_str()), (4, "ok"));
        assert!(lines[2].is_empty());
    }

    #[test]
    fn a_wide_glyph_is_a_run_of_its_own() {
        let p = parsed("a日b".as_bytes());
        let runs = &lines(p.screen())[0];
        let cols: Vec<(u16, &str, bool)> = runs
            .iter()
            .map(|r| (r.col, r.text.as_str(), r.wide))
            .collect();
        assert_eq!(
            cols,
            vec![(0, "a", false), (1, "日", true), (3, "b", false)]
        );
    }

    #[test]
    fn the_modes_of_the_program_come_with_the_screen() {
        let p = parsed(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
        let s = screen("t", p.screen(), std::path::Path::new("/tmp"), "x");
        assert!(s.app_cursor && s.bracketed_paste && s.mouse_sgr);
        assert_eq!(s.mouse, MouseMode::PressRelease);
        assert_eq!(s.size, (3, 20));
        assert_eq!(s.cursor, Some((0, 0)));
    }
}
