//! VT checkpoint serialization.
//!
//! The daemon feeds every PTY chunk through a `vt100::Parser`, so at any moment
//! the parser's active screen (main or alternate) is a valid terminal state at a
//! complete parse boundary. [`render_checkpoint`] turns that state into a byte
//! stream that, applied to a freshly-reset client terminal, reconstructs the
//! same visible screen: switch to the active buffer, clear, redraw each row with
//! its SGR attributes, then restore the cursor.
//!
//! A checkpoint is a *synthetic* byte stream, not a raw replay — a truncated
//! raw ring buffer can start mid-UTF-8 / mid-CSI and depend on earlier screen
//! state, which is why §5 requires a parser-backed reconstruction instead.

use vt100::Parser;

/// Enter the alternate screen buffer (what TUIs like claude/codex run in).
/// xterm.js supports this via DECSET 1049.
const ENTER_ALT: &[u8] = b"\x1b[?1049h";
/// Leave the alternate screen, restoring the primary buffer.
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";
/// SGR reset + clear screen — the client may have arbitrary prior state, so the
/// checkpoint starts from a defined one (the VT100 grid draw already clears,
/// but we make the transition explicit before the buffer switch).
const RESET_CLEAR: &[u8] = b"\x1b[0m\x1b[2J\x1b[H";

/// Render the parser's active screen as a self-contained byte stream.
///
/// The output switches the client to the same buffer the parser is using, then
/// uses `vt100`'s own `contents_formatted` (attribute-diffed row redraw ending
/// in the cursor position), then forces an explicit cursor visibility + position
/// so the final state is exactly the parser's, independent of client quirks.
pub fn render_checkpoint(parser: &Parser) -> Vec<u8> {
    let screen = parser.screen();
    let mut out = Vec::new();
    // Switch buffers first, then clear in the right buffer, then redraw.
    out.extend_from_slice(if screen.alternate_screen() {
        ENTER_ALT
    } else {
        LEAVE_ALT
    });
    out.extend_from_slice(RESET_CLEAR);
    out.extend_from_slice(&screen.contents_formatted());
    // Redundant but deterministic: force the cursor to the parser's position and
    // visibility even if the client had different state before the checkpoint.
    let (row, col) = screen.cursor_position();
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    if screen.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25l");
    } else {
        out.extend_from_slice(b"\x1b[?25h");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `bytes` into a fresh parser and return it.
    fn parse_at(bytes: &[u8], rows: u16, cols: u16) -> Parser {
        let mut p = Parser::new(rows, cols, 1000);
        p.process(bytes);
        p
    }

    /// Round-trip: render `source`'s screen, feed the render to a FRESH parser
    /// at the same size, and assert the two visible screens match (text + cursor
    /// + the style of every non-empty cell). This is the core "checkpoint
    /// reconstructs the screen" invariant (§11).
    fn assert_reconstructs(source: &Parser, rows: u16, cols: u16) {
        let rendered = render_checkpoint(source);
        let rebuilt = parse_at(&rendered, rows, cols);
        assert_eq!(
            source.screen().contents(),
            rebuilt.screen().contents(),
            "visible text differs after checkpoint round-trip; rendered={:?}",
            String::from_utf8_lossy(&rendered)
        );
        assert_eq!(
            source.screen().cursor_position(),
            rebuilt.screen().cursor_position(),
            "cursor position differs after checkpoint round-trip"
        );

        // Cell-for-cell style comparison (skip empty cells).
        for r in 0..rows {
            for c in 0..cols {
                let Some(cell) = source.screen().cell(r, c) else {
                    continue;
                };
                if cell.contents().is_empty() {
                    continue;
                }
                let rebuilt_cell = rebuilt.screen().cell(r, c).expect("rebuilt cell");
                assert_eq!(
                    cell.contents(),
                    rebuilt_cell.contents(),
                    "cell {r},{c} contents differ"
                );
                assert_eq!(
                    cell.fgcolor(),
                    rebuilt_cell.fgcolor(),
                    "cell {r},{c} fgcolor differs"
                );
                assert_eq!(
                    cell.bgcolor(),
                    rebuilt_cell.bgcolor(),
                    "cell {r},{c} bgcolor differs"
                );
                assert_eq!(
                    cell.bold(),
                    rebuilt_cell.bold(),
                    "cell {r},{c} bold differs"
                );
                assert_eq!(
                    cell.inverse(),
                    rebuilt_cell.inverse(),
                    "cell {r},{c} inverse differs"
                );
            }
        }
    }

    #[test]
    fn empty_screen_roundtrips() {
        let src = parse_at(b"", 24, 80);
        assert_reconstructs(&src, 24, 80);
    }

    #[test]
    fn main_screen_text_and_cursor_roundtrip() {
        let src = parse_at(b"hello\nworld\x1b[3;10H", 24, 80);
        assert_reconstructs(&src, 24, 80);
        // The reconstructed screen really shows both lines.
        assert!(src.screen().contents().contains("hello"));
        assert!(src.screen().contents().contains("world"));
    }

    #[test]
    fn styles_roundtrip() {
        // Red bold on row 1, default on row 2.
        let src = parse_at(b"\x1b[31;1mred\x1b[0m\nplain", 24, 80);
        assert_reconstructs(&src, 24, 80);
    }

    #[test]
    fn wide_and_unicode_roundtrip() {
        // A CJK wide char plus a combining-mark sequence split across chunks.
        let src = parse_at("你好\u{301}abc".as_bytes(), 24, 80);
        assert_reconstructs(&src, 24, 80);
    }

    #[test]
    fn alt_screen_roundtrip() {
        // Enter alt, draw a TUI-ish frame, move the cursor.
        let src = parse_at(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[32mTOP\x1b[0m\x1b[2;5H\x1b[7minv\x1b[0m", 24, 80);
        assert!(src.screen().alternate_screen(), "test input must be in alt");
        assert_reconstructs(&src, 24, 80);
        // The rebuilt screen must also be in alt.
        let rebuilt = parse_at(&render_checkpoint(&src), 24, 80);
        assert!(
            rebuilt.screen().alternate_screen(),
            "checkpoint must preserve the alt screen"
        );
    }

    #[test]
    fn main_after_alt_exits_roundtrip() {
        // Enter then exit alt → back on the main screen.
        let src = parse_at(b"main-line\x1b[?1049halter\x1b[?1049l", 24, 80);
        assert!(!src.screen().alternate_screen());
        assert_reconstructs(&src, 24, 80);
    }

    #[test]
    fn hidden_cursor_is_preserved() {
        let src = parse_at(b"\x1b[?25lTUI", 24, 80);
        assert!(src.screen().hide_cursor());
        let rebuilt = parse_at(&render_checkpoint(&src), 24, 80);
        assert!(rebuilt.screen().hide_cursor());
    }

    #[test]
    fn utf8_split_across_chunks_is_fully_recovered() {
        // A checkpoint generated mid-UTF-8 (the parser holds the incomplete tail)
        // must reconstruct everything that was *completed* so far — §5: the
        // checkpoint is only required at complete parse boundaries.
        let full = "你".as_bytes(); // 3 bytes: E4 BD A0
        let mut src = Parser::new(24, 80, 1000);
        src.process(&full[..2]); // incomplete char held in the parser buffer
        let rebuilt = parse_at(&render_checkpoint(&src), 24, 80);
        assert_eq!(rebuilt.screen().contents(), "");
        // After the remaining byte, the char appears.
        src.process(&full[2..]);
        let rebuilt = parse_at(&render_checkpoint(&src), 24, 80);
        assert_eq!(rebuilt.screen().contents(), "你");
    }

    #[test]
    fn resize_after_checkpoint_does_not_corrupt() {
        // Render at one size, feed into a different-sized parser: the checkpoint
        // only promises reconstruction at the SAME size (attach applies
        // initial_size before snapshot per §5), so this must not panic and must
        // at least render *something* sensible.
        let src = parse_at(b"line1\nline2\nline3", 24, 80);
        let rendered = render_checkpoint(&src);
        let mut small = Parser::new(5, 10, 1000);
        small.process(&rendered); // no panic
        assert!(!small.screen().contents().is_empty());
    }
}
