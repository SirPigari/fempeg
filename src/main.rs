use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Instant;

use crate::term_colors::{blue, dark, green, pink, red, white};
use anyhow::{Context, Result};
use clap::CommandFactory;
use clap::Parser;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use num_cpus;
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;

#[cfg(feature = "include_exiftool")]
use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
#[cfg(feature = "include_exiftool")]
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
};
#[cfg(feature = "include_exiftool")]
use std::{collections::HashSet, io::stdout};

mod init_libraw;
mod libraw_ffi;
mod term_colors;

#[cfg(feature = "include_exiftool")]
mod exiftool;

const VALID_FORMATS: &[&str] = &["png", "jpeg", "jpg", "bmp", "gif", "webp", "tiff", "tif"];
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Convert NEF images (via libraw) to common formats",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Args {
    #[arg(value_name = "INPUT", required = true, num_args = 1.., help = "One or more input files or a single input directory")]
    input: Vec<PathBuf>,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT_DIR",
        help = "Output directory or file (if single input file)"
    )]
    output_dir: Option<PathBuf>,
    #[arg(
        short = 'f',
        long = "format",
        default_value = "png",
        help = "Output image format(s), e.g. png, jpeg, bmp, gif, webp, tiff (tif). Multiple formats can be specified separated by + or , (e.g. png+jpeg or png,jpeg)"
    )]
    format: String,
    #[arg(
        short = 'r',
        long = "ratio",
        default_value_t = 0.15_f64,
        help = "Resize image to this ratio"
    )]
    ratio: f64,
    #[arg(
        short = 't',
        long = "threads",
        help = "Number of threads to use (default: number of CPU cores)"
    )]
    threads: Option<usize>,
    #[arg(
        short = 'p',
        long = "preview",
        default_value_t = false,
        help = "Use embedded preview image if available"
    )]
    preview: bool,
    #[arg(
        short = 'b',
        long = "brightness",
        num_args = 0..=1,
        help = "Brightness control. No flag = leave as-is. `-b` (no value) => auto. Accepts: `auto`/`true`, `none`/`false`, a float factor (e.g. 0.58), an integer literal (e.g. 5 => factor 5.0), or a percent suffix (e.g. 120% => 1.2, -20% => 0.8)."
    )]
    brightness: Option<Option<String>>,
    #[arg(
        short = 'R',
        long = "rotation",
        help = "Rotation handling. Use `auto` to read EXIF orientation or provide degrees (90/180/270) or EXIF orientation (1-8)."
    )]
    rotation: Option<String>,
    #[arg(
        short = 'e',
        long = "enhance",
        default_value_t = false,
        help = "Automatically enhance the image (simple unsharpen + slight contrast)"
    )]
    enhance: bool,
    #[arg(
        short = 'd',
        long = "debug",
        default_value_t = false,
        help = "Enable debug output"
    )]
    debug: bool,
    #[arg(
        short = 'i',
        long = "info",
        default_value_t = false,
        help = "Print metadata/info for the input file(s) and exit"
    )]
    info: bool,
    #[arg(short = 'v', long = "version", help = "Print version information")]
    version: bool,
    #[arg(
        long = "sort",
        value_name = "METHOD",
        help = "Sort input files before processing. Methods: name, mtime, size, numeric"
    )]
    sort: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum BrightnessMode {
    None,
    Auto,
    Factor(f32),
}

fn parse_brightness(opt: &Option<Option<String>>) -> BrightnessMode {
    match opt {
        None => BrightnessMode::None,
        Some(None) => BrightnessMode::Auto,
        Some(Some(s)) => {
            let s_trim = s.trim();
            let low = s_trim.to_ascii_lowercase();
            if low == "true" || low == "auto" {
                BrightnessMode::Auto
            } else if low == "false" || low == "none" {
                BrightnessMode::None
            } else {
                if s_trim.ends_with('%') {
                    let num = s_trim[..s_trim.len() - 1].trim();
                    if let Ok(v) = num.parse::<f32>() {
                        return BrightnessMode::Factor(v / 100.0);
                    }
                }
                if s_trim.contains('.') || s_trim.contains('e') || s_trim.contains('E') {
                    if let Ok(v) = s_trim.parse::<f32>() {
                        return BrightnessMode::Factor(v);
                    }
                }
                if let Ok(i) = s_trim.parse::<i32>() {
                    return BrightnessMode::Factor(i as f32);
                }
                if let Ok(v) = s_trim.parse::<f32>() {
                    return BrightnessMode::Factor(v);
                }
                BrightnessMode::None
            }
        }
    }
}

#[cfg(not(feature = "include_exiftool"))]
fn print_metadata(path: &Path) -> Result<()> {
    let fname = path.to_string_lossy();
    println!("{}", blue(format!("Metadata: {}", fname)));
    let meta = std::fs::metadata(path).with_context(|| format!("Failed to stat {}", fname))?;
    println!("  {}: {} bytes", white("Size"), meta.len());
    let buf = std::fs::read(path).with_context(|| format!("Failed to read {}", fname))?;

    match rexif::parse_buffer(&buf) {
        Ok(exif) => {
            if !exif.entries.is_empty() {
                println!("\n{}", blue("EXIF / Metadata entries:"));
                for entry in exif.entries.iter() {
                    let tag_name = format!("{}", entry.tag);
                    let value = format!("{}", entry.value);
                    let max: usize = 512;
                    let v = if value.len() > max {
                        format!("{}...", &value[..max])
                    } else {
                        value
                    };
                    println!("  {}: {}", pink(tag_name), white(v));
                }
            }
        }
        Err(e) => {
            eprintln!("{}", pink(format!("Failed to parse EXIF/XMP: {}", e)));
        }
    }

    if is_nef_file(path) {
        println!("\n{}", blue("Format hint: NEF (Nikon RAW) detected"));
    } else {
        println!(
            "\n{}",
            blue("Format hint: NEF not detected by header heuristics")
        );
    }

    Ok(())
}

#[cfg(feature = "include_exiftool")]
fn print_metadata(path: &Path) -> Result<()> {
    let json = exiftool::call_exiftool(path)?;
    let map = exiftool::parse_exiftool_json(&json).unwrap_or_default();

    let mut entries: Vec<(String, String, String)> = Vec::new();
    for (k, v) in map.into_iter() {
        if let Some(pos) = k.find(':') {
            let (prefix, rest) = k.split_at(pos);
            let rest = &rest[1..];
            entries.push((prefix.to_string(), rest.to_string(), v));
        } else {
            entries.push(("".to_string(), k, v));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    use std::collections::HashMap;
    let mut prefix_colors: HashMap<String, Color> = HashMap::new();
    let palette = [
        Color::Rgb(0x9d, 0xac, 0xff),
        Color::Rgb(0xff, 0xd0, 0xd7),
        Color::Rgb(0xe4, 0xe4, 0xe4),
        Color::Rgb(0x08, 0x08, 0x08),
        Color::Green,
        Color::Red,
    ];
    let mut pi = 0usize;
    for (p, _, _) in &entries {
        if !prefix_colors.contains_key(p) {
            let c = palette[pi % palette.len()];
            prefix_colors.insert(p.clone(), c);
            pi = pi.wrapping_add(1);
        }
    }

    enable_raw_mode().context("enable_raw_mode failed")?;
    let mut stdout = stdout();
    execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let mut query = String::new();
    let mut offset: usize = 0;
    let mut last_key: Option<KeyCode> = None;
    let mut last_key_time: Option<Instant> = None;

    let res = (|| -> Result<()> {
        loop {
            terminal.draw(|f| {
                let size = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints([Constraint::Min(3), Constraint::Length(3)])
                    .split(size);

                let q = query.to_ascii_lowercase();
                let default_keys: Vec<&str> = vec![
                    "IFD0:Model",
                    "IFD0:Make",
                    "ExifIFD:SerialNumber",
                    "ExifIFD:LensModel",
                    "ExifIFD:FocalLength",
                    "ExifIFD:FNumber",
                    "ExifIFD:ExposureTime",
                    "Composite:ShutterSpeed",
                    "Nikon:ISO",
                    "ExifIFD:ExposureCompensation",
                    "ExifIFD:ExposureMode",
                    "ExifIFD:MeteringMode",
                    "Nikon:FocusMode",
                    "Composite:AutoFocus",
                    "Nikon:AFAreaMode",
                    "Nikon:WhiteBalance",
                    "Nikon:PictureControlName",
                    "Nikon:Contrast",
                    "Nikon:Sharpness",
                    "System:FileCreateDate",
                    "Composite:ImageSize",
                ];
                let filtered: Vec<_> = if q.trim().is_empty() {
                    if default_keys.is_empty() {
                        entries.iter().collect()
                    } else {
                        let mut added: HashSet<usize> = HashSet::new();
                        let mut out: Vec<&(String, String, String)> = Vec::new();

                        let make_full = |p: &str, k: &str| {
                            if p.is_empty() {
                                k.to_string()
                            } else {
                                format!("{}:{}", p, k)
                            }
                        };

                        for dk in &default_keys {
                            let target = dk.to_ascii_lowercase();
                            for (i, e) in entries.iter().enumerate() {
                                if added.contains(&i) {
                                    continue;
                                }
                                let full = make_full(&e.0, &e.1).to_ascii_lowercase();
                                if full == target {
                                    out.push(e);
                                    added.insert(i);
                                }
                            }
                        }

                        let pri = ["nikon", "composite", "exifidf"];
                        for &pfx in &pri {
                            let mut bucket: Vec<(usize, &(String, String, String))> = entries
                                .iter()
                                .enumerate()
                                .filter(|(i, e)| {
                                    !added.contains(i) && e.0.to_ascii_lowercase() == pfx
                                })
                                .collect();
                            bucket.sort_by_key(|(_, e)| e.1.clone());
                            for (i, e) in bucket {
                                out.push(e);
                                added.insert(i);
                            }
                        }

                        let mut rest: Vec<(usize, &(String, String, String))> = entries
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| !added.contains(i))
                            .collect();
                        rest.sort_by_key(|(_, e)| (e.0.clone(), e.1.clone()));
                        for (i, e) in rest {
                            out.push(e);
                            added.insert(i);
                        }

                        out
                    }
                } else {
                    entries
                        .iter()
                        .filter(|(_, k, v)| {
                            k.to_ascii_lowercase().contains(&q)
                                || v.to_ascii_lowercase().contains(&q)
                        })
                        .collect()
                };

                let height = (chunks[0].height as usize).saturating_sub(2);
                let start = offset.min(filtered.len());
                let end = (start + height).min(filtered.len());

                let rows: Vec<Row> = filtered[start..end]
                    .iter()
                    .map(|(p, k, v)| {
                        let prefix = if p.is_empty() {
                            "".to_string()
                        } else {
                            p.clone()
                        };
                        let fg = *prefix_colors.get(p).unwrap_or(&Color::Gray);
                        let cell_prefix = Cell::from(Span::styled(
                            prefix,
                            Style::default().fg(fg).add_modifier(Modifier::BOLD),
                        ));
                        let cell_key = Cell::from(k.clone());
                        let cell_val = Cell::from(v.clone());
                        Row::new(vec![cell_prefix, cell_key, cell_val])
                    })
                    .collect();

                let header = Row::new(vec!["Group", "Tag", "Value"])
                    .style(Style::default().add_modifier(Modifier::BOLD));
                let widths = [
                    Constraint::Length(16),
                    Constraint::Length(30),
                    Constraint::Min(10),
                ];
                let table = Table::new(rows, widths)
                    .header(header)
                    .block(
                        Block::default().borders(Borders::ALL).title(format!(
                            "Metadata: {}",
                            path.file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.to_string_lossy().to_string())
                        )),
                    )
                    .column_spacing(1);

                f.render_widget(table, chunks[0]);

                let search = Paragraph::new(query.as_str())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Search (type, Backspace, Del to clear, Esc/Ctrl-C to exit)"),
                    )
                    .wrap(Wrap { trim: true });
                f.render_widget(search, chunks[1]);
            })?;

            if event::poll(std::time::Duration::from_millis(200))? {
                match event::read()? {
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    }) => {
                        break;
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Esc, ..
                    }) => {
                        break;
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Char(ch),
                        ..
                    }) => {
                        let now = Instant::now();
                        let mut accept = true;
                        if let Some(last) = &last_key {
                            if *last == KeyCode::Char(ch) {
                                if let Some(t) = last_key_time {
                                    if now.duration_since(t).as_millis() < 40 {
                                        accept = false;
                                    }
                                }
                            }
                        }
                        if accept {
                            query.push(ch);
                            offset = 0;
                            last_key = Some(KeyCode::Char(ch));
                            last_key_time = Some(now);
                        }
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Backspace,
                        ..
                    }) => {
                        let now = Instant::now();
                        let mut accept = true;
                        if let Some(last) = &last_key {
                            if *last == KeyCode::Backspace {
                                if let Some(t) = last_key_time {
                                    if now.duration_since(t).as_millis() < 40 {
                                        accept = false;
                                    }
                                }
                            }
                        }
                        if accept {
                            query.pop();
                            offset = 0;
                            last_key = Some(KeyCode::Backspace);
                            last_key_time = Some(now);
                        }
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Delete,
                        ..
                    }) => {
                        let now = Instant::now();
                        let mut accept = true;
                        if let Some(last) = &last_key {
                            if *last == KeyCode::Delete {
                                if let Some(t) = last_key_time {
                                    if now.duration_since(t).as_millis() < 40 {
                                        accept = false;
                                    }
                                }
                            }
                        }
                        if accept {
                            query.clear();
                            offset = 0;
                            last_key = Some(KeyCode::Delete);
                            last_key_time = Some(now);
                        }
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Up, ..
                    }) => {
                        offset = offset.saturating_sub(1);
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Down,
                        ..
                    }) => {
                        offset = offset.saturating_add(1);
                    }
                    CEvent::Key(KeyEvent {
                        code: KeyCode::Enter,
                        ..
                    }) => { /* todo */ }
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )
    .ok();
    terminal.show_cursor().ok();

    res
}

fn apply_brightness(img: DynamicImage, mode: BrightnessMode) -> DynamicImage {
    match mode {
        BrightnessMode::None => img,
        BrightnessMode::Auto => img,
        BrightnessMode::Factor(f) => {
            let mut buf = img.to_rgba8();
            for p in buf.pixels_mut() {
                p[0] = ((p[0] as f32 * f).min(255.0).max(0.0)) as u8;
                p[1] = ((p[1] as f32 * f).min(255.0).max(0.0)) as u8;
                p[2] = ((p[2] as f32 * f).min(255.0).max(0.0)) as u8;
            }
            DynamicImage::ImageRgba8(buf)
        }
    }
}

fn format_time(secs: f64) -> String {
    let s = secs as u64;
    if s < 60 {
        format!("{}s", s)
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

fn resize_image(img: DynamicImage, ratio: f64) -> DynamicImage {
    let scale = ratio.sqrt();
    let new_w = (img.width() as f64 * scale).max(1.0) as u32;
    let new_h = (img.height() as f64 * scale).max(1.0) as u32;
    img.resize_exact(new_w, new_h, FilterType::Lanczos3)
}

fn is_nef_file(path: &Path) -> bool {
    let f = std::fs::File::open(path);
    let mut f = match f {
        Ok(x) => x,
        Err(_) => return false,
    };
    let mut buf = Vec::new();
    let _ = std::io::Read::by_ref(&mut f)
        .take(131072)
        .read_to_end(&mut buf);
    if buf.len() < 4 {
        return false;
    }
    if !(buf.starts_with(b"II*\0") || buf.starts_with(b"MM\0*")) {
        return false;
    }
    let mut found_nikon = false;
    let lower: Vec<u8> = buf.iter().map(|b| b.to_ascii_lowercase()).collect();
    if lower.windows(5).any(|w| w == b"nikon") {
        found_nikon = true;
    }
    if found_nikon {
        return true;
    }
    if let Ok(exif) = rexif::parse_buffer(&buf) {
        for entry in exif.entries.iter() {
            let val = format!("{}", entry.value).to_ascii_lowercase();
            if val.contains("nikon") {
                return true;
            }
        }
    }
    false
}

fn sort_inputs(inputs: &mut Vec<PathBuf>, method: &str, debug: bool) {
    match method.to_ascii_lowercase().as_str() {
        "name" => inputs.sort_by_key(|p| p.file_name().map(|s| s.to_os_string())),
        "numeric" => inputs.sort_by(|a, b| {
            let na = a.file_stem().and_then(|s| s.to_str()).and_then(|s| {
                s.chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            });
            let nb = b.file_stem().and_then(|s| s.to_str()).and_then(|s| {
                s.chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            });
            match (na, nb) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        }),
        "size" => inputs.sort_by(|a, b| {
            let sa = a.metadata().map(|m| m.len()).unwrap_or(0);
            let sb = b.metadata().map(|m| m.len()).unwrap_or(0);
            sa.cmp(&sb)
        }),
        "mtime" | "time" | "date" => inputs.sort_by(|a, b| {
            let ta = a
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok().map(|e| e.as_secs()));
            let tb = b
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok().map(|e| e.as_secs()));
            match (ta, tb) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        }),
        other => {
            if debug {
                eprintln!("Unknown sort method '{}', leaving unsorted", other);
            }
        }
    }
}

unsafe fn load_with_libraw(
    path: &Path,
    use_preview: bool,
    debug: bool,
    auto_brightness: bool,
) -> Result<DynamicImage> {
    let api = libraw_ffi::get_api().context("Failed to load libraw symbols")?;
    if debug {
        println!("{} calling libraw_init...", blue("[init]"));
    }
    let raw = unsafe { (api.libraw_init)(0) };
    if debug {
        println!("{} libraw_init -> {:p}", blue("[init]"), raw);
    }
    if raw.is_null() {
        anyhow::bail!("libraw_init returned null");
    }

    if debug {
        println!("{} reading file into memory...", blue("[read]"));
    }
    let data = std::fs::read(path).with_context(|| format!("Failed to read {:?}", path))?;
    if debug {
        println!(
            "{} calling libraw_open_buffer (len={})...",
            blue("[buffer]"),
            data.len()
        );
    }
    let r = unsafe { (api.libraw_open_buffer)(raw, data.as_ptr(), data.len()) };
    if debug {
        println!("{} libraw_open_buffer -> {}", blue("[buffer]"), r);
    }
    if r != 0 {
        unsafe { (api.libraw_close)(raw) };
        anyhow::bail!("libraw_open_buffer failed: {}", r);
    }

    if debug {
        println!("{} calling libraw_unpack...", blue("[unpack]"));
    }
    let r = unsafe { (api.libraw_unpack)(raw) };
    if debug {
        println!("{} libraw_unpack -> {}", blue("[unpack]"), r);
    }
    if r != 0 {
        unsafe { (api.libraw_close)(raw) };
        anyhow::bail!("libraw_unpack failed: {}", r);
    }

    let _ = unsafe { (api.libraw_set_output_bps)(raw, 8) };
    let _ = unsafe { (api.libraw_set_output_color)(raw, 1) };
    let no_auto_val = if auto_brightness { 0 } else { 1 };
    let _ = unsafe { (api.libraw_set_no_auto_bright)(raw, no_auto_val) };

    if use_preview {
        let mut err_code: std::os::raw::c_int = 0;
        let pimg = unsafe {
            (api.libraw_dcraw_make_mem_image)(raw, &mut err_code as *mut std::os::raw::c_int)
        };
        if !pimg.is_null() {
            let ty = unsafe { (*pimg).type_ };
            let data_size = unsafe { (*pimg).data_size as usize };
            if data_size > 0 {
                let header_size = std::mem::size_of::<libraw_ffi::LibRawProcessedImage>();
                let data_ptr = (pimg as *const u8).wrapping_add(header_size) as *const u8;
                let slice = unsafe { std::slice::from_raw_parts(data_ptr, data_size) };
                if ty == 1 {
                    let img = image::load_from_memory(slice)
                        .context("Failed to decode preview JPEG from libraw")?;
                    unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                    unsafe { (api.libraw_close)(raw) };
                    return Ok(img);
                } else {
                    let colors = unsafe { (*pimg).colors as usize };
                    let width = unsafe { (*pimg).width as u32 };
                    let height = unsafe { (*pimg).height as u32 };
                    let bits = unsafe { (*pimg).bits };
                    if bits != 8 {
                        unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                        unsafe { (api.libraw_close)(raw) };
                        anyhow::bail!("libraw preview bitmap has unsupported bit depth: {}", bits);
                    }
                    let expected = (width as usize) * (height as usize) * colors;
                    if data_size < expected {
                        unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                        unsafe { (api.libraw_close)(raw) };
                        anyhow::bail!(
                            "libraw preview bitmap too small: {} < {}",
                            data_size,
                            expected
                        );
                    }
                    let vec = slice[..expected].to_vec();
                    let result_img = match colors {
                        3 => {
                            let imgbuf = image::RgbImage::from_raw(width, height, vec)
                                .context("Failed to construct RGB image from libraw preview")?;
                            DynamicImage::ImageRgb8(imgbuf)
                        }
                        4 => {
                            let imgbuf = image::RgbaImage::from_raw(width, height, vec)
                                .context("Failed to construct RGBA image from libraw.preview")?;
                            DynamicImage::ImageRgba8(imgbuf)
                        }
                        _ => {
                            unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                            unsafe { (api.libraw_close)(raw) };
                            anyhow::bail!("Unsupported preview colors: {}", colors);
                        }
                    };
                    unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                    unsafe { (api.libraw_close)(raw) };
                    return Ok(result_img);
                }
            }
            unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
        }
        let err_msg = unsafe {
            let p = (api.libraw_strerror)(err_code);
            if p.is_null() {
                "(unknown)".into()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        if debug {
            eprintln!(
                "libraw dcraw_make_mem_image preview returned null or empty (err={} msg={}), continuing to full processing",
                err_code, err_msg
            );
        }
    }

    if debug {
        println!("{} calling libraw_dcraw_process...", blue("[process]"));
    }
    let r = unsafe { (api.libraw_dcraw_process)(raw) };
    if debug {
        println!("{} libraw_dcraw_process -> {}", blue("[process]"), r);
    }
    if r != 0 {
        unsafe { (api.libraw_close)(raw) };
        anyhow::bail!("libraw_dcraw_process failed: {}", r);
    }

    if debug {
        println!(
            "{} calling libraw_dcraw_make_mem_image...",
            blue("[mem_image]")
        );
    }
    let mut err_code: std::os::raw::c_int = 0;
    let pimg = unsafe {
        (api.libraw_dcraw_make_mem_image)(raw, &mut err_code as *mut std::os::raw::c_int)
    };
    if debug {
        println!(
            "{} libraw_dcraw_make_mem_image -> {:p}, err={}",
            blue("[mem_image]"),
            pimg,
            err_code
        );
    }
    if pimg.is_null() {
        let err_msg = unsafe {
            let p = (api.libraw_strerror)(err_code);
            if p.is_null() {
                "(unknown)".into()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        unsafe { (api.libraw_close)(raw) };
        anyhow::bail!(
            "libraw dcraw_make_mem_image returned null: {} ({})",
            err_code,
            err_msg
        );
    }

    let ty = unsafe { (*pimg).type_ };
    let data_size = unsafe { (*pimg).data_size as usize };
    let header_size = std::mem::size_of::<libraw_ffi::LibRawProcessedImage>();
    let data_ptr = (pimg as *const u8).wrapping_add(header_size) as *const u8;
    if data_ptr.is_null() || data_size == 0 {
        unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
        unsafe { (api.libraw_close)(raw) };
        anyhow::bail!("libraw processed image has no data (size={})", data_size);
    }
    if debug {
        println!(
            "{} constructing slice for data_size={}",
            blue("[mem_image]"),
            data_size
        );
    }
    let slice = unsafe { std::slice::from_raw_parts(data_ptr, data_size) };
    if debug {
        if slice.len() > 0 {
            let b = slice[0];
            println!("{} first byte = {}", blue("[mem_image]"), b);
        } else {
            println!("{} slice has no bytes", blue("[mem_image]"));
        }
    }
    let img = if ty == 1 {
        image::load_from_memory(slice).context("Failed to decode processed JPEG from libraw")?
    } else {
        let colors = unsafe { (*pimg).colors as usize };
        let width = unsafe { (*pimg).width as u32 };
        let height = unsafe { (*pimg).height as u32 };
        let bits = unsafe { (*pimg).bits };
        if bits != 8 {
            unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
            unsafe { (api.libraw_close)(raw) };
            anyhow::bail!(
                "libraw processed bitmap has unsupported bit depth: {}",
                bits
            );
        }
        let expected = (width as usize) * (height as usize) * colors;
        if data_size < expected {
            unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
            unsafe { (api.libraw_close)(raw) };
            anyhow::bail!(
                "libraw processed bitmap too small: {} < {}",
                data_size,
                expected
            );
        }
        let vec = slice[..expected].to_vec();
        match colors {
            3 => {
                let imgbuf = image::RgbImage::from_raw(width, height, vec)
                    .context("Failed to construct RGB image from libraw processed data")?;
                DynamicImage::ImageRgb8(imgbuf)
            }
            4 => {
                let imgbuf = image::RgbaImage::from_raw(width, height, vec)
                    .context("Failed to construct RGBA image from libraw processed data")?;
                DynamicImage::ImageRgba8(imgbuf)
            }
            _ => {
                unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
                unsafe { (api.libraw_close)(raw) };
                anyhow::bail!("Unsupported processed colors: {}", colors);
            }
        }
    };
    unsafe { (api.libraw_dcraw_clear_mem)(pimg) };
    unsafe { (api.libraw_close)(raw) };
    Ok(img)
}

fn save_image(img: &DynamicImage, out_path: &Path, fmt: &str) -> Result<()> {
    let fmt = match fmt.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "tiff" => ImageFormat::Tiff,
        "bmp" => ImageFormat::Bmp,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        other => anyhow::bail!("Unsupported output format: {}", other),
    };
    let mut f =
        File::create(out_path).with_context(|| format!("Failed to create {:?}", out_path))?;
    let bytes = match fmt {
        ImageFormat::Jpeg => {
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Jpeg)
                .context("Failed to encode JPEG")?;
            buf
        }
        _ => {
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), fmt)
                .context("Failed to encode image")?;
            buf
        }
    };
    f.write_all(&bytes)?;
    Ok(())
}

fn main() -> Result<()> {
    let raw_args: Vec<String> = env::args().collect();
    if raw_args.iter().any(|a| a == "-h" || a == "--help") {
        let mut buf: Vec<u8> = Vec::new();
        let mut cmd = Args::command();
        cmd.write_long_help(&mut buf)?;
        let s = String::from_utf8_lossy(&buf).to_string();
        let prog_name = Args::command().get_name().to_string();
        fn color_help(s: &str, prog_name: &str) -> String {
            let mut out = String::new();
            let mut prog_colored = false;
            for line in s.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && trimmed == trimmed.to_ascii_uppercase()
                    && trimmed.ends_with(':')
                {
                    out.push_str(&format!("{}\n", blue(trimmed)));
                    continue;
                }

                if !prog_colored && line.contains(prog_name) {
                    if let Some(pos) = line.find(prog_name) {
                        let (before, rest) = line.split_at(pos);
                        let rest = &rest[prog_name.len()..];
                        out.push_str(&format!(
                            "{}{}{}\n",
                            white(before),
                            pink(prog_name),
                            blue(rest)
                        ));
                        prog_colored = true;
                        continue;
                    }
                }
                if let Some(pos) = line.find("    ") {
                    let (left, right) = line.split_at(pos);
                    let right = &right[4..];
                    out.push_str(&format!("{}    {}\n", pink(left), dark(right)));
                } else {
                    if trimmed.is_empty() {
                        out.push_str("\n");
                    } else {
                        out.push_str(&format!("{}\n", white(line)));
                    }
                }
            }
            out
        }

        println!("{}", color_help(&s, &prog_name));
        return Ok(());
    }
    if raw_args.iter().any(|a| a == "--version" || a == "-v") {
        let prog_name = Args::command().get_name().to_string();
        println!("{} version {}", pink(prog_name), VERSION);
        #[cfg(feature = "include_exiftool")]
        {
            match exiftool::get_exiftool_version() {
                Ok(v) => println!("{} {}", blue("exiftool version:"), white(v)),
                Err(e) => println!(
                    "{} {}",
                    blue("exiftool version:"),
                    pink(format!("error retrieving version: {}", e))
                ),
            }
        }
        return Ok(());
    }

    let args = Args::parse();

    let out_formats: Vec<String> = args
        .format
        .split(|c| c == '+' || c == ',')
        .map(|s| {
            s.to_ascii_lowercase()
                .replace("jpg", "jpeg")
                .replace("tif", "tiff")
        })
        .collect();
    for f in &out_formats {
        if !VALID_FORMATS.contains(&f.as_str()) {
            anyhow::bail!(
                "Unsupported format: {}. Valid formats: {}",
                f,
                VALID_FORMATS
                    .into_iter()
                    .map(|s| blue(s).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if !(args.ratio > 0.0 && args.ratio <= 1.0) {
        anyhow::bail!("Resize ratio must be between 0 and 1");
    }

    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut out_dirs: Vec<PathBuf> = Vec::new();
    let mut out_files_for_single: Option<Vec<PathBuf>> = None;

    if args.input.len() == 1 && args.input[0].exists() && args.input[0].is_dir() {
        let input_dir = &args.input[0];
        let out_arg = match args.output_dir.as_ref() {
            Some(p) => p,
            None => anyhow::bail!("Output directory required when input is a directory"),
        };

        for fmt in &out_formats {
            let d = out_arg.join(fmt);
            fs::create_dir_all(&d).ok();
            out_dirs.push(d);
        }

        let mut nef_files: Vec<PathBuf> = fs::read_dir(input_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("nef"))
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        nef_files.sort();
        inputs = nef_files;
    } else {
        for p in &args.input {
            if p.exists() && p.is_file() {
                inputs.push(p.clone());
            }
        }
        if inputs.len() == 1 {
            let in_path = &inputs[0];
            match args.output_dir.as_ref() {
                Some(out_arg) => {
                    if out_arg.exists() && out_arg.is_dir() {
                        let in_stem = in_path.file_stem().unwrap().to_string_lossy();
                        let mut files = Vec::new();
                        for fmt in &out_formats {
                            let d = out_arg.join(format!("{}.{}", in_stem, fmt));
                            if let Some(p) = d.parent() {
                                fs::create_dir_all(p).ok();
                            }
                            files.push(d);
                        }
                        out_files_for_single = Some(files);
                    } else {
                        let out_path = out_arg.clone();
                        let parent = out_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| PathBuf::from("."));
                        fs::create_dir_all(&parent).ok();
                        if out_formats.len() == 1 {
                            out_files_for_single = Some(vec![out_path]);
                        } else {
                            let base = out_path
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| "output".to_string());
                            let mut vecp = Vec::new();
                            for fmt in &out_formats {
                                vecp.push(parent.join(format!("{}.{}", base, fmt)));
                            }
                            out_files_for_single = Some(vecp);
                        }
                    }
                }
                None => {
                    let parent = in_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| PathBuf::from("."));
                        let stem = in_path.file_stem().unwrap().to_string_lossy();
                        let mut files = Vec::new();
                        for fmt in &out_formats {
                            files.push(parent.join(format!("{}.{}", stem, fmt)));
                        }
                        out_files_for_single = Some(files);
                }
            }
        }
    }

    let total = inputs.len();
    if args.info {
        if inputs.len() == 1 {
            let p = &inputs[0];
            if p.exists() && p.is_file() {
                if let Err(e) = print_metadata(&p) {
                    eprintln!(
                        "{}",
                        pink(format!("Error reading metadata for {}: {}", p.display(), e))
                    );
                }
            } else {
                eprintln!("{}", pink(format!("Not a file: {}", p.display())));
            }
            println!("");
        } else {
            eprintln!("Info flag takes only one file.")
        }
        return Ok(());
    }
    if total == 0 {
        println!("No {} files found.", pink(".NEF"));
        return Ok(());
    }

    if cfg!(debug_assertions) && args.debug {
        eprintln!(
            "Running in {} mode, converting files will be {}",
            blue("debug"),
            red("slower")
        );
    }

    if let Some(method) = args.sort.as_ref() {
        if args.debug {
            eprintln!("Sorting {} inputs by {} method", inputs.len(), blue(method));
        }
        sort_inputs(&mut inputs, method.as_str(), args.debug);
    }

    if total == 1 && out_files_for_single.is_some() {
        let in_path = inputs.remove(0);
        let outs = out_files_for_single.take().unwrap();
        let out_desc = outs
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        let out_desc_cl = out_desc.clone();
        let spinner_run = Arc::new(AtomicBool::new(true));
        let spinner_flag = spinner_run.clone();
        let spinner_in = in_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| in_path.to_string_lossy().to_string());
        let handle = thread::spawn(move || {
            let frames = ["/", "-", "\\", "|"];
            let mut idx = 0usize;
            while spinner_flag.load(Ordering::SeqCst) {
                let frame = frames[idx % frames.len()];
                print!(
                    "\rConverting {} to {}... [{}]",
                    pink(&spinner_in),
                    blue(&out_desc_cl),
                    frame
                );
                std::io::stdout().flush().ok();
                idx = idx.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });

        let t0 = Instant::now();
        let brightness_mode = parse_brightness(&args.brightness);
        let auto_bright = matches!(brightness_mode, BrightnessMode::Auto);
        if !is_nef_file(&in_path) {
            spinner_run.store(false, Ordering::SeqCst);
            handle.join().ok();
            return Err(anyhow::anyhow!(pink(format!(
                "\n{}: {}",
                red("Not a NEF format"),
                in_path.display()
            ))));
        }
        let res = unsafe { load_with_libraw(&in_path, args.preview, args.debug, auto_bright) };
        match res {
            Ok(img) => {
                let mut img = resize_image(img, args.ratio);
                img = apply_brightness(img, brightness_mode);
                if let Some(rot) = args.rotation.as_ref() {
                    if rot == "auto" {
                        if let Ok(buf) = std::fs::read(&in_path) {
                            if let Ok(exif) = rexif::parse_buffer(&buf) {
                                for entry in exif.entries.iter() {
                                    let tag_name = format!("{}", entry.tag).to_lowercase();
                                    if tag_name.contains("orientation") {
                                        let sval = format!("{}", entry.value);
                                        if let Some(tok) = sval.split_whitespace().next() {
                                            if let Ok(code) = tok.parse::<u32>() {
                                                img = match code {
                                                    3 => image::DynamicImage::ImageRgba8(
                                                        image::imageops::rotate180(&img),
                                                    ),
                                                    6 => image::DynamicImage::ImageRgba8(
                                                        image::imageops::rotate90(&img),
                                                    ),
                                                    8 => image::DynamicImage::ImageRgba8(
                                                        image::imageops::rotate270(&img),
                                                    ),
                                                    _ => img,
                                                };
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    } else if let Ok(deg) = rot.parse::<i32>() {
                        img = match deg.rem_euclid(360) {
                            90 => image::DynamicImage::ImageRgba8(image::imageops::rotate90(&img)),
                            180 => {
                                image::DynamicImage::ImageRgba8(image::imageops::rotate180(&img))
                            }
                            270 => {
                                image::DynamicImage::ImageRgba8(image::imageops::rotate270(&img))
                            }
                            _ => img,
                        };
                    }
                }
                if args.enhance {
                    img = apply_brightness(img, BrightnessMode::Factor(1.05));
                    img = image::DynamicImage::ImageRgba8(image::imageops::unsharpen(&img, 1.0, 1));
                }
                for out_path in &outs {
                    let fmt = out_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("png")
                        .to_string();
                    if let Err(e) = save_image(&img, out_path, &fmt) {
                        spinner_run.store(false, Ordering::SeqCst);
                        handle.join().ok();
                        eprintln!(
                            "{}",
                            pink(format!(
                                "\n{} {}: {}",
                                red("Error saving"),
                                out_path.display(),
                                e
                            ))
                        );
                        return Err(e);
                    }
                }
                spinner_run.store(false, Ordering::SeqCst);
                handle.join().ok();
                let elapsed = t0.elapsed().as_secs_f64();
                println!(
                    "\rDone conversion, output file{}: {}",
                    if outs.len() > 1 { "s" } else { "" },
                    pink(out_desc)
                );
                println!(
                    "Total execution time: {}",
                    blue(format_time(elapsed as f64))
                );
                return Ok(());
            }
            Err(e) => {
                spinner_run.store(false, Ordering::SeqCst);
                handle.join().ok();
                eprintln!(
                    "{}",
                    pink(format!(
                        "\n{} {}: {}",
                        red("Error converting"),
                        in_path.display(),
                        e
                    ))
                );
                return Err(e);
            }
        }
    }

    println!(
        "{}\n",
        blue(format!("Found {} NEF files. Starting conversion...", total))
    );

    let threads = args.threads.unwrap_or_else(|| num_cpus::get());
    let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;
    let debug = args.debug;

    let start = Instant::now();
    let counter = Arc::new(Mutex::new(0usize));
    let stop_flag = Arc::new(AtomicBool::new(false));

    {
        let stop = stop_flag.clone();
        ctrlc::set_handler(move || {
            eprintln!("Received interrupt, stopping after current tasks...");
            stop.store(true, Ordering::SeqCst);
        })?;
    }

    let (tx, rx) = mpsc::channel::<String>();

    let printer = thread::spawn(move || {
        let mut converted = 0usize;
        while let Ok(msg) = rx.recv() {
            converted = converted.saturating_add(1);
            println!("[{}/{}] {}", converted, total, msg);
        }
    });

    let inputs_owned = inputs;
    pool.install(|| {
        inputs_owned.into_par_iter().for_each(|in_path| {
            if stop_flag.load(Ordering::SeqCst) {
                return;
            }
            let tx = tx.clone();
            let out_dirs = out_dirs.clone();
            let out_formats = out_formats.clone();
            let ratio = args.ratio;
            let preview = args.preview;
            let debug = debug;
            let brightness_mode = parse_brightness(&args.brightness);
            let rotation_opt = args.rotation.clone();
            let enhance_flag = args.enhance;
            let counter = counter.clone();
            let total = total;

            let t0 = Instant::now();
            let auto_bright = matches!(brightness_mode, BrightnessMode::Auto);
            if !is_nef_file(&in_path) {
                let fname = in_path.file_name().unwrap().to_string_lossy();
                tx.send(format!("{}... {}", fname, pink("Skipped (not NEF)")))
                    .ok();
                return;
            }
            let res = unsafe { load_with_libraw(&in_path, preview, debug, auto_bright) };
            match res {
                Ok(img) => {
                    let mut img = resize_image(img, ratio);
                    img = apply_brightness(img, brightness_mode);
                    if let Some(rot) = rotation_opt.as_ref() {
                        if rot == "auto" {
                            if let Ok(buf) = std::fs::read(&in_path) {
                                if let Ok(exif) = rexif::parse_buffer(&buf) {
                                    for entry in exif.entries.iter() {
                                        let tag_name = format!("{}", entry.tag).to_lowercase();
                                        if tag_name.contains("orientation") {
                                            let sval = format!("{}", entry.value);
                                            if let Some(tok) = sval.split_whitespace().next() {
                                                if let Ok(code) = tok.parse::<u32>() {
                                                    img = match code {
                                                        3 => image::DynamicImage::ImageRgba8(
                                                            image::imageops::rotate180(&img),
                                                        ),
                                                        6 => image::DynamicImage::ImageRgba8(
                                                            image::imageops::rotate90(&img),
                                                        ),
                                                        8 => image::DynamicImage::ImageRgba8(
                                                            image::imageops::rotate270(&img),
                                                        ),
                                                        _ => img,
                                                    };
                                                }
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        } else if let Ok(deg) = rot.parse::<i32>() {
                            img = match deg.rem_euclid(360) {
                                90 => {
                                    image::DynamicImage::ImageRgba8(image::imageops::rotate90(&img))
                                }
                                180 => image::DynamicImage::ImageRgba8(image::imageops::rotate180(
                                    &img,
                                )),
                                270 => image::DynamicImage::ImageRgba8(image::imageops::rotate270(
                                    &img,
                                )),
                                _ => img,
                            };
                        }
                    }
                    if enhance_flag {
                        img = apply_brightness(img, BrightnessMode::Factor(1.05));
                        img = image::DynamicImage::ImageRgba8(image::imageops::unsharpen(
                            &img, 1.0, 1,
                        ));
                    }
                    if let Some(ref single_outs) = out_files_for_single {
                        for (fmt, out_path) in out_formats.iter().zip(single_outs.iter()) {
                            if let Err(e) = save_image(&img, out_path, fmt) {
                                let fname = in_path.file_name().unwrap().to_string_lossy();
                                tx.send(format!("{}... {}: {}", fname, red("Error saving"), e))
                                    .ok();
                                return;
                            }
                        }
                    } else {
                        let fname = in_path.file_name().unwrap().to_string_lossy();
                        if out_dirs.is_empty() {
                            let parent = in_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| PathBuf::from("."));
                            for fmt in out_formats.iter() {
                                let out_name = format!(
                                    "{}.{}",
                                    in_path.file_stem().unwrap().to_string_lossy(),
                                    fmt
                                );
                                let out_path = parent.join(out_name);
                                if let Err(e) = save_image(&img, &out_path, fmt) {
                                    tx.send(format!("{}... {}: {}", fname, red("Error saving"), e))
                                        .ok();
                                    return;
                                }
                            }
                        } else {
                            for (fmt, out_dir) in out_formats.iter().zip(out_dirs.iter()) {
                                let out_name = format!(
                                    "{}.{}",
                                    in_path.file_stem().unwrap().to_string_lossy(),
                                    fmt
                                );
                                let out_path = out_dir.join(out_name);
                                if let Err(e) = save_image(&img, &out_path, fmt) {
                                    tx.send(format!("{}... {}: {}", fname, red("Error saving"), e))
                                        .ok();
                                    return;
                                }
                            }
                        }
                    }
                    let elapsed = t0.elapsed().as_secs_f64();
                    let mut done = counter.lock().unwrap();
                    *done += 1;
                    let avg = start.elapsed().as_secs_f64() / (*done as f64);
                    let remaining = avg * ((total - *done) as f64);
                    let name_for_msg = in_path.file_name().unwrap().to_string_lossy();
                    tx.send(format!(
                        "{} → {}... Done ({}).\n   ↳ Est. time left: {}",
                        pink(name_for_msg),
                        blue(out_formats.join("+")),
                        format_time(elapsed),
                        format_time(remaining)
                    ))
                    .ok();
                }
                Err(e) => {
                    let name_for_msg = in_path.file_name().unwrap().to_string_lossy();
                    tx.send(format!("{}... {}: {}", name_for_msg, red("Error"), e))
                        .ok();
                }
            }
        });
    });

    drop(tx);
    printer.join().ok();

    let total_time = start.elapsed().as_secs_f64();
    if stop_flag.load(Ordering::SeqCst) {
        println!("\n{}", red("Stopped early."));
    } else {
        println!("\n{}", green("All conversions completed."));
    }
    println!("Total execution time: {}", blue(format_time(total_time)));

    Ok(())
}
