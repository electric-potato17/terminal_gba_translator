//! Standalone harness for render.rs: drives both renderers with synthetic
//! frames, so no emulator core is needed.
//!
//!   cargo run --release --bin render_demo                  # interactive, auto-detected renderer
//!   cargo run --release --bin render_demo -- --mode kitty  # force a renderer
//!   cargo run --release --bin render_demo -- --ppm f.ppm   # show a 240x160 P6 dump from emu.rs
//!   cargo run --release --bin render_demo -- --bench       # headless throughput table, no tty needed

#[allow(dead_code)]
#[path = "../render.rs"]
mod render;

use std::cell::Cell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use render::{
    ColorDepth, FRAME_BYTES, GBA_HEIGHT, GBA_WIDTH, HalfBlockRenderer, KittyRenderer, KittyStrategy, RenderMode,
    Renderer, ScreenGuard, SizeSource, TermSize,
};

const USAGE: &str = "\
render_demo: exercise termgba's renderers without an emulator

USAGE: render_demo [OPTIONS]

OPTIONS:
  --mode <auto|halfblock|kitty>      renderer (default: auto-detect)
  --strategy <auto|edit|retransmit>  Kitty update strategy (default: auto-detect)
  --color <auto|truecolor|256>       half-block color depth (default: auto-detect)
  --no-compress                      send Kitty payloads uncompressed
  --pattern <bars|checker|gradient|sprite|noise>
  --ppm <FILE>                       show a 240x160 binary PPM (P6) instead of a pattern
  --fps <N>                          target frame rate (default: 59.7275)
  --frames <N>                       exit after N frames (bench default: 300)
  --bench                            headless: render into a byte counter and print a table
  --size <COLSxROWS[@PXWxPXH]>       terminal size for --bench (default: 160x50@1600x1000)

KEYS (interactive):
  1-5 pattern   6 PPM image   m toggle renderer   s toggle Kitty strategy
  z toggle compression   q / Esc / Ctrl-C quit
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pattern {
    Bars,
    Checker,
    Gradient,
    Sprite,
    Noise,
    Image,
}

const SYNTHETIC: [Pattern; 5] = [Pattern::Bars, Pattern::Checker, Pattern::Gradient, Pattern::Sprite, Pattern::Noise];

struct Opts {
    mode: Option<RenderMode>,
    strategy: Option<KittyStrategy>,
    depth: Option<ColorDepth>,
    compress: bool,
    pattern: Pattern,
    image: Option<Vec<u8>>,
    fps: f64,
    frames: Option<u64>,
    bench: bool,
    size: TermSize,
}

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let result = if opts.bench { run_bench(&opts) } else { run_interactive(opts) };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        mode: None,
        strategy: None,
        depth: None,
        compress: true,
        pattern: Pattern::Sprite,
        image: None,
        fps: 59.7275,
        frames: None,
        bench: false,
        size: TermSize { cols: 160, rows: 50, px_width: 1600, px_height: 1000 },
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--mode" => {
                o.mode = match val()?.as_str() {
                    "auto" => None,
                    "halfblock" | "half" => Some(RenderMode::HalfBlock),
                    "kitty" => Some(RenderMode::Kitty),
                    v => return Err(format!("unknown mode {v}")),
                }
            }
            "--strategy" => {
                o.strategy = match val()?.as_str() {
                    "auto" => None,
                    "edit" => Some(KittyStrategy::FrameEdit),
                    "retransmit" => Some(KittyStrategy::Retransmit),
                    v => return Err(format!("unknown strategy {v}")),
                }
            }
            "--color" => {
                o.depth = match val()?.as_str() {
                    "auto" => None,
                    "truecolor" => Some(ColorDepth::TrueColor),
                    "256" => Some(ColorDepth::Ansi256),
                    v => return Err(format!("unknown color depth {v}")),
                }
            }
            "--no-compress" => o.compress = false,
            "--pattern" => {
                o.pattern = match val()?.as_str() {
                    "bars" => Pattern::Bars,
                    "checker" => Pattern::Checker,
                    "gradient" => Pattern::Gradient,
                    "sprite" => Pattern::Sprite,
                    "noise" => Pattern::Noise,
                    v => return Err(format!("unknown pattern {v}")),
                }
            }
            "--ppm" => {
                let path = val()?;
                o.image = Some(load_ppm(&path).map_err(|e| format!("{path}: {e}"))?);
                o.pattern = Pattern::Image;
            }
            "--fps" => o.fps = val()?.parse().map_err(|_| "bad --fps")?,
            "--frames" => o.frames = Some(val()?.parse().map_err(|_| "bad --frames")?),
            "--bench" => o.bench = true,
            "--size" => o.size = parse_size(&val()?).ok_or("bad --size, expected e.g. 160x50@1600x1000")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.fps <= 0.0 {
        return Err("--fps must be positive".into());
    }
    Ok(o)
}

fn parse_size(s: &str) -> Option<TermSize> {
    let pair = |p: &str| -> Option<(u16, u16)> {
        let (a, b) = p.split_once('x')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let (cells, px) = match s.split_once('@') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let (cols, rows) = pair(cells)?;
    let (px_width, px_height) = match px {
        Some(p) => pair(p)?,
        None => (0, 0),
    };
    Some(TermSize { cols, rows, px_width, px_height })
}

/// Loads a 240x160 binary PPM as an XBGR8 frame (R, G, B, X byte order).
fn load_ppm(path: &str) -> io::Result<Vec<u8>> {
    let data = std::fs::read(path)?;
    let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
    let mut pos = 0;
    let mut token = || -> Option<String> {
        loop {
            while data.get(pos)?.is_ascii_whitespace() {
                pos += 1;
            }
            if data[pos] == b'#' {
                while *data.get(pos)? != b'\n' {
                    pos += 1;
                }
            } else {
                break;
            }
        }
        let start = pos;
        while data.get(pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            pos += 1;
        }
        Some(String::from_utf8_lossy(&data[start..pos]).into_owned())
    };
    let header: Option<[String; 4]> = (|| Some([token()?, token()?, token()?, token()?]))();
    let [magic, w, h, maxval] = header.ok_or_else(|| bad("truncated PPM header"))?;
    if magic != "P6" {
        return Err(bad("not a binary PPM (P6)"));
    }
    if w != GBA_WIDTH.to_string() || h != GBA_HEIGHT.to_string() || maxval != "255" {
        return Err(bad(&format!("expected 240x160 maxval 255, got {w}x{h} maxval {maxval}")));
    }
    // Exactly one whitespace byte separates the header from the pixel data.
    let body = data.get(pos + 1..pos + 1 + GBA_WIDTH * GBA_HEIGHT * 3).ok_or_else(|| bad("truncated PPM data"))?;
    Ok(body.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 0xFF]).collect())
}

// ---------------------------------------------------------------------------
// Synthetic frames
// ---------------------------------------------------------------------------

fn tri(v: u64, max: usize) -> usize {
    let period = 2 * max as u64;
    let m = v % period;
    (if m > max as u64 { period - m } else { m }) as usize
}

fn bars(x: usize, y: usize) -> [u8; 3] {
    const COLORS: [[u8; 3]; 7] =
        [[192, 192, 192], [192, 192, 0], [0, 192, 192], [0, 192, 0], [192, 0, 192], [192, 0, 0], [0, 0, 192]];
    if y < GBA_HEIGHT * 2 / 3 {
        COLORS[x * COLORS.len() / GBA_WIDTH]
    } else {
        let v = (x * 255 / (GBA_WIDTH - 1)) as u8;
        [v, v, v]
    }
}

struct FrameGen {
    rng: u64,
    buf: Vec<u8>,
}

impl FrameGen {
    fn new() -> Self {
        Self { rng: 0x9E37_79B9_7F4A_7C15, buf: vec![0; FRAME_BYTES] }
    }

    fn fill(&mut self, pattern: Pattern, t: u64, image: Option<&[u8]>) -> &[u8] {
        if let Pattern::Image = pattern {
            match image {
                Some(img) => self.buf.copy_from_slice(img),
                None => self.buf.fill(0),
            }
            return &self.buf;
        }
        let (sx, sy) = (tri(t * 2, GBA_WIDTH - 16), tri(t * 3 / 2, GBA_HEIGHT - 16));
        let blue = tri(t * 4, 255) as u8;
        for y in 0..GBA_HEIGHT {
            for x in 0..GBA_WIDTH {
                let c = match pattern {
                    Pattern::Bars => bars(x, y),
                    Pattern::Checker => {
                        let s = t as usize;
                        if ((x + s) / 16 + (y + s / 2) / 16).is_multiple_of(2) { [235, 235, 220] } else { [40, 40, 90] }
                    }
                    Pattern::Gradient => {
                        [(x * 255 / (GBA_WIDTH - 1)) as u8, (y * 255 / (GBA_HEIGHT - 1)) as u8, blue]
                    }
                    Pattern::Sprite => {
                        if (sx..sx + 16).contains(&x) && (sy..sy + 16).contains(&y) {
                            [255, 255, 255]
                        } else {
                            bars(x, y)
                        }
                    }
                    Pattern::Noise => {
                        self.rng ^= self.rng << 13;
                        self.rng ^= self.rng >> 7;
                        self.rng ^= self.rng << 17;
                        let v = self.rng.to_le_bytes();
                        [v[0], v[1], v[2]]
                    }
                    Pattern::Image => unreachable!(),
                };
                let o = (y * GBA_WIDTH + x) * 4;
                self.buf[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
        &self.buf
    }
}

// ---------------------------------------------------------------------------
// Renderer construction and stats
// ---------------------------------------------------------------------------

/// Counts bytes on their way to the real writer.
struct Counting<W> {
    inner: W,
    bytes: Rc<Cell<u64>>,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes.set(self.bytes.get() + n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Clone, Copy)]
struct Config {
    mode: RenderMode,
    strategy: KittyStrategy,
    depth: ColorDepth,
    compress: bool,
}

impl Config {
    fn label(&self) -> String {
        match self.mode {
            RenderMode::HalfBlock => match self.depth {
                ColorDepth::TrueColor => "halfblock truecolor".into(),
                ColorDepth::Ansi256 => "halfblock 256".into(),
            },
            RenderMode::Kitty => format!(
                "kitty {}{}",
                match self.strategy {
                    KittyStrategy::FrameEdit => "edit",
                    KittyStrategy::Retransmit => "retransmit",
                },
                if self.compress { " zlib" } else { " raw" }
            ),
        }
    }

    fn build<'a, W: Write + 'a>(&self, out: W, size: SizeSource) -> Box<dyn Renderer + 'a> {
        match self.mode {
            RenderMode::HalfBlock => Box::new(HalfBlockRenderer::with_writer(out, size, self.depth)),
            RenderMode::Kitty => {
                let mut k = KittyRenderer::with_writer(out, size, self.strategy);
                k.set_compression(self.compress);
                Box::new(k)
            }
        }
    }
}

#[derive(Default)]
struct Stats {
    frames: u64,
    draw: Duration,
    max_draw: Duration,
    bytes: u64,
}

impl Stats {
    fn record(&mut self, d: Duration) {
        self.frames += 1;
        self.draw += d;
        self.max_draw = self.max_draw.max(d);
    }
    fn avg_ms(&self) -> f64 {
        self.draw.as_secs_f64() * 1000.0 / self.frames.max(1) as f64
    }
    fn avg_kb(&self) -> f64 {
        self.bytes as f64 / 1024.0 / self.frames.max(1) as f64
    }
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

fn run_bench(opts: &Opts) -> io::Result<()> {
    let frames = opts.frames.unwrap_or(300);
    let size = SizeSource::Fixed(opts.size);
    let configs = [
        Config { mode: RenderMode::HalfBlock, strategy: KittyStrategy::FrameEdit, depth: ColorDepth::TrueColor, compress: false },
        Config { mode: RenderMode::HalfBlock, strategy: KittyStrategy::FrameEdit, depth: ColorDepth::Ansi256, compress: false },
        Config { mode: RenderMode::Kitty, strategy: KittyStrategy::FrameEdit, depth: ColorDepth::TrueColor, compress: true },
        Config { mode: RenderMode::Kitty, strategy: KittyStrategy::Retransmit, depth: ColorDepth::TrueColor, compress: true },
        Config { mode: RenderMode::Kitty, strategy: KittyStrategy::Retransmit, depth: ColorDepth::TrueColor, compress: false },
    ];
    let mut patterns = SYNTHETIC.to_vec();
    if opts.image.is_some() {
        patterns.push(Pattern::Image);
    }

    println!(
        "{frames} frames per run, terminal {}x{} cells @ {}x{} px (encode cost only, output discarded)\n",
        opts.size.cols, opts.size.rows, opts.size.px_width, opts.size.px_height
    );
    println!("{:<24} {:<9} {:>9} {:>9} {:>10} {:>10}", "renderer", "pattern", "avg ms", "max ms", "KB/frame", "MB/s@60");
    let mut frame_gen = FrameGen::new();
    for cfg in configs {
        for &pattern in &patterns {
            let bytes = Rc::new(Cell::new(0));
            let mut stats = Stats::default();
            {
                let mut r = cfg.build(Counting { inner: io::sink(), bytes: bytes.clone() }, size);
                for t in 0..frames {
                    let px = frame_gen.fill(pattern, t, opts.image.as_deref());
                    let start = Instant::now();
                    r.draw(px)?;
                    stats.record(start.elapsed());
                }
            }
            stats.bytes = bytes.get();
            println!(
                "{:<24} {:<9} {:>9.3} {:>9.3} {:>10.1} {:>10.2}",
                cfg.label(),
                format!("{pattern:?}").to_lowercase(),
                stats.avg_ms(),
                stats.max_draw.as_secs_f64() * 1000.0,
                stats.avg_kb(),
                stats.avg_kb() * 60.0 / 1024.0,
            );
        }
    }
    Ok(())
}

struct RawMode;

impl RawMode {
    fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn run_interactive(opts: Opts) -> io::Result<()> {
    let mut cfg = Config {
        mode: opts.mode.unwrap_or_else(render::detect_mode),
        strategy: opts.strategy.unwrap_or_else(render::detect_kitty_strategy),
        depth: opts.depth.unwrap_or_else(render::detect_color_depth),
        compress: opts.compress,
    };
    let mut pattern = opts.pattern;
    let mut all_stats: Vec<(String, Stats)> = Vec::new();

    {
        let _raw = RawMode::enable()?;
        let _screen = ScreenGuard::enter()?;
        let interval = Duration::from_secs_f64(1.0 / opts.fps);
        let mut frame_gen = FrameGen::new();
        let mut t = 0u64;

        let mut quit = false;
        while !quit {
            let label = cfg.label();
            let bytes = Rc::new(Cell::new(0));
            let mut stats = Stats::default();
            let mut renderer = cfg.build(Counting { inner: io::stdout(), bytes: bytes.clone() }, SizeSource::Auto);
            let mut next = Instant::now();
            let mut rebuild = false;

            while !rebuild && !quit {
                let px = frame_gen.fill(pattern, t, opts.image.as_deref());
                let start = Instant::now();
                renderer.draw(px)?;
                stats.record(start.elapsed());
                t += 1;
                if opts.frames.is_some_and(|n| t >= n) {
                    quit = true;
                    break;
                }

                next += interval;
                let now = Instant::now();
                if next < now {
                    next = now;
                }
                while event::poll(next.saturating_duration_since(Instant::now()))? {
                    let Event::Key(key) = event::read()? else { continue };
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => quit = true,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => quit = true,
                        KeyCode::Char(c @ '1'..='5') => pattern = SYNTHETIC[c as usize - '1' as usize],
                        KeyCode::Char('6') if opts.image.is_some() => pattern = Pattern::Image,
                        KeyCode::Char('m') => {
                            cfg.mode = match cfg.mode {
                                RenderMode::HalfBlock => RenderMode::Kitty,
                                RenderMode::Kitty => RenderMode::HalfBlock,
                            };
                            rebuild = true;
                        }
                        KeyCode::Char('s') => {
                            cfg.strategy = match cfg.strategy {
                                KittyStrategy::FrameEdit => KittyStrategy::Retransmit,
                                KittyStrategy::Retransmit => KittyStrategy::FrameEdit,
                            };
                            rebuild = cfg.mode == RenderMode::Kitty;
                        }
                        KeyCode::Char('z') => {
                            cfg.compress = !cfg.compress;
                            rebuild = cfg.mode == RenderMode::Kitty;
                        }
                        _ => {}
                    }
                }
            }

            // Drop the old renderer first so a Kitty image gets deleted.
            drop(renderer);
            stats.bytes = bytes.get();
            all_stats.push((label, stats));
            let mut out = io::stdout();
            out.write_all(b"\x1b[0m\x1b[2J")?;
            out.flush()?;
        }
    }

    for (label, s) in all_stats.iter().filter(|(_, s)| s.frames > 0) {
        let fps = s.frames as f64 / s.draw.as_secs_f64().max(1e-9);
        println!(
            "{label}: {} frames, draw avg {:.2} ms (max {:.2}), {:.1} KB/frame, draw-only ceiling {:.0} fps",
            s.frames,
            s.avg_ms(),
            s.max_draw.as_secs_f64() * 1000.0,
            s.avg_kb(),
            fps
        );
    }
    Ok(())
}
