use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::quote::normalize_symbol;

pub const MAX_SYMBOLS: usize = 50;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedMode {
    #[default]
    Auto,
    Tencent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub symbols: Vec<String>,
    pub opacity: f32,
    pub always_on_top: bool,
    pub compact: bool,
    pub poll_seconds: u64,
    pub feed_mode: FeedMode,
    pub position: Option<[i32; 2]>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            symbols: vec!["sh000001".into(), "sh600519".into(), "sz000001".into()],
            opacity: 0.92,
            always_on_top: true,
            compact: false,
            poll_seconds: 3,
            feed_mode: FeedMode::Auto,
            position: None,
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        let mut symbols = Vec::new();
        for symbol in &self.symbols {
            if let Ok(symbol) = normalize_symbol(symbol)
                && !symbols.contains(&symbol)
                && symbols.len() < MAX_SYMBOLS
            {
                symbols.push(symbol);
            }
        }
        self.symbols = symbols;
        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.0, 1.0)
        } else {
            0.92
        };
        self.poll_seconds = self.poll_seconds.clamp(2, 60);
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    pub path: PathBuf,
}

impl Default for ConfigStore {
    fn default() -> Self {
        let base = std::env::var_os("MINI_STOCK_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
                    .unwrap_or_else(|| {
                        std::env::var_os("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(std::env::temp_dir)
                            .join(".config")
                    })
                    .join("MiniStockMonitor")
            });
        Self {
            path: base.join("settings.json"),
        }
    }
}

impl ConfigStore {
    pub fn load(&self) -> (Settings, Option<String>) {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Settings::default(), None);
            }
            Err(error) => return (Settings::default(), Some(format!("配置读取失败：{error}"))),
        };
        match serde_json::from_slice::<Settings>(&raw) {
            Ok(mut settings) => {
                settings.sanitize();
                (settings, None)
            }
            Err(_) => {
                let backup = self.path.with_file_name(format!(
                    "settings.invalid-{}.json",
                    chrono::Utc::now().timestamp_millis()
                ));
                let message = if fs::copy(&self.path, backup).is_ok() {
                    "配置格式有误，已备份原文件并使用默认设置"
                } else {
                    "配置格式有误，原文件无法备份；本次使用默认设置"
                };
                (Settings::default(), Some(message.into()))
            }
        }
    }

    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("invalid config path"))?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_keeps_empty_watchlist_and_replaces_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore {
            path: directory.path().join("settings.json"),
        };
        let mut settings = Settings::default();
        store.save(&settings).unwrap();
        settings.symbols.clear();
        settings.opacity = 0.0;
        settings.position = Some([-1000, 240]);
        store.save(&settings).unwrap();
        let (loaded, warning) = store.load();
        assert!(warning.is_none());
        assert!(loaded.symbols.is_empty());
        assert_eq!(loaded.opacity, 0.0);
        assert_eq!(loaded.position, settings.position);
    }

    #[test]
    fn corrupted_config_is_preserved_before_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore {
            path: directory.path().join("settings.json"),
        };
        fs::write(&store.path, b"{broken").unwrap();
        assert!(store.load().1.is_some());
        assert!(fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.invalid-")
        }));
    }

    #[test]
    fn sanitizes_duplicate_and_malicious_symbols() {
        let mut settings = Settings {
            symbols: vec!["600519".into(), "sh600519".into(), "sz000001&x=y".into()],
            poll_seconds: 0,
            opacity: 4.0,
            ..Settings::default()
        };
        settings.sanitize();
        assert_eq!(settings.symbols, ["sh600519"]);
        assert_eq!(settings.poll_seconds, 2);
        assert_eq!(settings.opacity, 1.0);
    }

    #[test]
    fn mixed_hk_watchlist_survives_save_reload_and_deduplicates_code_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore {
            path: directory.path().join("settings.json"),
        };
        let mut settings = Settings {
            symbols: ["00700", "HK700", "700.HK", "09988", "600519", "000001"]
                .map(str::to_owned)
                .into(),
            ..Settings::default()
        };
        settings.sanitize();
        store.save(&settings).unwrap();
        let (loaded, warning) = store.load();
        assert!(warning.is_none());
        assert_eq!(
            loaded.symbols,
            ["hk00700", "hk09988", "sh600519", "sz000001"]
        );
    }
}
