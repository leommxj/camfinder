use anyhow::{Context, Result};
use camfinder::config::{PreferenceField, ScanPreferences as Settings};
use camfinder::engine::{ScanSnapshot, SessionStatus};
use camfinder::persistence::StabilityState;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::ttf::{Font, Sdl2TtfContext};
use sdl2::video::Window;
use std::path::Path;

const BG: Color = Color::RGB(12, 19, 26);
const PANEL: Color = Color::RGB(24, 35, 44);
const TEXT: Color = Color::RGB(231, 240, 241);
const MUTED: Color = Color::RGB(143, 163, 174);
const ACCENT: Color = Color::RGB(66, 220, 170);
const WARNING: Color = Color::RGB(255, 185, 83);
const SCAN_ROWS: usize = 6;
const SCAN_TOP: i32 = 27;
const SCAN_ROW_HEIGHT: u32 = 20;

pub struct Fonts<'a> {
    body: Font<'a, 'static>,
    title: Font<'a, 'static>,
}

impl<'a> Fonts<'a> {
    pub fn load(ttf: &'a Sdl2TtfContext) -> Result<Self> {
        let directory = [
            "/usr/share/fonts/truetype/dejavu",
            "/usr/share/fonts/dejavu",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| path.join("DejaVuSans.ttf").exists())
        .context("DejaVu Sans is required; install fonts-dejavu-core")?;
        Ok(Self {
            body: ttf
                .load_font(directory.join("DejaVuSans.ttf"), 12)
                .map_err(anyhow::Error::msg)?,
            title: ttf
                .load_font(directory.join("DejaVuSans-Bold.ttf"), 14)
                .map_err(anyhow::Error::msg)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Scan,
    Detail,
    Config,
    Status,
}

pub struct Ui {
    pub page: Page,
    selected: usize,
    scan_start: usize,
    field: usize,
}

impl Ui {
    pub fn new(page: Page) -> Self {
        Self {
            page,
            selected: 0,
            scan_start: 0,
            field: 0,
        }
    }

    pub fn navigate(&mut self, key: Keycode, settings: &mut Settings) {
        let vertical = match key {
            Keycode::Up | Keycode::F => -1,
            Keycode::Down | Keycode::X => 1,
            _ => 0,
        };
        let horizontal = match key {
            Keycode::Left | Keycode::Z => -1,
            Keycode::Right | Keycode::C => 1,
            _ => 0,
        };
        if self.page == Page::Config {
            self.field = (self.field as i32 + vertical).rem_euclid(3) as usize;
            let field = [
                PreferenceField::Profile,
                PreferenceField::Gain,
                PreferenceField::Recording,
            ][self.field];
            settings.adjust(field, horizontal);
        } else if vertical < 0 || horizontal < 0 {
            self.selected = self.selected.saturating_sub(1);
        } else if vertical > 0 || horizontal > 0 {
            self.selected = self.selected.saturating_add(1);
        }
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        settings: &Settings,
        snapshot: &ScanSnapshot,
        demo: bool,
    ) -> Result<()> {
        canvas.set_draw_color(BG);
        canvas.clear();
        self.selected = self
            .selected
            .min(snapshot.persistence.len().saturating_sub(1));
        if self.page == Page::Scan {
            self.scan_header(canvas, fonts, snapshot, demo)?;
        } else {
            text(canvas, &fonts.title, 8, 4, 180, TEXT, "Camfinder")?;
            text(
                canvas,
                &fonts.body,
                237,
                6,
                77,
                if demo { WARNING } else { ACCENT },
                if demo { "DEMO" } else { "AIRSPY" },
            )?;
            fill(canvas, Rect::new(8, 27, 304, 1), PANEL)?;
        }
        if let Some(error) = snapshot.error.as_ref().filter(|_| self.page == Page::Scan) {
            text(canvas, &fonts.title, 8, 35, 304, WARNING, "Scan stopped")?;
            for (line, value) in error
                .chars()
                .collect::<Vec<_>>()
                .chunks(41)
                .take(3)
                .enumerate()
            {
                text(
                    canvas,
                    &fonts.body,
                    8,
                    58 + line as i32 * 17,
                    304,
                    TEXT,
                    &value.iter().collect::<String>(),
                )?;
            }
            text(
                canvas,
                &fonts.body,
                8,
                117,
                304,
                MUTED,
                "Reconnect Airspy, then press 8 to retry.",
            )?;
        } else {
            match self.page {
                Page::Scan => self.scan(canvas, fonts, snapshot)?,
                Page::Detail => self.detail(canvas, fonts, snapshot)?,
                Page::Config => self.config(canvas, fonts, settings)?,
                Page::Status => self.status(canvas, fonts, snapshot)?,
            }
        }
        for (index, (page, label)) in [
            (Page::Scan, "4 Scan"),
            (Page::Detail, "5 Detail"),
            (Page::Config, "6 Setup"),
            (Page::Status, "7 Stats"),
        ]
        .into_iter()
        .enumerate()
        {
            let x = 24 + index as i32 * 56;
            if self.page == page {
                fill(canvas, Rect::new(x, 147, 52, 23), PANEL)?;
            }
            text(
                canvas,
                &fonts.body,
                x + 2,
                151,
                50,
                if self.page == page { ACCENT } else { MUTED },
                label,
            )?;
        }
        let label = match snapshot.status {
            SessionStatus::Paused => "8 Run",
            SessionStatus::Error | SessionStatus::Stopped => "8 Retry",
            _ => "8 Pause",
        };
        text(canvas, &fonts.body, 250, 151, 65, ACCENT, label)?;
        canvas.present();
        Ok(())
    }

    fn scan_header(
        &self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        snapshot: &ScanSnapshot,
        demo: bool,
    ) -> Result<()> {
        let state = match snapshot.status {
            SessionStatus::Paused => "Paused",
            SessionStatus::Starting => "Starting",
            SessionStatus::Error | SessionStatus::Stopped => "Stopped",
            SessionStatus::Scanning => "Scan",
        };
        let total = snapshot.persistence.len();
        let count = if total == 0 {
            "Found 0".into()
        } else {
            format!("Found {}/{}", self.selected + 1, total)
        };
        let count_width = fonts.body.size_of(&count)?.0.min(128);
        text(
            canvas,
            &fonts.body,
            8,
            5,
            296 - count_width,
            if demo { WARNING } else { TEXT },
            &format!(
                "{}{state} {}/{}  {:.2}s",
                if demo { "Demo " } else { "" },
                snapshot.completed_targets,
                snapshot.total_targets,
                snapshot.pass_seconds,
            ),
        )?;
        text(
            canvas,
            &fonts.body,
            312 - count_width as i32,
            5,
            count_width,
            ACCENT,
            &count,
        )?;
        fill(canvas, Rect::new(8, 24, 304, 1), PANEL)
    }

    fn scan(
        &mut self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        snapshot: &ScanSnapshot,
    ) -> Result<()> {
        if snapshot.persistence.is_empty() {
            self.scan_start = 0;
            text(canvas, &fonts.title, 8, 63, 304, TEXT, "No candidates yet")?;
            text(
                canvas,
                &fonts.body,
                8,
                87,
                304,
                MUTED,
                if snapshot.pass == 0 {
                    "Waiting for a complete scan."
                } else {
                    "Scanning for harmonic candidates."
                },
            )?;
        } else {
            let total = snapshot.persistence.len();
            self.scan_start = self
                .scan_start
                .min(self.selected)
                .max(self.selected.saturating_sub(SCAN_ROWS - 1))
                .min(total.saturating_sub(SCAN_ROWS));
            for (row, item) in snapshot
                .persistence
                .iter()
                .skip(self.scan_start)
                .take(SCAN_ROWS)
                .enumerate()
            {
                let y = SCAN_TOP + row as i32 * SCAN_ROW_HEIGHT as i32;
                if row + self.scan_start == self.selected {
                    fill(canvas, Rect::new(6, y, 302, SCAN_ROW_HEIGHT - 1), PANEL)?;
                }
                let (state, color) = match item.state {
                    StabilityState::Stable => ("Stable", ACCENT),
                    StabilityState::Candidate => ("New", WARNING),
                    StabilityState::Tracking => ("Track", MUTED),
                    _ => ("Absent", MUTED),
                };
                text(
                    canvas,
                    &fonts.body,
                    10,
                    y + 2,
                    76,
                    TEXT,
                    group(&item.profile),
                )?;
                text(canvas, &fonts.body, 90, y + 2, 68, color, state)?;
                text(
                    canvas,
                    &fonts.body,
                    161,
                    y + 2,
                    145,
                    TEXT,
                    &format!("{} hits   {:.0} dB", item.last_hits, item.last_snr_avg_db),
                )?;
            }
            if total > SCAN_ROWS {
                let height = SCAN_ROWS as u32 * SCAN_ROW_HEIGHT - 2;
                let thumb = (height * SCAN_ROWS as u32 / total as u32).max(8);
                let offset = (height - thumb) as usize * self.scan_start / (total - SCAN_ROWS);
                fill(canvas, Rect::new(312, SCAN_TOP, 2, height), PANEL)?;
                fill(
                    canvas,
                    Rect::new(312, SCAN_TOP + offset as i32, 2, thumb),
                    MUTED,
                )?;
            }
        }
        Ok(())
    }

    fn detail(
        &mut self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        snapshot: &ScanSnapshot,
    ) -> Result<()> {
        if snapshot.persistence.is_empty() {
            return text(
                canvas,
                &fonts.body,
                8,
                55,
                304,
                MUTED,
                "Complete a scan to inspect candidates.",
            );
        }
        let summary = &snapshot.persistence[self.selected];
        text(
            canvas,
            &fonts.title,
            8,
            32,
            304,
            TEXT,
            &format!(
                "{}  |  Candidate {}/{}",
                group(&summary.profile),
                self.selected + 1,
                snapshot.persistence.len()
            ),
        )?;
        let item = summary.current_result(&snapshot.results);
        let Some(item) = item else {
            return text(
                canvas,
                &fonts.body,
                8,
                65,
                304,
                MUTED,
                "This candidate was absent in the last pass.",
            );
        };
        text(
            canvas,
            &fonts.body,
            8,
            54,
            304,
            ACCENT,
            &format!(
                "{} hits   Score {:.1}   SNR {:.1} dB",
                item.hits, item.score, item.snr_avg_db
            ),
        )?;
        text(
            canvas,
            &fonts.body,
            8,
            72,
            304,
            MUTED,
            &format!(
                "Offset {:+.0} Hz   Spread {:.0} Hz",
                item.offset_group_hz.unwrap_or_default(),
                item.offset_spread_hz
            ),
        )?;
        for (row, hit) in item.hit_details.iter().take(3).enumerate() {
            text(
                canvas,
                &fonts.body,
                8,
                93 + row as i32 * 16,
                304,
                TEXT,
                &format!("{:.6} MHz     {:.1} dB", hit.observed_hz / 1e6, hit.snr_db),
            )?;
        }
        Ok(())
    }

    fn config(
        &self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        settings: &Settings,
    ) -> Result<()> {
        text(canvas, &fonts.title, 8, 32, 304, TEXT, "Scan setup")?;
        let gain = settings.gain().to_string();
        for (row, (label, value)) in [
            ("Profile", settings.label()),
            ("VGA gain", gain.as_str()),
            ("Recording", settings.record_label()),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 56 + row as i32 * 23;
            if row == self.field {
                fill(canvas, Rect::new(6, y - 1, 308, 22), PANEL)?;
            }
            text(canvas, &fonts.body, 10, y + 2, 98, MUTED, label)?;
            text(
                canvas,
                &fonts.body,
                112,
                y + 2,
                194,
                if row == self.field { ACCENT } else { TEXT },
                value,
            )?;
        }
        text(
            canvas,
            &fonts.body,
            8,
            128,
            304,
            MUTED,
            "Arrows change  |  Enter saves and starts",
        )
    }

    fn status(
        &self,
        canvas: &mut Canvas<Window>,
        fonts: &Fonts<'_>,
        snapshot: &ScanSnapshot,
    ) -> Result<()> {
        text(canvas, &fonts.title, 8, 32, 304, TEXT, "Scan timing")?;
        if let Some(item) = snapshot.statistics.last() {
            for (row, value) in [
                format!("Last point {:.3} MHz", item.target_hz / 1e6),
                format!(
                    "{} IQ samples / {} FFT frames",
                    item.iq_samples, item.fft_frames
                ),
                format!(
                    "Capture {:.1} ms   Analysis {:.1} ms",
                    item.capture_ms, item.analysis_ms
                ),
                format!(
                    "Queue {:.1} ms   Tune {:.1} ms",
                    item.queue_wait_ms, item.tune_ms
                ),
                format!("Timeouts {}   Overflows {}", item.timeouts, item.overflows),
            ]
            .into_iter()
            .enumerate()
            {
                text(
                    canvas,
                    &fonts.body,
                    8,
                    57 + row as i32 * 17,
                    304,
                    TEXT,
                    &value,
                )?;
            }
        } else {
            text(
                canvas,
                &fonts.body,
                8,
                63,
                304,
                MUTED,
                "Hardware timing appears after a scan.",
            )?;
            text(
                canvas,
                &fonts.body,
                8,
                86,
                304,
                TEXT,
                &format!(
                    "{} points   {} passes   {:.1} seconds",
                    snapshot.total_targets, snapshot.pass, snapshot.elapsed_seconds
                ),
            )?;
        }
        Ok(())
    }
}

fn group(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn fill(canvas: &mut Canvas<Window>, rect: Rect, color: Color) -> Result<()> {
    canvas.set_draw_color(color);
    canvas.fill_rect(rect).map_err(anyhow::Error::msg)
}

fn text(
    canvas: &mut Canvas<Window>,
    font: &Font<'_, '_>,
    x: i32,
    y: i32,
    width: u32,
    color: Color,
    value: &str,
) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let mut value = value.to_string();
    if font.size_of(&value)?.0 > width {
        while !value.is_empty() && font.size_of(&format!("{value}…"))?.0 > width {
            value.pop();
        }
        value.push('…');
    }
    let surface = font.render(&value).blended(color)?;
    let texture_creator = canvas.texture_creator();
    let texture = texture_creator.create_texture_from_surface(&surface)?;
    canvas
        .copy(
            &texture,
            None,
            Rect::new(x, y, surface.width(), surface.height()),
        )
        .map_err(anyhow::Error::msg)
}
