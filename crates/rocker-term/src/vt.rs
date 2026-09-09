//! A small, self-contained VT/ANSI screen model.
//!
//! It is deliberately not a full terminal: enough of ECMA-48 + the common xterm
//! private modes to run an interactive `sh`/`bash`, `ls --color`, `top`, `git`,
//! and friends. Bytes go in via [`Screen::feed`]; the UI reads [`Screen::rows`]
//! and [`Screen::cursor`] each frame. No dependencies, so `rocker-term` stays
//! hermetic (PLAN §2 keeps the terminal backend swappable).

/// A cell colour: terminal-default, one of the 256 indexed colours, or direct
/// RGB. The UI resolves `Default`/`Indexed` against the active theme palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// One character cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

impl Cell {
    fn blank(pen: &Pen) -> Self {
        Self {
            ch: ' ',
            fg: pen.fg,
            bg: pen.bg,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: pen.inverse,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Pen {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Default for Pen {
    fn default() -> Self {
        Self {
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

impl Pen {
    fn stamp(&self, ch: char) -> Cell {
        Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            bold: self.bold,
            dim: self.dim,
            italic: self.italic,
            underline: self.underline,
            inverse: self.inverse,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    /// Consume exactly one more byte (charset designators like `ESC ( B`).
    EscIgnoreOne,
}

/// The terminal screen: a grid of [`Cell`]s plus cursor and parser state.
pub struct Screen {
    cols: u16,
    rows: u16,
    grid: Vec<Cell>,
    /// Saved primary grid while the alternate screen is active.
    saved_grid: Option<Vec<Cell>>,
    cx: u16,
    cy: u16,
    saved_cursor: (u16, u16),
    scroll_top: u16,
    scroll_bot: u16,
    pen: Pen,
    cursor_visible: bool,
    wrap_next: bool,
    alt_screen: bool,
    /// Bumped on every visible change so the UI can skip idle repaints.
    dirty: u64,

    state: State,
    params: Vec<i64>,
    param_acc: Option<i64>,
    private: bool,
    osc_buf: Vec<u8>,
    utf8: Utf8Acc,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            grid: vec![Cell::default(); cols as usize * rows as usize],
            saved_grid: None,
            cx: 0,
            cy: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bot: rows - 1,
            pen: Pen::default(),
            cursor_visible: true,
            wrap_next: false,
            alt_screen: false,
            dirty: 1,
            state: State::Ground,
            params: Vec::new(),
            param_acc: None,
            private: false,
            osc_buf: Vec::new(),
            utf8: Utf8Acc::default(),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// A monotonically increasing counter; compare against a stored value to
    /// know whether the grid changed since the last frame.
    pub fn revision(&self) -> u64 {
        self.dirty
    }

    pub fn cursor(&self) -> (u16, u16) {
        (self.cx.min(self.cols - 1), self.cy.min(self.rows - 1))
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Row `y` as a slice of cells, left to right.
    pub fn row(&self, y: u16) -> &[Cell] {
        let start = y as usize * self.cols as usize;
        &self.grid[start..start + self.cols as usize]
    }

    /// Iterator over every row, top to bottom.
    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        (0..self.rows).map(move |y| self.row(y))
    }

    /// Resize the grid, preserving the top-left overlap and clamping the cursor.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.grid = remap(&self.grid, self.cols, self.rows, cols, rows);
        if let Some(saved) = &self.saved_grid {
            self.saved_grid = Some(remap(saved, self.cols, self.rows, cols, rows));
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_next = false;
        self.dirty += 1;
    }

    /// Feed raw bytes from the exec stream.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
        self.dirty += 1;
    }

    fn step(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Esc => self.esc(b),
            State::Csi => self.csi(b),
            State::Osc => self.osc(b),
            State::EscIgnoreOne => self.state = State::Ground,
        }
    }

    fn ground(&mut self, b: u8) {
        match b {
            0x1b => {
                self.state = State::Esc;
                self.utf8.reset();
            }
            b'\r' => {
                self.cx = 0;
                self.wrap_next = false;
            }
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            b'\t' => {
                let next = ((self.cx / 8) + 1) * 8;
                self.cx = next.min(self.cols - 1);
                self.wrap_next = false;
            }
            0x08 => {
                if self.cx > 0 {
                    self.cx -= 1;
                }
                self.wrap_next = false;
            }
            0x07 => {} // bell
            0x00..=0x1f => {}
            _ => {
                if let Some(ch) = self.utf8.push(b) {
                    self.put_char(ch);
                }
            }
        }
    }

    fn esc(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.params.clear();
                self.param_acc = None;
                self.private = false;
                self.state = State::Csi;
            }
            b']' => {
                self.osc_buf.clear();
                self.state = State::Osc;
            }
            b'(' | b')' | b'*' | b'+' => self.state = State::EscIgnoreOne,
            b'M' => self.reverse_index(),
            b'D' => self.line_feed(),
            b'E' => {
                self.cx = 0;
                self.line_feed();
            }
            b'7' => self.saved_cursor = (self.cx, self.cy),
            b'8' => {
                self.cx = self.saved_cursor.0.min(self.cols - 1);
                self.cy = self.saved_cursor.1.min(self.rows - 1);
                self.wrap_next = false;
            }
            b'c' => self.full_reset(),
            _ => {}
        }
    }

    fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let acc = self.param_acc.get_or_insert(0);
                *acc = (*acc * 10 + (b - b'0') as i64).min(65535);
            }
            b';' => {
                let v = self.param_acc.take().unwrap_or(0);
                self.params.push(v);
            }
            b'?' | b'<' | b'=' | b'>' => self.private = true,
            0x20..=0x2f => {} // intermediate bytes, ignored
            0x40..=0x7e => {
                if let Some(v) = self.param_acc.take() {
                    self.params.push(v);
                }
                self.dispatch_csi(b);
                self.state = State::Ground;
            }
            _ => self.state = State::Ground,
        }
    }

    fn osc(&mut self, b: u8) {
        match b {
            0x07 => self.state = State::Ground,
            0x1b => self.state = State::EscIgnoreOne, // ST: ESC \
            _ => {
                if self.osc_buf.len() < 1024 {
                    self.osc_buf.push(b);
                }
            }
        }
    }

    fn param(&self, i: usize, default: i64) -> i64 {
        match self.params.get(i).copied() {
            Some(0) | None => default,
            Some(v) => v,
        }
    }

    fn raw_param(&self, i: usize) -> i64 {
        self.params.get(i).copied().unwrap_or(0)
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        match final_byte {
            b'H' | b'f' => {
                let row = self.param(0, 1).max(1) as u16 - 1;
                let col = self.param(1, 1).max(1) as u16 - 1;
                self.cy = row.min(self.rows - 1);
                self.cx = col.min(self.cols - 1);
                self.wrap_next = false;
            }
            b'A' => self.cy = self.cy.saturating_sub(self.param(0, 1) as u16),
            b'B' => self.cy = (self.cy + self.param(0, 1) as u16).min(self.rows - 1),
            b'C' => {
                self.cx = (self.cx + self.param(0, 1) as u16).min(self.cols - 1);
                self.wrap_next = false;
            }
            b'D' => {
                self.cx = self.cx.saturating_sub(self.param(0, 1) as u16);
                self.wrap_next = false;
            }
            b'E' => {
                self.cx = 0;
                self.cy = (self.cy + self.param(0, 1) as u16).min(self.rows - 1);
            }
            b'F' => {
                self.cx = 0;
                self.cy = self.cy.saturating_sub(self.param(0, 1) as u16);
            }
            b'G' | b'`' => {
                self.cx = (self.param(0, 1).max(1) as u16 - 1).min(self.cols - 1);
                self.wrap_next = false;
            }
            b'd' => self.cy = (self.param(0, 1).max(1) as u16 - 1).min(self.rows - 1),
            b'J' => self.erase_display(self.raw_param(0)),
            b'K' => self.erase_line(self.raw_param(0)),
            b'm' => self.select_graphic_rendition(),
            b'r' => {
                let top = self.param(0, 1).max(1) as u16 - 1;
                let bot = self.param(1, self.rows as i64).max(1) as u16 - 1;
                if top < bot && bot < self.rows {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                }
                self.cx = 0;
                self.cy = self.scroll_top;
            }
            b'S' => self.scroll_up(self.param(0, 1) as u16),
            b'T' => self.scroll_down(self.param(0, 1) as u16),
            b'L' => self.insert_lines(self.param(0, 1) as u16),
            b'M' => self.delete_lines(self.param(0, 1) as u16),
            b'P' => self.delete_chars(self.param(0, 1) as u16),
            b'@' => self.insert_blanks(self.param(0, 1) as u16),
            b'X' => self.erase_chars(self.param(0, 1) as u16),
            b's' => self.saved_cursor = (self.cx, self.cy),
            b'u' => {
                self.cx = self.saved_cursor.0.min(self.cols - 1);
                self.cy = self.saved_cursor.1.min(self.rows - 1);
            }
            b'h' if self.private => self.set_private_mode(true),
            b'l' if self.private => self.set_private_mode(false),
            _ => {}
        }
    }

    fn set_private_mode(&mut self, on: bool) {
        for &p in &self.params.clone() {
            match p {
                25 => self.cursor_visible = on,
                47 | 1047 | 1049 => self.set_alt_screen(on),
                _ => {}
            }
        }
    }

    fn set_alt_screen(&mut self, on: bool) {
        if on == self.alt_screen {
            return;
        }
        self.alt_screen = on;
        if on {
            self.saved_grid = Some(std::mem::replace(
                &mut self.grid,
                vec![Cell::default(); self.cols as usize * self.rows as usize],
            ));
            self.cx = 0;
            self.cy = 0;
        } else if let Some(saved) = self.saved_grid.take() {
            if saved.len() == self.grid.len() {
                self.grid = saved;
            }
        }
        self.wrap_next = false;
    }

    fn select_graphic_rendition(&mut self) {
        if self.params.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let params = self.params.clone();
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => self.pen = Pen::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                3 => self.pen.italic = true,
                4 => self.pen.underline = true,
                7 => self.pen.inverse = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                23 => self.pen.italic = false,
                24 => self.pen.underline = false,
                27 => self.pen.inverse = false,
                30..=37 => self.pen.fg = Color::Indexed((params[i] - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((params[i] - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((params[i] - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((params[i] - 100 + 8) as u8),
                38 | 48 => {
                    let target_fg = params[i] == 38;
                    let (consumed, color) = parse_extended_color(&params[i + 1..]);
                    if let Some(c) = color {
                        if target_fg {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.wrap_next {
            self.cx = 0;
            self.line_feed();
            self.wrap_next = false;
        }
        let idx = self.cy as usize * self.cols as usize + self.cx as usize;
        if let Some(cell) = self.grid.get_mut(idx) {
            *cell = self.pen.stamp(ch);
        }
        if self.cx + 1 >= self.cols {
            self.wrap_next = true;
        } else {
            self.cx += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.cy == self.scroll_bot {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cy == self.scroll_top {
            self.scroll_down(1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    fn scroll_up(&mut self, n: u16) {
        let n = n.max(1).min(self.scroll_bot - self.scroll_top + 1);
        let w = self.cols as usize;
        let top = self.scroll_top as usize;
        let bot = self.scroll_bot as usize;
        for y in top..=bot {
            let src = y + n as usize;
            if src <= bot {
                self.grid.copy_within(src * w..src * w + w, y * w);
            } else {
                let blank = Cell::blank(&self.pen);
                for x in 0..w {
                    self.grid[y * w + x] = blank;
                }
            }
        }
    }

    fn scroll_down(&mut self, n: u16) {
        let n = n.max(1).min(self.scroll_bot - self.scroll_top + 1);
        let w = self.cols as usize;
        let top = self.scroll_top as usize;
        let bot = self.scroll_bot as usize;
        for y in (top..=bot).rev() {
            if y >= top + n as usize {
                let src = y - n as usize;
                self.grid.copy_within(src * w..src * w + w, y * w);
            } else {
                let blank = Cell::blank(&self.pen);
                for x in 0..w {
                    self.grid[y * w + x] = blank;
                }
            }
        }
    }

    fn insert_lines(&mut self, n: u16) {
        if self.cy < self.scroll_top || self.cy > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = self.cy;
        self.scroll_down(n);
        self.scroll_top = saved_top;
    }

    fn delete_lines(&mut self, n: u16) {
        if self.cy < self.scroll_top || self.cy > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = self.cy;
        self.scroll_up(n);
        self.scroll_top = saved_top;
    }

    fn delete_chars(&mut self, n: u16) {
        let w = self.cols as usize;
        let row = self.cy as usize * w;
        let start = self.cx as usize;
        let n = (n.max(1) as usize).min(w - start);
        self.grid.copy_within(row + start + n..row + w, row + start);
        let blank = Cell::blank(&self.pen);
        for x in (w - n)..w {
            self.grid[row + x] = blank;
        }
    }

    fn insert_blanks(&mut self, n: u16) {
        let w = self.cols as usize;
        let row = self.cy as usize * w;
        let start = self.cx as usize;
        let n = (n.max(1) as usize).min(w - start);
        for x in (start..w).rev() {
            if x >= start + n {
                self.grid[row + x] = self.grid[row + x - n];
            } else {
                self.grid[row + x] = Cell::blank(&self.pen);
            }
        }
    }

    fn erase_chars(&mut self, n: u16) {
        let w = self.cols as usize;
        let row = self.cy as usize * w;
        let start = self.cx as usize;
        let end = (start + n.max(1) as usize).min(w);
        let blank = Cell::blank(&self.pen);
        for x in start..end {
            self.grid[row + x] = blank;
        }
    }

    fn erase_line(&mut self, mode: i64) {
        let w = self.cols as usize;
        let row = self.cy as usize * w;
        let blank = Cell::blank(&self.pen);
        let (a, b) = match mode {
            1 => (0, self.cx as usize + 1),
            2 => (0, w),
            _ => (self.cx as usize, w),
        };
        for x in a..b.min(w) {
            self.grid[row + x] = blank;
        }
    }

    fn erase_display(&mut self, mode: i64) {
        let w = self.cols as usize;
        let h = self.rows as usize;
        let blank = Cell::blank(&self.pen);
        let cur = self.cy as usize * w + self.cx as usize;
        let range = match mode {
            1 => 0..cur + 1,
            2 | 3 => 0..w * h,
            _ => cur..w * h,
        };
        for i in range {
            self.grid[i] = blank;
        }
    }

    fn full_reset(&mut self) {
        self.pen = Pen::default();
        self.cx = 0;
        self.cy = 0;
        self.scroll_top = 0;
        self.scroll_bot = self.rows - 1;
        self.cursor_visible = true;
        self.wrap_next = false;
        self.alt_screen = false;
        self.saved_grid = None;
        for c in &mut self.grid {
            *c = Cell::default();
        }
    }
}

/// Copy the top-left overlap of `src` into a fresh `dst_cols x dst_rows` grid.
fn remap(src: &[Cell], src_cols: u16, src_rows: u16, dst_cols: u16, dst_rows: u16) -> Vec<Cell> {
    let mut dst = vec![Cell::default(); dst_cols as usize * dst_rows as usize];
    let rows = src_rows.min(dst_rows);
    let cols = src_cols.min(dst_cols);
    for y in 0..rows {
        for x in 0..cols {
            dst[y as usize * dst_cols as usize + x as usize] =
                src[y as usize * src_cols as usize + x as usize];
        }
    }
    dst
}

/// Parse the tail of an SGR 38/48 sequence. Returns `(params consumed, color)`.
fn parse_extended_color(rest: &[i64]) -> (usize, Option<Color>) {
    match rest.first().copied() {
        Some(5) => (
            2,
            rest.get(1).map(|&n| Color::Indexed(n.clamp(0, 255) as u8)),
        ),
        Some(2) => {
            let r = rest.get(1).copied().unwrap_or(0).clamp(0, 255) as u8;
            let g = rest.get(2).copied().unwrap_or(0).clamp(0, 255) as u8;
            let b = rest.get(3).copied().unwrap_or(0).clamp(0, 255) as u8;
            (4, Some(Color::Rgb(r, g, b)))
        }
        _ => (0, None),
    }
}

/// Incremental UTF-8 decoder: bytes in, `char`s out, invalid sequences replaced.
#[derive(Default)]
struct Utf8Acc {
    buf: [u8; 4],
    len: usize,
    need: usize,
}

impl Utf8Acc {
    fn reset(&mut self) {
        self.len = 0;
        self.need = 0;
    }

    fn push(&mut self, b: u8) -> Option<char> {
        if self.need == 0 {
            match b {
                0x00..=0x7f => return Some(b as char),
                0xc0..=0xdf => self.need = 1,
                0xe0..=0xef => self.need = 2,
                0xf0..=0xf7 => self.need = 3,
                _ => return Some('\u{fffd}'),
            }
            self.buf[0] = b;
            self.len = 1;
            None
        } else {
            if b & 0xc0 != 0x80 {
                // Broken continuation — resync on this byte.
                self.reset();
                return self.push(b);
            }
            self.buf[self.len] = b;
            self.len += 1;
            self.need -= 1;
            if self.need == 0 {
                let ch = std::str::from_utf8(&self.buf[..self.len])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or('\u{fffd}');
                self.reset();
                Some(ch)
            } else {
                None
            }
        }
    }
}

/// Encode a key press into the bytes a PTY expects. `ctrl`/`alt` are the
/// modifier state; `text` is the resolved character(s) for a plain key press.
pub mod keys {
    /// A named non-text key.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Key {
        Enter,
        Backspace,
        Tab,
        BackTab,
        Escape,
        Up,
        Down,
        Right,
        Left,
        Home,
        End,
        PageUp,
        PageDown,
        Delete,
        Insert,
        F(u8),
    }

    /// Bytes for a named key. Cursor keys use the DECCKM-neutral `ESC [` form,
    /// which every shell accepts.
    pub fn encode(key: Key) -> Vec<u8> {
        let s: &[u8] = match key {
            Key::Enter => b"\r",
            Key::Backspace => b"\x7f",
            Key::Tab => b"\t",
            Key::BackTab => b"\x1b[Z",
            Key::Escape => b"\x1b",
            Key::Up => b"\x1b[A",
            Key::Down => b"\x1b[B",
            Key::Right => b"\x1b[C",
            Key::Left => b"\x1b[D",
            Key::Home => b"\x1b[H",
            Key::End => b"\x1b[F",
            Key::PageUp => b"\x1b[5~",
            Key::PageDown => b"\x1b[6~",
            Key::Delete => b"\x1b[3~",
            Key::Insert => b"\x1b[2~",
            Key::F(1) => b"\x1bOP",
            Key::F(2) => b"\x1bOQ",
            Key::F(3) => b"\x1bOR",
            Key::F(4) => b"\x1bOS",
            Key::F(5) => b"\x1b[15~",
            Key::F(6) => b"\x1b[17~",
            Key::F(7) => b"\x1b[18~",
            Key::F(8) => b"\x1b[19~",
            Key::F(9) => b"\x1b[20~",
            Key::F(10) => b"\x1b[21~",
            Key::F(11) => b"\x1b[23~",
            Key::F(12) => b"\x1b[24~",
            Key::F(_) => b"",
        };
        s.to_vec()
    }

    /// `Ctrl` + an ASCII letter/symbol → the corresponding C0 control byte.
    pub fn ctrl_byte(ch: char) -> Option<u8> {
        let c = ch.to_ascii_uppercase();
        match c {
            '@'..='_' => Some((c as u8) & 0x1f),
            'a'..='z' => Some((c.to_ascii_uppercase() as u8) & 0x1f),
            ' ' => Some(0),
            '?' => Some(0x7f),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(screen: &Screen, y: u16) -> String {
        screen.row(y).iter().map(|c| c.ch).collect::<String>()
    }

    #[test]
    fn plain_text_and_wrap() {
        let mut s = Screen::new(10, 3);
        s.feed(b"hello");
        assert_eq!(line(&s, 0).trim_end(), "hello");
        s.feed(b" world!!");
        // 13 chars into a 10-wide grid wraps onto row 1.
        assert_eq!(line(&s, 0), "hello worl");
        assert_eq!(line(&s, 1).trim_end(), "d!!");
    }

    #[test]
    fn carriage_return_and_newline() {
        let mut s = Screen::new(20, 4);
        s.feed(b"abc\r\ndef");
        assert_eq!(line(&s, 0).trim_end(), "abc");
        assert_eq!(line(&s, 1).trim_end(), "def");
    }

    #[test]
    fn sgr_sets_color() {
        let mut s = Screen::new(10, 2);
        s.feed(b"\x1b[31mX\x1b[0mY");
        assert_eq!(s.row(0)[0].fg, Color::Indexed(1));
        assert_eq!(s.row(0)[1].fg, Color::Default);
    }

    #[test]
    fn cursor_position_and_erase() {
        let mut s = Screen::new(10, 3);
        s.feed(b"AAAAA\x1b[1;1Hbb");
        assert_eq!(line(&s, 0), "bbAAA     ");
        s.feed(b"\x1b[2J");
        assert_eq!(line(&s, 0).trim_end(), "");
    }

    #[test]
    fn scrolls_when_bottom_row_overflows() {
        let mut s = Screen::new(4, 2);
        s.feed(b"1\r\n2\r\n3");
        assert_eq!(line(&s, 0).trim_end(), "2");
        assert_eq!(line(&s, 1).trim_end(), "3");
    }

    #[test]
    fn resize_keeps_top_left() {
        let mut s = Screen::new(10, 3);
        s.feed(b"keepme");
        s.resize(6, 4);
        assert_eq!(line(&s, 0), "keepme");
    }

    #[test]
    fn utf8_multibyte() {
        let mut s = Screen::new(8, 2);
        s.feed("héllo".as_bytes());
        assert_eq!(s.row(0)[1].ch, 'é');
    }
}
