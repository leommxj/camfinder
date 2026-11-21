use anyhow::{bail, ensure, Context, Result};
use sdl2::keyboard::Keycode;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};

const WIDTH: usize = 320;
const HEIGHT: usize = 170;

pub struct Panel {
    framebuffer: Option<File>,
    pixels: Vec<u8>,
    inputs: Vec<File>,
}

impl Panel {
    pub fn open() -> Result<Self> {
        let requested = std::env::var_os("CAMFINDER_FRAMEBUFFER").filter(|path| !path.is_empty());
        let automatic = cfg!(target_arch = "aarch64")
            && std::env::var_os("DISPLAY")
                .filter(|display| !display.is_empty())
                .is_none()
            && std::env::var_os("WAYLAND_DISPLAY")
                .filter(|display| !display.is_empty())
                .is_none()
            && std::path::Path::new("/dev/fb0").exists();
        if requested.is_none() && !automatic {
            return Ok(Self {
                framebuffer: None,
                pixels: Vec::new(),
                inputs: Vec::new(),
            });
        }
        if !cfg!(all(target_os = "linux", target_pointer_width = "64")) {
            bail!("framebuffer input requires 64-bit Linux");
        }
        let path = requested.unwrap_or_else(|| "/dev/fb0".into());
        let size = fs::read_to_string("/sys/class/graphics/fb0/virtual_size")?;
        let depth = fs::read_to_string("/sys/class/graphics/fb0/bits_per_pixel")?;
        let stride = fs::read_to_string("/sys/class/graphics/fb0/stride")?;
        if size.trim() != "320,170" || depth.trim() != "16" || stride.trim() != "640" {
            bail!("expected a 320x170 RGB565 framebuffer with stride 640");
        }
        let framebuffer = Some(
            OpenOptions::new()
                .write(true)
                .open(path)
                .context("open framebuffer as ordinary user")?,
        );
        std::env::set_var("SDL_VIDEODRIVER", "dummy");
        let mut inputs = Vec::new();
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        for entry in fs::read_dir("/dev/input")? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("event") {
                if let Ok(file) = OpenOptions::new()
                    .read(true)
                    .custom_flags(0x800)
                    .open(entry.path())
                {
                    inputs.push(file);
                }
            }
        }
        if inputs.is_empty() {
            bail!("no input devices readable; launcher user needs input group access");
        }
        Ok(Self {
            framebuffer,
            pixels: vec![0; WIDTH * HEIGHT * 2],
            inputs,
        })
    }

    pub fn active(&self) -> bool {
        self.framebuffer.is_some()
    }

    pub fn present(&mut self, rgb: &[u8]) -> Result<()> {
        if let Some(framebuffer) = &mut self.framebuffer {
            ensure!(
                rgb.len() == WIDTH * HEIGHT * 3,
                "expected 320x170 RGB24 pixels"
            );
            for (output, pixel) in self.pixels.chunks_exact_mut(2).zip(rgb.chunks_exact(3)) {
                let value = ((pixel[0] as u16 >> 3) << 11)
                    | ((pixel[1] as u16 >> 2) << 5)
                    | (pixel[2] as u16 >> 3);
                output.copy_from_slice(&value.to_le_bytes());
            }
            framebuffer.seek(SeekFrom::Start(0))?;
            framebuffer.write_all(&self.pixels)?;
        }
        Ok(())
    }

    pub fn keys(&mut self) -> Vec<(Keycode, bool)> {
        let mut keys = Vec::new();
        for input in &mut self.inputs {
            loop {
                let mut bytes = [0u8; 24];
                match input.read(&mut bytes) {
                    Ok(24) if u16::from_ne_bytes([bytes[16], bytes[17]]) == 1 => {
                        let code = u16::from_ne_bytes([bytes[18], bytes[19]]);
                        let value = i32::from_ne_bytes(bytes[20..24].try_into().unwrap());
                        if value == 2 {
                            continue;
                        }
                        let key = match code {
                            1 => Keycode::Escape,
                            5 => Keycode::Num4,
                            6 => Keycode::Num5,
                            7 => Keycode::Num6,
                            8 => Keycode::Num7,
                            9 => Keycode::Num8,
                            28 => Keycode::Return,
                            33 | 103 => Keycode::Up,
                            44 | 105 => Keycode::Left,
                            45 | 108 => Keycode::Down,
                            46 | 106 => Keycode::Right,
                            57 => Keycode::Space,
                            _ => continue,
                        };
                        keys.push((key, value == 1));
                    }
                    Ok(24) => continue,
                    _ => break,
                }
            }
        }
        keys
    }
}
