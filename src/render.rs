//! Video output: GBA framebuffer (240x160 XBGR8) -> terminal escape codes.
//!
//! Two backends implement [`Renderer`]:
//! - [`HalfBlockRenderer`]: `▀` cells with 24-bit (or 256-color) fg/bg. Works in
//!   almost any terminal. Only cells that changed since the last frame are
//!   rewritten.
//! - [`KittyRenderer`]: Kitty graphics protocol, full-resolution pixels. Works in
//!   kitty, Ghostty, and WezTerm.
//!
//! Integration notes (for main.rs):
//! - `make_renderer(detect_mode())` returns a `Box<dyn Renderer>` writing to stdout.
//! - Renderers query the terminal size on every draw and repaint on resize. If a
//!   query fails mid-session they keep using the last known size.
//! - Raw mode and the alternate screen belong to `input::RawModeGuard`. Renderers
//!   only hide the cursor on their first draw and show it again when dropped.
//! - Renderers assume they own the whole screen: on the first draw and on resize
//!   they clear it with `ESC[2J` and center the image. A status bar would need
//!   the layout functions to reserve rows first.
//! - Every Kitty command sends `q=2`, so the terminal never writes replies to
//!   stdin, where they would reach input.rs.
//! - Override detection with `TERMGBA_RENDER=kitty|halfblock`,
//!   `TERMGBA_KITTY_STRATEGY=edit|retransmit`, and `TERMGBA_COLOR=truecolor|256`.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use base64::Engine as _;
use flate2::write::ZlibEncoder;
use flate2::Compression;

pub const GBA_WIDTH: usize = 240;
pub const GBA_HEIGHT: usize = 160;
pub const FRAME_BYTES: usize = GBA_WIDTH * GBA_HEIGHT * 4;

// The core loop hands renderers `emu::Frame::pixels`, so the sizes must agree.
const _: () = assert!(FRAME_BYTES == crate::emu::Frame::PIXEL_BYTES);

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
const KITTY_CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    HalfBlock,
    Kitty,
}

pub trait Renderer {
    /// Draw one frame. `pixels` must be exactly 240*160*4 bytes of XBGR8.
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()>;
}

/// Pixel layout contract with emu.rs. mGBA's 32-bit `color_t` is XBGR8
/// (0xXXBBGGRR), which on little-endian hosts sits in memory as R, G, B, X.
/// This is the only function that knows the byte order.
#[inline]
fn rgb_at(pixels: &[u8], idx: usize) -> [u8; 3] {
    let o = idx * 4;
    [pixels[o], pixels[o + 1], pixels[o + 2]]
}

/// Check the invariant every renderer receives from the core loop: exactly one
/// 240x160 XBGR8 frame.
pub fn validate_framebuffer(pixels: &[u8]) -> io::Result<()> {
    if pixels.len() != FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid framebuffer size: expected {FRAME_BYTES}, got {}",
                pixels.len()
            ),
        ));
    }
    Ok(())
}

pub fn make_renderer(mode: RenderMode) -> Box<dyn Renderer> {
    match mode {
        RenderMode::HalfBlock => Box::new(HalfBlockRenderer::new()),
        RenderMode::Kitty => Box::new(KittyRenderer::new()),
    }
}

// ---------------------------------------------------------------------------
// Terminal detection
// ---------------------------------------------------------------------------

/// Picks a renderer from the environment. It never writes to or reads from the
/// tty, so it cannot race input.rs for stdin.
pub fn detect_mode() -> RenderMode {
    detect_mode_with(|k| std::env::var(k).ok())
}

fn is_set(env: &impl Fn(&str) -> Option<String>, key: &str) -> bool {
    env(key).is_some_and(|v| !v.is_empty())
}

fn is_kitty_itself(env: &impl Fn(&str) -> Option<String>) -> bool {
    env("TERM").unwrap_or_default().contains("kitty") || is_set(env, "KITTY_WINDOW_ID")
}

fn detect_mode_with(env: impl Fn(&str) -> Option<String>) -> RenderMode {
    match env("TERMGBA_RENDER")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("kitty") => return RenderMode::Kitty,
        Some("halfblock" | "half" | "ansi") => return RenderMode::HalfBlock,
        _ => {}
    }
    // Multiplexers need passthrough wrapping for graphics, which v1 doesn't do.
    if is_set(&env, "TMUX") || is_set(&env, "STY") {
        return RenderMode::HalfBlock;
    }
    let term = env("TERM").unwrap_or_default();
    let prog = env("TERM_PROGRAM").unwrap_or_default();
    if is_kitty_itself(&env)
        || term == "xterm-ghostty"
        || prog.eq_ignore_ascii_case("ghostty")
        || prog.eq_ignore_ascii_case("wezterm")
    {
        RenderMode::Kitty
    } else {
        RenderMode::HalfBlock
    }
}

pub fn detect_kitty_strategy() -> KittyStrategy {
    detect_kitty_strategy_with(|k| std::env::var(k).ok())
}

fn detect_kitty_strategy_with(env: impl Fn(&str) -> Option<String>) -> KittyStrategy {
    match env("TERMGBA_KITTY_STRATEGY")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("edit" | "frameedit") => return KittyStrategy::FrameEdit,
        Some("retransmit") => return KittyStrategy::Retransmit,
        _ => {}
    }
    // Only kitty is known to implement animation frame editing (a=f).
    if is_kitty_itself(&env) {
        KittyStrategy::FrameEdit
    } else {
        KittyStrategy::Retransmit
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
}

pub fn detect_color_depth() -> ColorDepth {
    detect_color_depth_with(|k| std::env::var(k).ok())
}

fn detect_color_depth_with(env: impl Fn(&str) -> Option<String>) -> ColorDepth {
    match env("TERMGBA_COLOR")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("truecolor" | "24bit") => return ColorDepth::TrueColor,
        Some("256" | "ansi256") => return ColorDepth::Ansi256,
        _ => {}
    }
    let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    // Apple Terminal is the common holdout without 24-bit color. Most other
    // terminals support it even when COLORTERM is missing (e.g. over ssh).
    if env("TERM_PROGRAM").as_deref() == Some("Apple_Terminal") {
        ColorDepth::Ansi256
    } else {
        ColorDepth::TrueColor
    }
}

// ---------------------------------------------------------------------------
// Terminal geometry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
    /// Window size in pixels, or 0 if the terminal doesn't report it.
    pub px_width: u16,
    pub px_height: u16,
}

impl TermSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            px_width: 0,
            px_height: 0,
        }
    }

    pub fn query() -> io::Result<Self> {
        match crossterm::terminal::window_size() {
            Ok(ws) if ws.columns > 0 && ws.rows > 0 => Ok(Self {
                cols: ws.columns,
                rows: ws.rows,
                px_width: ws.width,
                px_height: ws.height,
            }),
            _ => {
                let (cols, rows) = crossterm::terminal::size()?;
                Ok(Self::new(cols, rows))
            }
        }
    }

    /// Cell size in pixels. Assumes 8x16 (1:2) cells when the terminal doesn't
    /// report pixel dimensions.
    fn cell_px(&self) -> (f64, f64) {
        if self.px_width > 0 && self.px_height > 0 && self.cols > 0 && self.rows > 0 {
            (
                f64::from(self.px_width) / f64::from(self.cols),
                f64::from(self.px_height) / f64::from(self.rows),
            )
        } else {
            (8.0, 16.0)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SizeSource {
    /// Query the controlling terminal on every draw.
    Auto,
    /// Use a fixed size, for tests, benchmarks, or a caller-managed viewport.
    Fixed(TermSize),
}

impl SizeSource {
    fn get(&self) -> io::Result<TermSize> {
        match *self {
            SizeSource::Auto => TermSize::query(),
            SizeSource::Fixed(s) => Ok(s),
        }
    }
}

/// A rectangle of terminal cells, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellRect {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
}

const GBA_ASPECT: f64 = GBA_WIDTH as f64 / GBA_HEIGHT as f64;

/// Half-block layout: `cols` x `rows` cells showing `cols` x `2*rows` pixels.
fn halfblock_layout(term: TermSize) -> CellRect {
    let (cw, ch) = term.cell_px();
    // Width/height ratio of one half-block "pixel".
    let par = cw / (ch / 2.0);
    let max_w = f64::from(term.cols);
    let max_h = f64::from(term.rows) * 2.0;
    let w = max_w.min((max_h * GBA_ASPECT / par).floor()).floor();
    let h = (w * par / GBA_ASPECT).round().min(max_h) as u16;
    let cols = w as u16;
    let rows = h / 2;
    if cols == 0 || rows == 0 {
        return CellRect {
            x: 0,
            y: 0,
            cols: 0,
            rows: 0,
        };
    }
    CellRect {
        x: (term.cols - cols) / 2,
        y: (term.rows - rows) / 2,
        cols,
        rows,
    }
}

/// Kitty layout: the largest 3:2 box of cells that fits the terminal.
fn kitty_layout(term: TermSize) -> CellRect {
    let (cw, ch) = term.cell_px();
    let avail_w = f64::from(term.cols) * cw;
    let avail_h = f64::from(term.rows) * ch;
    let scale = (avail_w / GBA_WIDTH as f64).min(avail_h / GBA_HEIGHT as f64);
    let cols = ((GBA_WIDTH as f64 * scale) / cw)
        .round()
        .min(f64::from(term.cols)) as u16;
    let rows = ((GBA_HEIGHT as f64 * scale) / ch)
        .round()
        .min(f64::from(term.rows)) as u16;
    if cols == 0 || rows == 0 {
        return CellRect {
            x: 0,
            y: 0,
            cols: 0,
            rows: 0,
        };
    }
    CellRect {
        x: (term.cols - cols) / 2,
        y: (term.rows - rows) / 2,
        cols,
        rows,
    }
}

/// The terminal size for this draw. A failed query mid-session (e.g. a
/// transient ioctl error) falls back to the last size that worked instead of
/// ending the session.
fn resolve_size(queried: io::Result<TermSize>, last: Option<TermSize>) -> io::Result<TermSize> {
    match (queried, last) {
        (Ok(size), _) => Ok(size),
        (Err(_), Some(size)) => Ok(size),
        (Err(e), None) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Small encoding helpers
// ---------------------------------------------------------------------------

fn push_num(buf: &mut Vec<u8>, mut n: u32) {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    buf.extend_from_slice(&tmp[i..]);
}

/// CUP to a 0-based cell.
fn push_cup(buf: &mut Vec<u8>, x: usize, y: usize) {
    buf.extend_from_slice(b"\x1b[");
    push_num(buf, y as u32 + 1);
    buf.push(b';');
    push_num(buf, x as u32 + 1);
    buf.push(b'H');
}

/// For each destination index, the source range `[start, end)` it covers.
/// Downscaling averages whole ranges. Upscaling repeats the nearest pixel.
fn spans(src: usize, dst: usize) -> Vec<(usize, usize)> {
    (0..dst)
        .map(|i| {
            let s = i * src / dst;
            let e = ((i + 1) * src / dst).max(s + 1).min(src);
            (s, e)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Half-block renderer
// ---------------------------------------------------------------------------

/// Cell colors are packed as 0x00RRGGBB for truecolor, or 0x01000000 | index
/// for 256-color.
const ANSI_FLAG: u32 = 0x0100_0000;
const NO_COLOR: u32 = u32::MAX;

fn ansi256(c: [u8; 3]) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn level(v: u8) -> usize {
        match v {
            0..48 => 0,
            48..115 => 1,
            _ => ((usize::from(v) - 35) / 40).min(5),
        }
    }
    fn dist(a: [u8; 3], b: [u8; 3]) -> u32 {
        a.iter()
            .zip(b)
            .map(|(&x, y)| (i32::from(x) - i32::from(y)).pow(2) as u32)
            .sum()
    }
    let (ri, gi, bi) = (level(c[0]), level(c[1]), level(c[2]));
    let cube = [LEVELS[ri], LEVELS[gi], LEVELS[bi]];
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;

    let avg = (u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])) / 3;
    let k = if avg < 8 { 0 } else { ((avg - 3) / 10).min(23) };
    let g = (8 + 10 * k) as u8;

    if dist(c, [g, g, g]) < dist(c, cube) {
        232 + k as u8
    } else {
        cube_idx as u8
    }
}

fn color_key(depth: ColorDepth, c: [u8; 3]) -> u32 {
    match depth {
        ColorDepth::TrueColor => u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]),
        ColorDepth::Ansi256 => ANSI_FLAG | u32::from(ansi256(c)),
    }
}

fn push_sgr_color(buf: &mut Vec<u8>, key: u32, foreground: bool) {
    buf.extend_from_slice(if foreground { b"\x1b[38;" } else { b"\x1b[48;" });
    if key & ANSI_FLAG != 0 {
        buf.extend_from_slice(b"5;");
        push_num(buf, key & 0xFF);
    } else {
        buf.extend_from_slice(b"2;");
        push_num(buf, key >> 16);
        buf.push(b';');
        push_num(buf, (key >> 8) & 0xFF);
        buf.push(b';');
        push_num(buf, key & 0xFF);
    }
    buf.push(b'm');
}

pub struct HalfBlockRenderer<W: Write = Stdout> {
    out: W,
    size: SizeSource,
    depth: ColorDepth,
    /// Terminal size and layout from the last draw.
    layout: Option<(TermSize, CellRect)>,
    xs: Vec<(usize, usize)>,
    ys: Vec<(usize, usize)>,
    /// Scaled image, `cols` x `2*rows`.
    scaled: Vec<[u8; 3]>,
    /// (top, bottom) color keys currently on screen, per cell.
    on_screen: Vec<(u32, u32)>,
    buf: Vec<u8>,
    /// Whether we hid the cursor and must show it again on drop.
    cursor_hidden: bool,
}

impl HalfBlockRenderer<Stdout> {
    pub fn new() -> Self {
        Self::with_writer(io::stdout(), SizeSource::Auto, detect_color_depth())
    }
}

impl Default for HalfBlockRenderer<Stdout> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: Write> HalfBlockRenderer<W> {
    pub fn with_writer(out: W, size: SizeSource, depth: ColorDepth) -> Self {
        Self {
            out,
            size,
            depth,
            layout: None,
            xs: Vec::new(),
            ys: Vec::new(),
            scaled: Vec::new(),
            on_screen: Vec::new(),
            buf: Vec::new(),
            cursor_hidden: false,
        }
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn set_size_source(&mut self, size: SizeSource) {
        self.size = size;
    }

    /// Forget what is on screen so the next draw clears and repaints
    /// everything. Call this after anything else writes to the terminal.
    pub fn invalidate(&mut self) {
        self.layout = None;
    }

    fn scale(&mut self, pixels: &[u8]) {
        self.scaled.clear();
        for &(y0, y1) in &self.ys {
            for &(x0, x1) in &self.xs {
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    let row = sy * GBA_WIDTH;
                    for sx in x0..x1 {
                        let p = rgb_at(pixels, row + sx);
                        r += u32::from(p[0]);
                        g += u32::from(p[1]);
                        b += u32::from(p[2]);
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                self.scaled.push([
                    ((r + n / 2) / n) as u8,
                    ((g + n / 2) / n) as u8,
                    ((b + n / 2) / n) as u8,
                ]);
            }
        }
    }
}

impl<W: Write> Renderer for HalfBlockRenderer<W> {
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()> {
        validate_framebuffer(pixels)?;
        let term = resolve_size(self.size.get(), self.layout.map(|(t, _)| t))?;
        self.buf.clear();

        if self.layout.is_none_or(|(t, _)| t != term) {
            let rect = halfblock_layout(term);
            self.xs = spans(GBA_WIDTH, usize::from(rect.cols));
            self.ys = spans(GBA_HEIGHT, usize::from(rect.rows) * 2);
            self.on_screen.clear();
            self.on_screen.resize(
                usize::from(rect.cols) * usize::from(rect.rows),
                (NO_COLOR, NO_COLOR),
            );
            self.layout = Some((term, rect));
            self.buf.extend_from_slice(b"\x1b[0m\x1b[?25l\x1b[2J");
            self.cursor_hidden = true;
        }
        let (_, rect) = self.layout.expect("layout set above");
        let (w, rows) = (usize::from(rect.cols), usize::from(rect.rows));

        if w > 0 && rows > 0 {
            self.scale(pixels);
            let body_start = self.buf.len();
            self.buf.extend_from_slice(SYNC_BEGIN);

            // Cursor position after the last glyph (row, col), and the SGR
            // colors currently in effect.
            let mut cursor = None;
            let (mut fg, mut bg) = (NO_COLOR, NO_COLOR);
            for row in 0..rows {
                for col in 0..w {
                    let top = color_key(self.depth, self.scaled[2 * row * w + col]);
                    let bot = color_key(self.depth, self.scaled[(2 * row + 1) * w + col]);
                    let cell = &mut self.on_screen[row * w + col];
                    if *cell == (top, bot) {
                        continue;
                    }
                    *cell = (top, bot);

                    if cursor != Some((row, col)) {
                        push_cup(
                            &mut self.buf,
                            usize::from(rect.x) + col,
                            usize::from(rect.y) + row,
                        );
                    }
                    if top == bot {
                        if bg == top {
                            self.buf.push(b' ');
                        } else if fg == top {
                            self.buf.extend_from_slice("█".as_bytes());
                        } else {
                            push_sgr_color(&mut self.buf, top, false);
                            bg = top;
                            self.buf.push(b' ');
                        }
                    } else {
                        if fg != top {
                            push_sgr_color(&mut self.buf, top, true);
                            fg = top;
                        }
                        if bg != bot {
                            push_sgr_color(&mut self.buf, bot, false);
                            bg = bot;
                        }
                        self.buf.extend_from_slice("▀".as_bytes());
                    }
                    cursor = Some((row, col + 1));
                }
            }

            if cursor.is_none() {
                self.buf.truncate(body_start);
            } else {
                self.buf.extend_from_slice(b"\x1b[0m");
                self.buf.extend_from_slice(SYNC_END);
            }
        }

        if !self.buf.is_empty() {
            self.out.write_all(&self.buf)?;
            self.out.flush()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Kitty graphics renderer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyStrategy {
    /// Place the image once, then overwrite only the changed rectangle of its
    /// root frame (`a=f`). Sends the fewest bytes but needs kitty's animation
    /// support.
    FrameEdit,
    /// Retransmit the whole frame whenever it changes (`a=T` with the same
    /// image and placement ids). Works wherever basic graphics do.
    Retransmit,
}

fn next_image_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    // Keep ids distinct from other programs in the same terminal. Never 0.
    0x4000_0000 | ((std::process::id() & 0xFFFF) << 8) | (n & 0xFF)
}

pub struct KittyRenderer<W: Write = Stdout> {
    out: W,
    size: SizeSource,
    strategy: KittyStrategy,
    compress: bool,
    image_id: u32,
    layout: Option<(TermSize, CellRect)>,
    placed: bool,
    /// The current frame as RGB, and the frame the terminal is showing.
    rgb: Vec<u8>,
    shown: Vec<u8>,
    have_shown: bool,
    crop: Vec<u8>,
    zbuf: Vec<u8>,
    b64: String,
    buf: Vec<u8>,
    /// Whether we hid the cursor and must show it again on drop.
    cursor_hidden: bool,
}

impl KittyRenderer<Stdout> {
    pub fn new() -> Self {
        Self::with_writer(io::stdout(), SizeSource::Auto, detect_kitty_strategy())
    }
}

impl Default for KittyRenderer<Stdout> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: Write> KittyRenderer<W> {
    pub fn with_writer(out: W, size: SizeSource, strategy: KittyStrategy) -> Self {
        Self {
            out,
            size,
            strategy,
            compress: true,
            image_id: next_image_id(),
            layout: None,
            placed: false,
            rgb: Vec::with_capacity(GBA_WIDTH * GBA_HEIGHT * 3),
            shown: Vec::with_capacity(GBA_WIDTH * GBA_HEIGHT * 3),
            have_shown: false,
            crop: Vec::new(),
            zbuf: Vec::new(),
            b64: String::new(),
            buf: Vec::new(),
            cursor_hidden: false,
        }
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn set_size_source(&mut self, size: SizeSource) {
        self.size = size;
    }

    /// Turn zlib compression of payloads (`o=z`) on or off. On by default.
    pub fn set_compression(&mut self, on: bool) {
        self.compress = on;
    }

    /// Forget what is on screen so the next draw clears and retransmits.
    pub fn invalidate(&mut self) {
        self.layout = None;
    }
}

/// Bounding box `(x, y, w, h)` of pixels that differ between two RGB frames.
fn dirty_rect(prev: &[u8], cur: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let stride = GBA_WIDTH * 3;
    let (mut y0, mut y1) = (usize::MAX, 0);
    let (mut x0, mut x1) = (usize::MAX, 0);
    for (y, (a, b)) in prev
        .chunks_exact(stride)
        .zip(cur.chunks_exact(stride))
        .enumerate()
    {
        if a == b {
            continue;
        }
        let first = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0) / 3;
        let last = a.iter().zip(b).rposition(|(p, q)| p != q).unwrap_or(0) / 3;
        y0 = y0.min(y);
        y1 = y;
        x0 = x0.min(first);
        x1 = x1.max(last);
    }
    (y0 != usize::MAX).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// Encode one graphics command, compressing and splitting into chunks as needed.
fn push_kitty_cmd(
    buf: &mut Vec<u8>,
    zbuf: &mut Vec<u8>,
    b64: &mut String,
    compress: bool,
    header: &str,
    data: &[u8],
) -> io::Result<()> {
    // Compressing tiny rectangles costs more than it saves.
    let compress = compress && data.len() >= 256;
    let payload: &[u8] = if compress {
        zbuf.clear();
        let mut enc = ZlibEncoder::new(&mut *zbuf, Compression::fast());
        enc.write_all(data)?;
        enc.finish()?;
        zbuf
    } else {
        data
    };
    b64.clear();
    base64::engine::general_purpose::STANDARD.encode_string(payload, b64);

    let mut chunks = b64.as_bytes().chunks(KITTY_CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = chunks.peek().is_some();
        buf.extend_from_slice(b"\x1b_G");
        if first {
            buf.extend_from_slice(header.as_bytes());
            if compress {
                buf.extend_from_slice(b",o=z");
            }
            if more {
                buf.extend_from_slice(b",m=1");
            }
        } else {
            // Continuation chunks may carry only m (and q).
            buf.extend_from_slice(if more { b"m=1,q=2" } else { b"m=0,q=2" });
        }
        buf.push(b';');
        buf.extend_from_slice(chunk);
        buf.extend_from_slice(b"\x1b\\");
        first = false;
    }
    Ok(())
}

impl<W: Write> Renderer for KittyRenderer<W> {
    fn draw(&mut self, pixels: &[u8]) -> io::Result<()> {
        validate_framebuffer(pixels)?;
        let term = resolve_size(self.size.get(), self.layout.map(|(t, _)| t))?;
        self.buf.clear();

        if self.layout.is_none_or(|(t, _)| t != term) {
            if self.placed {
                self.buf.extend_from_slice(
                    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.image_id).as_bytes(),
                );
            }
            self.buf.extend_from_slice(b"\x1b[?25l\x1b[2J");
            self.cursor_hidden = true;
            self.layout = Some((term, kitty_layout(term)));
            self.placed = false;
            self.have_shown = false;
        }
        let (_, rect) = self.layout.expect("layout set above");

        if rect.cols > 0 && rect.rows > 0 {
            self.rgb.clear();
            for i in 0..GBA_WIDTH * GBA_HEIGHT {
                self.rgb.extend_from_slice(&rgb_at(pixels, i));
            }
            let dirty = if self.have_shown {
                dirty_rect(&self.shown, &self.rgb)
            } else {
                Some((0, 0, GBA_WIDTH, GBA_HEIGHT))
            };

            if let Some((x, y, w, h)) = dirty {
                self.buf.extend_from_slice(SYNC_BEGIN);
                if !self.placed || self.strategy == KittyStrategy::Retransmit {
                    push_cup(&mut self.buf, usize::from(rect.x), usize::from(rect.y));
                    let header = format!(
                        "a=T,i={},p=1,f=24,s={GBA_WIDTH},v={GBA_HEIGHT},c={},r={},C=1,q=2",
                        self.image_id, rect.cols, rect.rows
                    );
                    push_kitty_cmd(
                        &mut self.buf,
                        &mut self.zbuf,
                        &mut self.b64,
                        self.compress,
                        &header,
                        &self.rgb,
                    )?;
                    self.placed = true;
                } else {
                    self.crop.clear();
                    for row in y..y + h {
                        let start = (row * GBA_WIDTH + x) * 3;
                        self.crop.extend_from_slice(&self.rgb[start..start + w * 3]);
                    }
                    let header = format!(
                        "a=f,i={},r=1,x={x},y={y},s={w},v={h},f=24,X=1,q=2",
                        self.image_id
                    );
                    push_kitty_cmd(
                        &mut self.buf,
                        &mut self.zbuf,
                        &mut self.b64,
                        self.compress,
                        &header,
                        &self.crop,
                    )?;
                }
                self.buf.extend_from_slice(SYNC_END);
                std::mem::swap(&mut self.shown, &mut self.rgb);
                self.have_shown = true;
            }
        }

        if !self.buf.is_empty() {
            self.out.write_all(&self.buf)?;
            self.out.flush()?;
        }
        Ok(())
    }
}

impl<W: Write> Drop for KittyRenderer<W> {
    fn drop(&mut self) {
        if self.placed {
            let _ = write!(self.out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.image_id);
        }
        if self.cursor_hidden {
            let _ = self.out.write_all(b"\x1b[?25h");
        }
        let _ = self.out.flush();
    }
}

impl<W: Write> Drop for HalfBlockRenderer<W> {
    fn drop(&mut self) {
        if self.cursor_hidden {
            let _ = self.out.write_all(b"\x1b[0m\x1b[?25h");
            let _ = self.out.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Read;

    fn frame(f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut px = Vec::with_capacity(FRAME_BYTES);
        for y in 0..GBA_HEIGHT {
            for x in 0..GBA_WIDTH {
                let [r, g, b] = f(x, y);
                px.extend_from_slice(&[r, g, b, 0xFF]);
            }
        }
        px
    }

    fn busy(x: usize, y: usize) -> [u8; 3] {
        [(x * 7 + y) as u8, (y * 13) as u8, (x ^ y) as u8]
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn take(w: &mut Vec<u8>) -> String {
        String::from_utf8(std::mem::take(w)).expect("renderer output is UTF-8")
    }

    /// Minimal VT interpreter covering what HalfBlockRenderer emits.
    struct Screen {
        cols: usize,
        cells: Vec<Option<(u32, u32)>>,
        cx: usize,
        cy: usize,
        fg: u32,
        bg: u32,
    }

    impl Screen {
        fn new(cols: usize, rows: usize) -> Self {
            Self {
                cols,
                cells: vec![None; cols * rows],
                cx: 0,
                cy: 0,
                fg: NO_COLOR,
                bg: NO_COLOR,
            }
        }

        fn cell(&self, x: usize, y: usize) -> Option<(u32, u32)> {
            self.cells[y * self.cols + x]
        }

        /// Applies output and returns the number of glyphs drawn.
        fn feed(&mut self, s: &str) -> usize {
            let mut glyphs = 0;
            let mut it = s.chars().peekable();
            while let Some(c) = it.next() {
                if c == '\x1b' {
                    assert_eq!(
                        it.next(),
                        Some('['),
                        "unexpected escape in half-block output"
                    );
                    let mut params = String::new();
                    let fin = loop {
                        let ch = it.next().unwrap();
                        if ('@'..='~').contains(&ch) {
                            break ch;
                        }
                        params.push(ch);
                    };
                    if params.starts_with('?') {
                        continue;
                    }
                    let nums: Vec<u32> =
                        params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
                    match fin {
                        'H' => {
                            self.cy = nums[0] as usize - 1;
                            self.cx = nums[1] as usize - 1;
                        }
                        'J' => self.cells.fill(None),
                        'm' => match nums.as_slice() {
                            [0] => (self.fg, self.bg) = (NO_COLOR, NO_COLOR),
                            [38, 2, r, g, b] => self.fg = r << 16 | g << 8 | b,
                            [48, 2, r, g, b] => self.bg = r << 16 | g << 8 | b,
                            [38, 5, n] => self.fg = ANSI_FLAG | n,
                            [48, 5, n] => self.bg = ANSI_FLAG | n,
                            other => panic!("unexpected SGR {other:?}"),
                        },
                        other => panic!("unexpected CSI {other}"),
                    }
                    continue;
                }
                let val = match c {
                    '▀' => (self.fg, self.bg),
                    '█' => (self.fg, self.fg),
                    ' ' => (self.bg, self.bg),
                    other => panic!("unexpected glyph {other:?}"),
                };
                assert!(
                    val.0 != NO_COLOR && val.1 != NO_COLOR,
                    "glyph drawn with unset color"
                );
                self.cells[self.cy * self.cols + self.cx] = Some(val);
                self.cx += 1;
                glyphs += 1;
            }
            glyphs
        }
    }

    fn truecolor_renderer(term: TermSize) -> HalfBlockRenderer<Vec<u8>> {
        HalfBlockRenderer::with_writer(Vec::new(), SizeSource::Fixed(term), ColorDepth::TrueColor)
    }

    #[test]
    fn pixel_byte_order_is_rgbx() {
        let px = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(rgb_at(&px, 0), [1, 2, 3]);
        assert_eq!(rgb_at(&px, 1), [5, 6, 7]);
    }

    #[test]
    fn wrong_size_frames_are_rejected() {
        let term = SizeSource::Fixed(TermSize::new(80, 24));
        let mut hb = HalfBlockRenderer::with_writer(Vec::new(), term, ColorDepth::TrueColor);
        let mut k = KittyRenderer::with_writer(Vec::new(), term, KittyStrategy::FrameEdit);
        for len in [0, FRAME_BYTES - 1, FRAME_BYTES + 4] {
            let px = vec![0u8; len];
            assert_eq!(hb.draw(&px).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(k.draw(&px).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        assert!(validate_framebuffer(&vec![0; FRAME_BYTES]).is_ok());
    }

    #[test]
    fn size_query_failure_falls_back_to_last_known_size() {
        let last = TermSize::new(80, 24);
        let fail = || -> io::Result<TermSize> { Err(io::Error::other("tty gone")) };
        assert_eq!(resolve_size(fail(), Some(last)).unwrap(), last);
        assert!(resolve_size(fail(), None).is_err());
        assert_eq!(
            resolve_size(Ok(TermSize::new(1, 2)), Some(last)).unwrap(),
            TermSize::new(1, 2)
        );
    }

    /// A writer that stays readable after the renderer owning it is dropped.
    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn renderers_hide_cursor_and_restore_it_on_drop() {
        let term = SizeSource::Fixed(TermSize::new(80, 24));
        let px = frame(busy);
        let hb_out = Shared::default();
        let mut hb = HalfBlockRenderer::with_writer(hb_out.clone(), term, ColorDepth::TrueColor);
        hb.draw(&px).unwrap();
        drop(hb);
        let k_out = Shared::default();
        let mut k = KittyRenderer::with_writer(k_out.clone(), term, KittyStrategy::FrameEdit);
        k.draw(&px).unwrap();
        drop(k);
        for out in [hb_out, k_out] {
            let s = String::from_utf8(out.0.borrow().clone()).unwrap();
            let hide = s.find("\x1b[?25l").expect("cursor hidden on first draw");
            assert!(s.rfind("\x1b[?25h").expect("cursor restored on drop") > hide);
        }
    }

    #[test]
    fn detects_render_mode_from_env() {
        use RenderMode::*;
        assert_eq!(detect_mode_with(env(&[("TERM", "xterm-kitty")])), Kitty);
        assert_eq!(detect_mode_with(env(&[("TERM", "xterm-ghostty")])), Kitty);
        assert_eq!(detect_mode_with(env(&[("TERM_PROGRAM", "WezTerm")])), Kitty);
        assert_eq!(
            detect_mode_with(env(&[("TERM", "xterm-256color")])),
            HalfBlock
        );
        assert_eq!(
            detect_mode_with(env(&[("TERM", "xterm-kitty"), ("TMUX", "/tmp/x")])),
            HalfBlock
        );
        assert_eq!(
            detect_mode_with(env(&[("TERM", "xterm"), ("TERMGBA_RENDER", "kitty")])),
            Kitty
        );
        assert_eq!(
            detect_mode_with(env(&[
                ("TERM", "xterm-kitty"),
                ("TERMGBA_RENDER", "halfblock")
            ])),
            HalfBlock
        );
        assert_eq!(detect_mode_with(env(&[])), HalfBlock);
    }

    #[test]
    fn detects_kitty_strategy_and_color_depth() {
        assert_eq!(
            detect_kitty_strategy_with(env(&[("KITTY_WINDOW_ID", "1")])),
            KittyStrategy::FrameEdit
        );
        assert_eq!(
            detect_kitty_strategy_with(env(&[("TERM", "xterm-ghostty")])),
            KittyStrategy::Retransmit
        );
        assert_eq!(
            detect_kitty_strategy_with(env(&[
                ("TERM", "xterm-ghostty"),
                ("TERMGBA_KITTY_STRATEGY", "edit")
            ])),
            KittyStrategy::FrameEdit
        );
        assert_eq!(
            detect_color_depth_with(env(&[("TERM_PROGRAM", "Apple_Terminal")])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            detect_color_depth_with(env(&[
                ("TERM_PROGRAM", "Apple_Terminal"),
                ("COLORTERM", "TrueColor")
            ])),
            ColorDepth::TrueColor
        );
        assert_eq!(detect_color_depth_with(env(&[])), ColorDepth::TrueColor);
    }

    #[test]
    fn ansi256_quantization() {
        assert_eq!(ansi256([0, 0, 0]), 16);
        assert_eq!(ansi256([255, 255, 255]), 231);
        assert_eq!(ansi256([255, 0, 0]), 196);
        assert_eq!(ansi256([128, 128, 128]), 244);
        assert_eq!(ansi256([95, 135, 175]), 16 + 36 + 12 + 3);
    }

    #[test]
    fn layouts_preserve_aspect_and_center() {
        // No pixel info, so cells are assumed 1:2 and half-block pixels square.
        assert_eq!(
            halfblock_layout(TermSize::new(80, 24)),
            CellRect {
                x: 4,
                y: 0,
                cols: 72,
                rows: 24
            }
        );
        assert_eq!(
            halfblock_layout(TermSize::new(240, 80)),
            CellRect {
                x: 0,
                y: 0,
                cols: 240,
                rows: 80
            }
        );
        // Large terminals upscale.
        assert_eq!(
            halfblock_layout(TermSize::new(300, 200)),
            CellRect {
                x: 0,
                y: 50,
                cols: 300,
                rows: 100
            }
        );
        assert_eq!(halfblock_layout(TermSize::new(1, 1)).cols, 0);

        assert_eq!(
            kitty_layout(TermSize::new(80, 24)),
            CellRect {
                x: 4,
                y: 0,
                cols: 72,
                rows: 24
            }
        );
        let with_px = TermSize {
            cols: 100,
            rows: 50,
            px_width: 1000,
            px_height: 1000,
        };
        // 10x20 px cells in a 1000x1000 window: 1000x667 px image = 100 cols x 33 rows.
        assert_eq!(
            kitty_layout(with_px),
            CellRect {
                x: 0,
                y: 8,
                cols: 100,
                rows: 33
            }
        );
    }

    #[test]
    fn halfblock_native_size_is_pixel_exact() {
        let term = TermSize::new(240, 80);
        let mut r = truecolor_renderer(term);
        let px = frame(busy);
        r.draw(&px).unwrap();
        let mut screen = Screen::new(240, 80);
        screen.feed(&take(r.writer_mut()));
        for row in 0..80 {
            for col in 0..240 {
                let want = (
                    color_key(ColorDepth::TrueColor, busy(col, row * 2)),
                    color_key(ColorDepth::TrueColor, busy(col, row * 2 + 1)),
                );
                assert_eq!(screen.cell(col, row), Some(want), "cell ({col},{row})");
            }
        }
    }

    #[test]
    fn halfblock_redraws_only_changed_cells() {
        let term = TermSize::new(240, 80);
        let mut r = truecolor_renderer(term);
        let mut screen = Screen::new(240, 80);
        let mut px = frame(busy);
        r.draw(&px).unwrap();
        screen.feed(&take(r.writer_mut()));

        r.draw(&px).unwrap();
        assert!(
            r.writer_mut().is_empty(),
            "identical frame should emit nothing"
        );

        // Pixel (10, 21) is the bottom half of cell (10, 10).
        px[(21 * GBA_WIDTH + 10) * 4..][..3].copy_from_slice(&[1, 2, 3]);
        r.draw(&px).unwrap();
        let out = take(r.writer_mut());
        assert_eq!(screen.feed(&out), 1);
        assert_eq!(
            screen.cell(10, 10),
            Some((color_key(ColorDepth::TrueColor, busy(10, 20)), 0x010203))
        );
    }

    #[test]
    fn halfblock_downscales_into_centered_rect() {
        let mut r = truecolor_renderer(TermSize::new(80, 24));
        r.draw(&frame(|_, _| [200, 100, 50])).unwrap();
        let mut screen = Screen::new(80, 24);
        screen.feed(&take(r.writer_mut()));
        for y in 0..24 {
            for x in 0..80 {
                let want = (4..76).contains(&x).then_some((0xC86432, 0xC86432));
                assert_eq!(screen.cell(x, y), want, "cell ({x},{y})");
            }
        }
    }

    #[test]
    fn halfblock_resize_clears_and_repaints() {
        let mut r = truecolor_renderer(TermSize::new(80, 24));
        let px = frame(busy);
        r.draw(&px).unwrap();
        take(r.writer_mut());
        r.set_size_source(SizeSource::Fixed(TermSize::new(120, 40)));
        r.draw(&px).unwrap();
        let out = take(r.writer_mut());
        assert!(out.starts_with("\x1b[0m\x1b[?25l\x1b[2J"));
        let rect = halfblock_layout(TermSize::new(120, 40));
        let mut screen = Screen::new(120, 40);
        assert_eq!(
            screen.feed(&out),
            usize::from(rect.cols) * usize::from(rect.rows)
        );
    }

    #[test]
    fn halfblock_256_color_uses_palette_sgr() {
        let mut r = HalfBlockRenderer::with_writer(
            Vec::new(),
            SizeSource::Fixed(TermSize::new(80, 24)),
            ColorDepth::Ansi256,
        );
        r.draw(&frame(busy)).unwrap();
        let out = take(r.writer_mut());
        assert!(out.contains("38;5;") && !out.contains(";2;"));
        Screen::new(80, 24).feed(&out);
    }

    /// One logical graphics command, with chunks reassembled.
    #[derive(Debug)]
    struct Gfx {
        keys: HashMap<String, String>,
        data: Vec<u8>,
    }

    fn parse_kitty(out: &str) -> Vec<Gfx> {
        let mut cmds: Vec<Gfx> = Vec::new();
        let mut b64 = String::new();
        let mut open = false;
        for part in out.split("\x1b_G").skip(1) {
            let body = &part[..part.find("\x1b\\").expect("unterminated APC")];
            let (ctrl, payload) = body.split_once(';').unwrap_or((body, ""));
            assert!(payload.len() <= KITTY_CHUNK);
            let keys: HashMap<String, String> = ctrl
                .split(',')
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap();
                    (k.to_string(), v.to_string())
                })
                .collect();
            assert_eq!(
                keys.get("q").map(String::as_str),
                Some("2"),
                "every chunk must suppress replies: {ctrl}"
            );
            if open {
                assert!(
                    keys.keys().all(|k| k == "m" || k == "q"),
                    "continuation chunk has extra keys: {ctrl}"
                );
            } else {
                cmds.push(Gfx {
                    keys: keys.clone(),
                    data: Vec::new(),
                });
                b64.clear();
            }
            b64.push_str(payload);
            open = keys.get("m").map(String::as_str) == Some("1");
            if !open {
                let cmd = cmds.last_mut().unwrap();
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(&b64)
                    .unwrap();
                cmd.data = if cmd.keys.get("o").map(String::as_str) == Some("z") {
                    let mut v = Vec::new();
                    flate2::read::ZlibDecoder::new(&raw[..])
                        .read_to_end(&mut v)
                        .unwrap();
                    v
                } else {
                    raw
                };
            }
        }
        assert!(!open, "last command left unterminated");
        cmds
    }

    fn rgb_of(px: &[u8]) -> Vec<u8> {
        px.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    fn kitty(strategy: KittyStrategy) -> KittyRenderer<Vec<u8>> {
        let term = TermSize {
            cols: 80,
            rows: 24,
            px_width: 800,
            px_height: 480,
        };
        KittyRenderer::with_writer(Vec::new(), SizeSource::Fixed(term), strategy)
    }

    #[test]
    fn kitty_first_frame_transmits_and_places_full_image() {
        let mut r = kitty(KittyStrategy::FrameEdit);
        let px = frame(busy);
        r.draw(&px).unwrap();
        let out = take(r.writer_mut());
        // 10x20 cells: a 720x480 image is 72x24 cells, centered at column 4.
        assert!(out.contains("\x1b[1;5H\x1b_G"));
        let cmds = parse_kitty(&out);
        assert_eq!(cmds.len(), 1);
        let k = &cmds[0].keys;
        for (key, val) in [
            ("a", "T"),
            ("p", "1"),
            ("f", "24"),
            ("s", "240"),
            ("v", "160"),
            ("c", "72"),
            ("r", "24"),
            ("C", "1"),
            ("o", "z"),
        ] {
            assert_eq!(k.get(key).map(String::as_str), Some(val), "key {key}");
        }
        assert_eq!(k["i"], r.image_id.to_string());
        assert_eq!(cmds[0].data, rgb_of(&px));
    }

    #[test]
    fn kitty_frame_edit_sends_only_dirty_rect() {
        let mut r = kitty(KittyStrategy::FrameEdit);
        let mut px = frame(busy);
        r.draw(&px).unwrap();
        take(r.writer_mut());

        r.draw(&px).unwrap();
        assert!(
            r.writer_mut().is_empty(),
            "identical frame should emit nothing"
        );

        for y in 30..35 {
            for x in 10..20 {
                px[(y * GBA_WIDTH + x) * 4..][..3].copy_from_slice(&[9, 9, 9]);
            }
        }
        r.draw(&px).unwrap();
        let cmds = parse_kitty(&take(r.writer_mut()));
        assert_eq!(cmds.len(), 1);
        let k = &cmds[0].keys;
        for (key, val) in [
            ("a", "f"),
            ("r", "1"),
            ("x", "10"),
            ("y", "30"),
            ("s", "10"),
            ("v", "5"),
            ("f", "24"),
        ] {
            assert_eq!(k.get(key).map(String::as_str), Some(val), "key {key}");
        }
        assert_eq!(cmds[0].data, [9u8; 150]);
    }

    #[test]
    fn kitty_retransmit_sends_full_frames() {
        let mut r = kitty(KittyStrategy::Retransmit);
        r.set_compression(false);
        let mut px = frame(busy);
        r.draw(&px).unwrap();
        take(r.writer_mut());
        px[0] = 255;
        r.draw(&px).unwrap();
        let cmds = parse_kitty(&take(r.writer_mut()));
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].keys["a"], "T");
        assert!(!cmds[0].keys.contains_key("o"));
        assert_eq!(cmds[0].data, rgb_of(&px));
    }

    #[test]
    fn kitty_resize_deletes_and_replaces() {
        let mut r = kitty(KittyStrategy::FrameEdit);
        let px = frame(busy);
        r.draw(&px).unwrap();
        take(r.writer_mut());
        r.set_size_source(SizeSource::Fixed(TermSize::new(100, 30)));
        r.draw(&px).unwrap();
        let out = take(r.writer_mut());
        let cmds = parse_kitty(&out);
        assert_eq!(cmds[0].keys["a"], "d");
        assert_eq!(cmds[1].keys["a"], "T");
        assert_eq!(cmds[1].data, rgb_of(&px));
    }

    #[test]
    fn dirty_rect_bounds() {
        let a = vec![0u8; GBA_WIDTH * GBA_HEIGHT * 3];
        let mut b = a.clone();
        assert_eq!(dirty_rect(&a, &b), None);
        b[(5 * GBA_WIDTH + 239) * 3 + 2] = 1;
        b[(100 * GBA_WIDTH + 3) * 3] = 1;
        assert_eq!(dirty_rect(&a, &b), Some((3, 5, 237, 96)));
    }
}
