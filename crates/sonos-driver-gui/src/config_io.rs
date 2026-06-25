//! Persisting the [`audio_core::Config`] as `TOML` on disk.
//!
//! The GUI loads its configuration from a per-user `TOML` file at startup and
//! writes it back when the user saves the settings form. Serialisation reuses
//! the same `serde` representation as the CLI, so a file written by one tool is
//! read identically by the other.

use std::path::{Path, PathBuf};

use audio_core::{Config, Error, Result};

/// Serialise a [`Config`] to a pretty `TOML` document.
///
/// # Errors
///
/// Returns [`audio_core::Error::Config`] if serialisation fails (which should
/// only happen for a logically inconsistent value).
pub fn to_toml(config: &Config) -> Result<String> {
    toml::to_string_pretty(config)
        .map_err(|err| Error::config(format!("failed to serialise configuration: {err}")))
}

/// Parse a [`Config`] from a `TOML` document.
///
/// # Errors
///
/// Returns [`audio_core::Error::Config`] if the document is not valid `TOML` or
/// does not match the configuration schema.
pub fn from_toml(text: &str) -> Result<Config> {
    toml::from_str(text)
        .map_err(|err| Error::config(format!("failed to parse configuration: {err}")))
}

/// Load a [`Config`] from `path`.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read, or
/// [`audio_core::Error::Config`] if its contents are not valid configuration.
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)?;
    from_toml(&text)
}

/// Save `config` to `path`, creating parent directories as needed.
///
/// # Errors
///
/// Returns an I/O error if the directory or file cannot be written, or
/// [`audio_core::Error::Config`] if serialisation fails.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let text = to_toml(config)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// The default per-user configuration path.
///
/// On Windows this is `%APPDATA%\sonos-driver\sonos-driver.toml`; if `APPDATA`
/// is unset it falls back to a relative file in the working directory.
#[must_use]
pub fn default_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("APPDATA") {
        return PathBuf::from(dir)
            .join("sonos-driver")
            .join("sonos-driver.toml");
    }
    PathBuf::from("sonos-driver.toml")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{from_toml, load, save, to_toml};
    use audio_core::{CodecProfile, Config};

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("sonos-gui-{}-{n}-{tag}", std::process::id()))
    }

    #[test]
    fn toml_round_trips_through_strings() {
        let config = Config::default();
        let text = to_toml(&config).expect("serialise");
        assert_eq!(from_toml(&text).expect("parse"), config);
    }

    #[test]
    fn from_toml_rejects_garbage() {
        assert!(from_toml("this is = = not toml").is_err());
    }

    #[test]
    fn save_then_load_preserves_config_and_creates_dirs() {
        let mut config = Config::default();
        config.stream.profile = CodecProfile::Wav;
        config.stream.port = 9100;
        config.sonos.room = Some("Studio".to_owned());

        let dir = unique_temp_dir("roundtrip");
        // A nested path exercises the create_dir_all branch in `save`.
        let path = dir.join("nested").join("sonos-driver.toml");
        save(&path, &config).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded, config);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_file_is_an_error() {
        let path = unique_temp_dir("missing").join("absent.toml");
        assert!(load(&path).is_err());
    }
}
