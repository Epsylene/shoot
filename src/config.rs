use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Error;

#[derive(Clone, Copy)]
pub struct Flash {
    pub alpha: u32,
    pub hold: Duration,
    pub fade: Duration,
}

pub struct Config {
    pub directory: PathBuf,
    pub filename: String,
    pub flash: Flash,
    pub area_dim: u16,
}

enum Value {
    Str(String),
    Int(i64),
}

pub fn path() -> Result<PathBuf, Error> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".config"),
    };
    Ok(base.join("shoot").join("config.toml"))
}

fn home() -> Result<PathBuf, Error> {
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?))
}

fn expand(dir: &str) -> Result<PathBuf, Error> {
    Ok(match dir.strip_prefix("~/") {
        Some(rest) => home()?.join(rest),
        None if dir == "~" => home()?,
        None => PathBuf::from(dir),
    })
}

pub fn load() -> Result<Config, Error> {
    let path = path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    parse(&text, &path)
}

fn parse(text: &str, path: &Path) -> Result<Config, Error> {
    let mut directory = String::from("~/Pictures");
    let mut filename = String::from("Screenshot from %Y-%m-%d %H-%M-%S");
    let mut flash_opacity = 38;
    let mut flash_hold_ms = 60;
    let mut flash_fade_ms = 144;
    let mut area_brightness = 55;

    let mut seen = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let at = |msg: String| -> Error { format!("{}:{}: {msg}", path.display(), n + 1).into() };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line.split_once('=').ok_or_else(|| at("expected 'key = value'".into()))?;
        let key = key.trim();
        let value = parse_value(raw.trim()).map_err(at)?;
        if seen.contains(&key) {
            return Err(at(format!("'{key}' is set twice")));
        }
        seen.push(key);

        let int = |min: i64, max: i64| match value {
            Value::Int(v) if (min..=max).contains(&v) => Ok(v),
            Value::Int(v) => Err(at(format!("'{key}' must be between {min} and {max}, got {v}"))),
            Value::Str(_) => Err(at(format!("'{key}' must be a number"))),
        };
        let string = || match &value {
            Value::Str(s) => Ok(s.clone()),
            Value::Int(_) => Err(at(format!("'{key}' must be a quoted string"))),
        };
        match key {
            "directory" => directory = string()?,
            "filename" => {
                filename = string()?;
                if filename.is_empty() || filename.contains('/') {
                    return Err(at("'filename' must be non-empty and contain no '/'".into()));
                }
            }
            "flash_opacity" => flash_opacity = int(0, 100)?,
            "flash_hold_ms" => flash_hold_ms = int(0, 5000)?,
            "flash_fade_ms" => flash_fade_ms = int(0, 5000)?,
            "area_brightness" => area_brightness = int(0, 100)?,
            _ => return Err(at(format!("unknown key '{key}'"))),
        }
    }

    Ok(Config {
        directory: expand(&directory)?,
        filename,
        flash: Flash {
            alpha: (flash_opacity * 255 / 100) as u32,
            hold: Duration::from_millis(flash_hold_ms as u64),
            fade: Duration::from_millis(flash_fade_ms as u64),
        },
        area_dim: (area_brightness * 256 / 100) as u16,
    })
}

fn parse_value(raw: &str) -> Result<Value, String> {
    if let Some(body) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    let rest = chars.as_str().trim();
                    if !rest.is_empty() && !rest.starts_with('#') {
                        return Err(format!("unexpected text after string: {rest}"));
                    }
                    return Ok(Value::Str(out));
                }
                '\\' => match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    other => return Err(format!("unsupported escape '\\{}'", other.unwrap_or(' '))),
                },
                c => out.push(c),
            }
        }
        return Err("unterminated string".into());
    }
    let token = raw.split('#').next().unwrap_or("").trim();
    token
        .parse()
        .map(Value::Int)
        .map_err(|_| format!("expected a number or a quoted string, got '{token}'"))
}
