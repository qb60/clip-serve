use std::{
    fs::File,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddrV4},
    path::Path,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const MAX_IMAGES: usize = 1000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen_address: Ipv4Addr,
    pub port: u16,
    pub max_images: usize,
    pub capture_on_startup: bool,
    pub autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: Ipv4Addr::LOCALHOST,
            port: 8888,
            max_images: 10,
            capture_on_startup: false,
            autostart: false,
        }
    }
}

impl Config {
    pub fn validate(self) -> io::Result<Self> {
        let message = if self.listen_address.is_unspecified()
            || self.listen_address.is_multicast()
            || self.listen_address.is_broadcast()
        {
            "Enter a specific local IPv4 address."
        } else if self.port == 0 {
            "Port must be between 1 and 65535."
        } else if !(1..=MAX_IMAGES).contains(&self.max_images) {
            "Maximum images must be between 1 and 1000."
        } else {
            return Ok(self);
        };
        Err(io::Error::new(io::ErrorKind::InvalidInput, message))
    }

    pub fn endpoint(self) -> SocketAddrV4 {
        SocketAddrV4::new(self.listen_address, self.port)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match File::open(path) {
            Ok(file) => serde_json::from_reader::<_, Self>(file)?.validate(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let defaults = Self::default();
                defaults.save(path)?;
                Ok(defaults)
            }
            Err(err) => Err(err),
        }
    }

    pub fn stage(self, path: &Path) -> io::Result<NamedTempFile> {
        self.validate()?;
        let mut pending = NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
        serde_json::to_writer_pretty(&mut pending, &self)?;
        pending.write_all(b"\n")?;
        pending.as_file().sync_all()?;
        Ok(pending)
    }

    pub fn save(self, path: &Path) -> io::Result<()> {
        self.stage(path)?.persist(path)?;
        Ok(())
    }
}

pub fn config_path(exe: &Path) -> std::path::PathBuf {
    exe.with_file_name("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_and_round_trip_with_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(&dir.path().join("clipserve"));
        let defaults = Config::load(&path).unwrap();
        assert_eq!(defaults.endpoint().to_string(), "127.0.0.1:8888");
        assert_eq!(defaults.max_images, 10);
        assert!(!defaults.capture_on_startup && !defaults.autostart);
        let changed = Config {
            listen_address: "192.168.56.1".parse().unwrap(),
            port: 65535,
            max_images: MAX_IMAGES,
            capture_on_startup: true,
            autostart: true,
        };
        let pending = changed.stage(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), defaults);
        pending.persist(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), changed);
        let saved: serde_json::Value = serde_json::from_reader(File::open(path).unwrap()).unwrap();
        assert_eq!(saved["autostart"], true);
    }

    #[test]
    fn rejects_invalid_schema_and_values_without_replacing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        for json in [
            "{}",
            "{not JSON}",
            r#"{"listen_address":"0.0.0.0","port":8888,"max_images":10,"capture_on_startup":false,"autostart":false}"#,
            r#"{"listen_address":"127.0.0.1","port":65536,"max_images":10,"capture_on_startup":false,"autostart":false}"#,
        ] {
            fs::write(&path, json).unwrap();
            assert!(Config::load(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), json);
        }
        assert!(
            Config {
                port: 0,
                ..Config::default()
            }
            .validate()
            .is_err()
        );
        for max_images in [0, MAX_IMAGES + 1] {
            assert!(
                Config {
                    max_images,
                    ..Config::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
