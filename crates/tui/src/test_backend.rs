use std::fmt::{self};
use std::io::{
    Write, {self},
};
use std::num::NonZeroU16;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::prelude::CrosstermBackend;

/// This wraps a CrosstermBackend and a vt100::Parser to mock
/// a "real" terminal.
///
/// Importantly, this wrapper avoids calling any crossterm methods
/// which write to stdout regardless of the writer. This includes:
/// - getting the terminal size
/// - getting the cursor position
pub(crate) struct VT100Backend {
    crossterm_backend: CrosstermBackend<vt100::Parser>,
}

impl fmt::Debug for VT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VT100Backend").finish_non_exhaustive()
    }
}

impl VT100Backend {
    /// Creates a new `TestBackend` with the specified width and height.
    ///
    /// # Panics
    /// Panics if either dimension is zero, which vt100 cannot represent.
    #[must_use]
    pub(crate) fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            crossterm_backend: CrosstermBackend::new(vt100::Parser::new(
                NonZeroU16::new(height).expect("terminal height must be nonzero"),
                NonZeroU16::new(width).expect("terminal width must be nonzero"),
                0,
            )),
        }
    }

    #[must_use]
    pub(crate) fn vt100(&self) -> &vt100::Parser {
        self.crossterm_backend.writer()
    }
}

impl Write for VT100Backend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crossterm_backend.writer_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }
}

impl fmt::Display for VT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.crossterm_backend.writer().screen().contents())
    }
}

impl Backend for VT100Backend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.crossterm_backend.draw(content)?;
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.hide_cursor()?;
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.show_cursor()?;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.vt100().screen().cursor_position().into())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.crossterm_backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm_backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm_backend.clear_region(clear_type)
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        self.crossterm_backend.append_lines(line_count)
    }

    fn size(&self) -> io::Result<Size> {
        let (rows, cols) = self.vt100().screen().size();
        Ok(Size::new(cols.get(), rows.get()))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size()?,
            // Arbitrary size, we don't rely on this in testing.
            pixels: Size {
                width: 640,
                height: 480,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, scroll_by: u16) -> io::Result<()> {
        self.crossterm_backend.scroll_region_up(region, scroll_by)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        scroll_by: u16,
    ) -> io::Result<()> {
        self.crossterm_backend.scroll_region_down(region, scroll_by)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn terminal_dimensions_preserve_column_and_row_order() {
        let mut backend = VT100Backend::new(80, 24);
        let expected = Size::new(80, 24);
        assert_eq!(Backend::size(&backend).expect("terminal size"), expected);
        assert_eq!(
            backend.window_size().expect("window size").columns_rows,
            expected
        );
        let (rows, cols) = backend.vt100().screen().size();
        assert_eq!((cols.get(), rows.get()), (80, 24));
    }

    #[test]
    fn smallest_terminal_accepts_text() {
        let mut backend = VT100Backend::new(1, 1);
        backend.write_all(b"x").expect("terminal write");
        assert_eq!(backend.vt100().screen().contents(), "x");
    }

    #[test]
    #[should_panic(expected = "terminal width must be nonzero")]
    fn zero_width_is_rejected() {
        let _ = VT100Backend::new(0, 1);
    }

    #[test]
    #[should_panic(expected = "terminal height must be nonzero")]
    fn zero_height_is_rejected() {
        let _ = VT100Backend::new(1, 0);
    }
}
