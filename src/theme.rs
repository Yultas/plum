//! Theme loaded from theme.toml (exe-dir first, then %APPDATA%\plum).

use ratatui::style::Color;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub surface2: Color,
    pub surface_text: Color,
    pub text: Color,
    pub dim: Color,
    pub meta: Color,
    pub accent: Color,
    pub ok: Color,
    pub err: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            bg: Color::Rgb(0x18, 0x17, 0x25),
            surface2: Color::Rgb(0x3F, 0x41, 0x4E),
            surface_text: Color::Rgb(0xFF, 0xFF, 0xFF),
            text: Color::Rgb(0xEB, 0xEA, 0xEC),
            dim: Color::Rgb(0xA1, 0xA4, 0xB2),
            meta: Color::Rgb(0x5E, 0x63, 0x78),
            accent: Color::Rgb(0x8E, 0x97, 0xFD),
            ok: Color::Rgb(0x6C, 0xB2, 0x8E),
            err: Color::Rgb(0xFA, 0x6E, 0x5A),
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Find the effective theme config: exe-dir/theme.toml, then
/// %APPDATA%\plum\theme.toml. Returns the existing path, if any.
pub fn config_candidates(exe_dir: Option<&Path>, appdata_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join("theme.toml"));
    }
    if let Some(dir) = appdata_dir {
        candidates.push(dir.join("theme.toml"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn init(config_path: Option<&Path>) {
    let mut t = Theme::default();
    if let Some(p) = config_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((key, val)) = line.split_once('=') else { continue };
                let key = key.trim();
                let Some(color) = parse_hex(val) else { continue };
                match key {
                    "bg" => t.bg = color,
                    "surface" => t.surface2 = color,
                    "surface-text" | "surface_text" => t.surface_text = color,
                    "text" => t.text = color,
                    "dim" => t.dim = color,
                    "meta" => t.meta = color,
                    "accent" => t.accent = color,
                    "ok" => t.ok = color,
                    "err" => t.err = color,
                    _ => {}
                }
            }
        }
    }
    let _ = THEME.set(t);
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some(Color::Rgb(
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ))
}

pub fn t() -> &'static Theme {
    THEME.get().expect("theme initialized")
}