//! The machines this client has connected to, in the order they were first
//! connected, kept in `$XDG_CONFIG_HOME/mac-gliff/config.toml` so the header
//! offer them again as tabs. It also keeps which of them were connected and
//! which was shown, so a reopened window connects to them again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a machine's remote screen maps onto the client's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    /// Mirror the remote's own screen at its own resolution.
    Mirror,
    /// A private headless remote output, resized to follow this window.
    Window,
}

/// A machine's remembered screen-mapping choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplaySetting {
    pub mode: DisplayMode,
    /// Skip the connect-time choice and use `mode` again.
    pub remember: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default, alias = "recent_machines")]
    pub machines: Vec<String>,
    /// The machines with a running session, in tab order of opening.
    #[serde(default)]
    pub open: Vec<String>,
    /// The machine the window showed last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<String>,
    /// Each machine's screen-mapping choice, asked for at connect; an
    /// entry with `remember` set is used without asking again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub display: BTreeMap<String, DisplaySetting>,
    /// The header's sound button is off: no tab plays its machine's sound.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

impl Config {
    /// Where the client keeps its config: `~/.config/mac-gliff/config.toml`.
    pub fn default_path() -> PathBuf {
        gtk4::glib::user_config_dir()
            .join("mac-gliff")
            .join("config.toml")
    }

    /// Read the config, or start empty when there is none yet. A file that
    /// cannot be parsed is logged and treated as empty, so a hand edit gone
    /// wrong never stops the client from starting.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read config");
                return Self::default();
            }
        };
        match Self::parse(&text) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot parse config");
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let mut config: Self = toml::from_str(text)?;
        config.machines.retain(|m| !m.trim().is_empty());
        let mut seen = std::collections::HashSet::new();
        config.machines.retain(|m| seen.insert(m.clone()));
        // Only remembered machines can be reopened.
        let machines = config.machines.clone();
        config.open.retain(|m| machines.contains(m));
        // A choice only outlives its machine while the machine does.
        config.display.retain(|m, _| machines.contains(m));
        let mut seen = std::collections::HashSet::new();
        config.open.retain(|m| seen.insert(m.clone()));
        Ok(config)
    }

    /// Add `machine` at the end of the list unless it is already there, so
    /// the tabs keep their places as machines are reconnected.
    pub fn remember(&mut self, machine: &str) {
        let machine = machine.trim();
        if machine.is_empty() || self.machines.iter().any(|m| m == machine) {
            return;
        }
        self.machines.push(machine.to_string());
    }

    pub fn forget(&mut self, machine: &str) {
        self.machines.retain(|m| m != machine);
        self.set_open(machine, false);
        self.display.remove(machine);
        if self.shown.as_deref() == Some(machine) {
            self.shown = None;
        }
    }

    /// Record whether `machine` has a running session, to reconnect it when
    /// the window opens again.
    pub fn set_open(&mut self, machine: &str, open: bool) {
        let listed = self.open.iter().any(|m| m == machine);
        if open && !listed {
            self.open.push(machine.to_string());
        } else if !open {
            self.open.retain(|m| m != machine);
        }
    }

    /// Move `machine` to just before `target`, or just after it with
    /// `after`. False when either is not in the list, or they are the same.
    pub fn move_machine(&mut self, machine: &str, target: &str, after: bool) -> bool {
        if machine == target || !self.machines.iter().any(|m| m == target) {
            return false;
        }
        let Some(from) = self.machines.iter().position(|m| m == machine) else {
            return false;
        };
        let moved = self.machines.remove(from);
        let to = self.machines.iter().position(|m| m == target).unwrap() + usize::from(after);
        self.machines.insert(to, moved);
        true
    }

    /// The machine's screen-mapping choice, if it has one.
    pub fn display_setting(&self, machine: &str) -> Option<DisplaySetting> {
        self.display.get(machine).copied()
    }

    /// Record the machine's screen-mapping choice. It is kept even before
    /// the machine is remembered, so a first connect that never succeeds
    /// loses the choice with the machine; `parse` drops such entries.
    pub fn set_display_setting(&mut self, machine: &str, setting: DisplaySetting) {
        self.display.insert(machine.to_string(), setting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_appends_new_machines_once() {
        let mut c = Config::default();
        c.remember("a");
        c.remember("b");
        c.remember("a");
        assert_eq!(c.machines, vec!["a", "b"]);
    }

    #[test]
    fn remember_trims_and_ignores_blank() {
        let mut c = Config::default();
        c.remember("  ");
        c.remember(" user@host ");
        assert_eq!(c.machines, vec!["user@host"]);
    }

    #[test]
    fn forget_removes_the_machine() {
        let mut c = Config::default();
        c.remember("a");
        c.remember("b");
        c.forget("a");
        c.forget("missing");
        assert_eq!(c.machines, vec!["b"]);
    }

    #[test]
    fn set_open_records_each_machine_once() {
        let mut c = Config::default();
        c.set_open("a", true);
        c.set_open("b", true);
        c.set_open("a", true);
        assert_eq!(c.open, vec!["a", "b"]);
        c.set_open("a", false);
        assert_eq!(c.open, vec!["b"]);
    }

    #[test]
    fn forget_closes_and_unshows_the_machine() {
        let mut c = Config::default();
        c.remember("a");
        c.set_open("a", true);
        c.shown = Some("a".into());
        c.forget("a");
        assert!(c.machines.is_empty() && c.open.is_empty());
        assert_eq!(c.shown, None);
    }

    #[test]
    fn parse_keeps_only_remembered_open_machines() {
        let c =
            Config::parse("machines = [\"a\", \"b\"]\nopen = [\"b\", \"x\", \"b\"]\nshown = \"b\"")
                .unwrap();
        assert_eq!(c.open, vec!["b"]);
        assert_eq!(c.shown.as_deref(), Some("b"));
    }

    #[test]
    fn move_machine_places_before_or_after_the_target() {
        let mut c = Config::default();
        for m in ["a", "b", "c", "d"] {
            c.remember(m);
        }
        assert!(c.move_machine("d", "b", false));
        assert_eq!(c.machines, vec!["a", "d", "b", "c"]);
        assert!(c.move_machine("a", "c", true));
        assert_eq!(c.machines, vec!["d", "b", "c", "a"]);
        assert!(c.move_machine("a", "d", false));
        assert_eq!(c.machines, vec!["a", "d", "b", "c"]);
    }

    #[test]
    fn move_machine_ignores_unknown_and_self() {
        let mut c = Config::default();
        c.remember("a");
        c.remember("b");
        assert!(!c.move_machine("a", "a", true));
        assert!(!c.move_machine("x", "a", false));
        assert!(!c.move_machine("a", "x", false));
        assert_eq!(c.machines, vec!["a", "b"]);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut c = Config::default();
        c.remember("b");
        c.remember("a");
        c.set_open("a", true);
        c.shown = Some("a".into());
        let text = toml::to_string(&c).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), c);
    }

    #[test]
    fn parse_tolerates_missing_key_and_drops_blanks_and_duplicates() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        let c = Config::parse("machines = [\"a\", \"\", \"b\", \"a\"]").unwrap();
        assert_eq!(c.machines, vec!["a", "b"]);
        assert!(Config::parse("machines = 3").is_err());
    }

    #[test]
    fn parse_reads_the_old_recent_machines_key() {
        let c = Config::parse("recent_machines = [\"a\", \"b\"]").unwrap();
        assert_eq!(c.machines, vec!["a", "b"]);
    }

    #[test]
    fn load_and_save_files() {
        let dir = std::env::temp_dir().join(format!("gliff-recent-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        assert_eq!(Config::load(&path), Config::default());

        let mut c = Config::default();
        c.remember("user@host");
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);

        std::fs::write(&path, "not toml [").unwrap();
        assert_eq!(Config::load(&path), Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn display_setting_round_trips_through_toml() {
        let mut c = Config::default();
        c.remember("user@host");
        c.set_display_setting(
            "user@host",
            DisplaySetting {
                mode: DisplayMode::Window,
                remember: true,
            },
        );
        let text = toml::to_string(&c).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), c);
        assert_eq!(
            c.display_setting("user@host"),
            Some(DisplaySetting {
                mode: DisplayMode::Window,
                remember: true,
            })
        );
        assert_eq!(c.display_setting("other"), None);
    }

    #[test]
    fn forget_and_parse_drop_the_display_setting() {
        let mut c = Config::default();
        c.remember("a");
        c.set_display_setting(
            "a",
            DisplaySetting {
                mode: DisplayMode::Mirror,
                remember: true,
            },
        );
        c.forget("a");
        assert_eq!(c.display_setting("a"), None);

        let mut c = Config::default();
        c.remember("a");
        c.set_display_setting(
            "gone",
            DisplaySetting {
                mode: DisplayMode::Mirror,
                remember: true,
            },
        );
        let text = toml::to_string(&c).unwrap();
        assert_eq!(Config::parse(&text).unwrap().display_setting("gone"), None);
    }
}
