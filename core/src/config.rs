use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::volumes::{VolumeInfo, VolumeKind};
use crate::Mode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRef {
    pub mount: String,
    pub kind: VolumeKind,
    #[serde(default)]
    pub serial: Option<String>,
}

impl VolumeRef {
    pub fn of(info: &VolumeInfo) -> Self {
        VolumeRef { mount: info.mount.clone(), kind: info.kind, serial: info.serial.clone() }
    }

    // Make sure that the drive is the same drive.
    pub fn matches(&self, now: &VolumeInfo) -> bool {
        if let (Some(a), Some(b)) = (&self.serial, &now.serial) {
            return a == b;
        }
        self.kind == VolumeKind::Internal || self.mount == now.mount
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PairConfig {
    pub id: String,
    pub name: String,
    pub source: String,
    pub copy: String,
    pub mode: Mode,

    pub keep_history: bool,
    pub history_days: u32,
    pub history_max_gb: u32,

    pub on_connect: bool,
    pub on_change: bool,
    pub every_minutes: u32,
    pub paused: bool,

    pub excludes: Vec<String>,
    pub skip_hidden: bool,
    pub verify: bool,
    pub check_contents: bool,
    pub detect_moves: bool,
    pub guard_percent: u8,
    pub eject_after: bool,

    pub source_volume: Option<VolumeRef>,
    pub copy_volume: Option<VolumeRef>,
}

impl Default for PairConfig {
    fn default() -> Self {
        PairConfig {
            id: String::new(),
            name: String::new(),
            source: String::new(),
            copy: String::new(),
            mode: Mode::Mirror,
            keep_history: true,
            history_days: 30,
            history_max_gb: 0,
            on_connect: true,
            on_change: false,
            every_minutes: 0,
            paused: false,
            excludes: Vec::new(),
            skip_hidden: false,
            verify: false,
            check_contents: true,
            detect_moves: true,
            guard_percent: 30,
            eject_after: false,
            source_volume: None,
            copy_volume: None,
        }
    }
}

impl PairConfig {
    pub fn automatic(&self) -> bool {
        !self.paused && (self.on_connect || self.on_change || self.every_minutes > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub launch_at_login: bool,
    pub keep_in_tray: bool,
    pub notifications: bool,
    pub auto_paused: bool,
    pub sounds: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings { launch_at_login: false, keep_in_tray: true, notifications: true, auto_paused: false, sounds: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub pairs: Vec<PairConfig>,
    pub settings: AppSettings,
}

impl Config {
    pub fn load(path: &Path) -> Config {
        match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Config>(&bytes) {
                Ok(c) => c,
                Err(_) => {
                    let _ = fs::rename(path, path.with_extension("unreadable.json"));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json(path, self)
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = crate::now_ms() as u64;
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:02x}", t, (n ^ std::process::id()) & 0xff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(mount: &str, kind: VolumeKind, serial: Option<&str>) -> VolumeInfo {
        VolumeInfo { mount: mount.into(), name: "x".into(), kind, fs: "x".into(), server: None, serial: serial.map(str::to_owned), total: 1, free: 1 }
    }

    #[test]
    fn a_drive_is_recognized_by_serial_where_there_is_one() {
        let was = VolumeRef::of(&vol("E:\\", VolumeKind::Internal, Some("1A2B3C4D")));
        assert!(was.matches(&vol("E:\\", VolumeKind::Internal, Some("1A2B3C4D"))));
        assert!(!was.matches(&vol("E:\\", VolumeKind::Internal, Some("99999999"))));
    }

    #[test]
    fn without_serials_removable_drives_must_be_where_they_were() {
        let stick = VolumeRef::of(&vol("/Volumes/LaCie", VolumeKind::External, None));
        assert!(stick.matches(&vol("/Volumes/LaCie", VolumeKind::External, None)));
        assert!(!stick.matches(&vol("/", VolumeKind::Internal, None)));
        assert!(!stick.matches(&vol("/Volumes/LaCie 1", VolumeKind::External, None)));
        let built_in = VolumeRef::of(&vol("/System/Volumes/Data", VolumeKind::Internal, None));
        assert!(built_in.matches(&vol("/", VolumeKind::Internal, None)));
    }

    #[test]
    fn settings_written_by_an_older_version_still_load() {
        let old = r#"{"pairs":[{"id":"a","name":"A","source":"/s","copy":"/c"}],"settings":{"keep_in_tray":false}}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert_eq!(c.pairs[0].mode, Mode::Mirror);
        assert!(c.pairs[0].keep_history && c.pairs[0].history_days == 30);
        assert!(!c.settings.keep_in_tray && c.settings.sounds && c.settings.notifications);
    }
}
