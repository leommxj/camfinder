mod platform;
mod ui;

use anyhow::{anyhow, bail, Context, Result};
use camfinder::config::SettingsStore;
use camfinder::engine::{ScanSession, SessionStatus};
use camfinder::timeparse::parse_duration;
use sdl2::{event::Event, keyboard::Keycode, pixels::PixelFormatEnum};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use ui::{Page, Ui};

struct Options {
    demo: bool,
    duration: Option<Duration>,
    screenshot: Option<PathBuf>,
    page: Page,
}

fn options() -> Result<Options> {
    let mut result = Options {
        demo: false,
        duration: None,
        screenshot: None,
        page: Page::Scan,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => result.demo = true,
            "--duration" => {
                result.duration = Some(parse_duration(
                    &args.next().context("--duration requires a value")?,
                )?);
            }
            "--screenshot" => {
                result.screenshot =
                    Some(args.next().context("--screenshot requires a path")?.into())
            }
            "--page" => {
                result.page = match args.next().as_deref() {
                    Some("scan") => Page::Scan,
                    Some("detail") => Page::Detail,
                    Some("config") => Page::Config,
                    Some("status") => Page::Status,
                    _ => bail!("page must be scan, detail, config or status"),
                }
            }
            "--help" | "-h" => {
                println!("Camfinder for CardputerZero\n\n  --demo                 Scan synthetic signals without an Airspy\n  --duration SECONDS     Exit after this duration\n  --screenshot PATH      Save a 320x170 PNG on exit\n  --page PAGE            scan, detail, config or status\n\n4 Scan  5 Details  6 Setup  7 Status  8 Pause/Resume\nArrows or F/Z/X/C navigate; Enter applies setup; Esc quits.");
                std::process::exit(0);
            }
            _ => bail!("unknown option {arg}; use --help"),
        }
    }
    if result.screenshot.is_some() && result.duration.is_none() {
        result.duration = Some(Duration::from_secs(2));
    }
    Ok(result)
}

fn main() -> Result<()> {
    let options = options()?;
    let store = SettingsStore::default();
    let mut settings = store.load()?;
    let mut panel = platform::Panel::open()?;
    if options.screenshot.is_some()
        && !panel.active()
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
        && std::env::var_os("SDL_VIDEODRIVER").is_none()
    {
        std::env::set_var("SDL_VIDEODRIVER", "dummy");
    }
    let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
    let video = sdl.video().map_err(anyhow::Error::msg)?;
    let ttf = sdl2::ttf::init().map_err(anyhow::Error::msg)?;
    let fonts = ui::Fonts::load(&ttf)?;
    let window = video
        .window("Camfinder", 320, 170)
        .position_centered()
        .build()?;
    let mut canvas = window.into_canvas().software().build()?;
    let mut events = sdl.event_pump().map_err(anyhow::Error::msg)?;
    let mut ui = Ui::new(options.page);
    let mut resolved = settings.resolve(&store, options.demo)?;
    resolved.run.duration = options.duration;
    let mut session = ScanSession::start_with_options(resolved.engine, resolved.run)?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Relaxed))?;
    loop {
        let mut keys = panel.keys();
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => stop.store(true, Ordering::Relaxed),
                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => keys.push((key, true)),
                _ => (),
            }
        }
        for (key, pressed) in keys {
            if !pressed {
                continue;
            }
            match key {
                Keycode::Escape | Keycode::Q => stop.store(true, Ordering::Relaxed),
                Keycode::Num4 => ui.page = Page::Scan,
                Keycode::Num5 => ui.page = Page::Detail,
                Keycode::Num6 => ui.page = Page::Config,
                Keycode::Num7 => ui.page = Page::Status,
                Keycode::Num8 | Keycode::Space => session.toggle_pause()?,
                Keycode::Return if ui.page == Page::Config => {
                    session.save_preferences(&settings, &store, options.demo)?;
                    ui.page = Page::Scan;
                }
                Keycode::Return => ui.page = Page::Detail,
                key => ui.navigate(key, &mut settings),
            }
        }
        let snapshot = session.snapshot();
        ui.draw(&mut canvas, &fonts, &settings, &snapshot, options.demo)?;
        if panel.active() {
            panel.present(
                &canvas
                    .read_pixels(None, PixelFormatEnum::RGB24)
                    .map_err(anyhow::Error::msg)?,
            )?;
        }
        let finished = stop.load(Ordering::Relaxed)
            || snapshot.status == SessionStatus::Stopped
            || (options.duration.is_some() && snapshot.status == SessionStatus::Error);
        if finished {
            if let Some(path) = &options.screenshot {
                if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let pixels = canvas
                    .read_pixels(None, PixelFormatEnum::RGB24)
                    .map_err(anyhow::Error::msg)?;
                let mut encoder = png::Encoder::new(std::fs::File::create(path)?, 320, 170);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header()?.write_image_data(&pixels)?;
            }
            session.stop()?;
            if let Some(error) = snapshot.error {
                return Err(anyhow!(error));
            }
            println!(
                "{}",
                serde_json::json!({"backend": if options.demo { "synthetic" } else { "airspy" }, "passes": snapshot.pass, "targets": snapshot.total_targets, "peaks": snapshot.peaks.len(), "candidates": snapshot.persistence.len()})
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
