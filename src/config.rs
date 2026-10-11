//! dino-tui's own config, the only thing it keeps: `~/.config/dino-tui/config.toml`
//! (`$XDG_CONFIG_HOME/dino-tui` when that's set). Five options; `:set` changes them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    /// The key that, while attached, takes the next key as a command: `C-\` (Ctrl-\), as in dtach.
    pub prefix: String,
    /// A one-line bar at the bottom while attached; the session gets one row less.
    pub status_line: bool,
    /// `dd` asks y/N before deleting a session.
    pub confirm_delete: bool,
    /// The sidebar beside an attached session, when the terminal has room (100 columns or more).
    /// The prefix and `s` turns it off and on, and changes this.
    pub sidebar: bool,
    /// How the sidebar groups sessions: `project` (where they run) or `state` (what they're doing).
    pub group_by: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { prefix: "C-\\".into(), status_line: false, confirm_delete: true, sidebar: true, group_by: "project".into() }
    }
}

pub const OPTIONS: &[&str] = &["prefix", "status_line", "confirm_delete", "sidebar", "group_by"];

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let base = base.unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
        base.join("dino-tui/config.toml")
    }

    /// The config file, or the defaults when there's none. A file that doesn't parse is an error:
    /// silently ignoring it would change keys under the user.
    pub fn load() -> anyhow::Result<Config> {
        let path = Self::path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => anyhow::bail!("can't read {}: {e}", path.display()),
        };
        let config: Config = toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        Prefix::parse(&config.prefix).ok_or_else(|| anyhow::anyhow!("{}: prefix {:?} isn't a key dino-tui knows (try \"C-\\\\\" or \"C-a\")", path.display(), config.prefix))?;
        Ok(config)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }

    pub fn prefix(&self) -> Prefix {
        Prefix::parse(&self.prefix).unwrap_or_default()
    }

    pub fn by_state(&self) -> bool {
        self.group_by == "state"
    }

    /// `:set <option> <value>`: what it says back, or why not.
    pub fn set(&mut self, option: &str, value: &str) -> Result<String, String> {
        match option {
            "prefix" => {
                let p = Prefix::parse(value).ok_or_else(|| format!("{value:?} isn't a key dino-tui can use as the prefix: C-a … C-z, C-\\, C-], C-^ or C-_"))?;
                self.prefix = p.label.clone();
                Ok(format!("prefix is {}", p.label))
            }
            "status_line" | "confirm_delete" | "sidebar" => {
                let on = match value {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => return Err(format!("{option} is true or false")),
                };
                match option {
                    "status_line" => self.status_line = on,
                    "sidebar" => self.sidebar = on,
                    _ => self.confirm_delete = on,
                }
                Ok(format!("{option} is {on}"))
            }
            "group_by" => match value {
                "project" | "state" => {
                    self.group_by = value.into();
                    Ok(format!("the sidebar groups by {value}"))
                }
                _ => Err("group_by is project or state".into()),
            },
            _ => Err(format!("no option {option:?}: dino-tui has {}", OPTIONS.join(", "))),
        }
    }

    pub fn show(&self) -> String {
        format!("prefix={} status_line={} confirm_delete={} sidebar={} group_by={}", self.prefix, self.status_line, self.confirm_delete, self.sidebar, self.group_by)
    }
}

/// The prefix key: the byte a terminal sends for it, and the code point it reports for it with the
/// kitty keyboard protocol on (an agent may turn that on, and it reaches this terminal too).
#[derive(Debug, Clone, PartialEq)]
pub struct Prefix {
    pub byte: u8,
    /// The unshifted key's code point: `\` is 92.
    pub code: u32,
    /// As written in the config: `C-\`.
    pub label: String,
}

impl Default for Prefix {
    fn default() -> Self {
        Prefix { byte: 0x1c, code: '\\' as u32, label: "C-\\".into() }
    }
}

impl Prefix {
    /// `C-\`, `C-a`, `ctrl-a`, `^A`: Ctrl with a letter or one of `\ ] ^ _`. Not `C-[` (Escape),
    /// `C-m` (Return), `C-i` (Tab), `C-h` (Backspace) or `C-c`.
    pub fn parse(s: &str) -> Option<Prefix> {
        let lower = s.trim().to_lowercase();
        let key = lower.strip_prefix("c-").or_else(|| lower.strip_prefix("ctrl-")).or_else(|| lower.strip_prefix("ctrl+")).or_else(|| lower.strip_prefix('^'))?;
        let mut chars = key.chars();
        let (c, None) = (chars.next()?, chars.next()) else { return None };
        if !(c.is_ascii_lowercase() || matches!(c, '\\' | ']' | '^' | '_')) || matches!(c, 'm' | 'i' | 'h' | 'c' | 'j') {
            return None;
        }
        Some(Prefix { byte: (c as u8) & 0x1f, code: c as u32, label: format!("C-{c}") })
    }

    /// As a terminal shows the key: `^\`, `^A`.
    pub fn caret(&self) -> String {
        format!("^{}", char::from_u32(self.code).unwrap_or('?').to_ascii_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_parse() {
        assert_eq!(Prefix::parse("C-\\"), Some(Prefix::default()));
        let a = Prefix::parse("ctrl-A").unwrap();
        assert_eq!((a.byte, a.code, a.label.as_str()), (1, 'a' as u32, "C-a"));
        assert_eq!(Prefix::parse("^]").unwrap().byte, 0x1d);
        assert_eq!(Prefix::parse("C-["), None);
        assert_eq!(Prefix::parse("C-m"), None);
        assert_eq!(Prefix::parse("a"), None);
    }

    #[test]
    fn set_changes_one_option() {
        let mut c = Config::default();
        assert_eq!(c.set("status_line", "on"), Ok("status_line is true".into()));
        assert!(c.status_line);
        assert!(c.set("prefix", "C-[").is_err());
        assert_eq!(c.set("prefix", "C-a"), Ok("prefix is C-a".into()));
        assert!(c.set("theme", "dark").is_err());
        assert_eq!(c.set("sidebar", "off"), Ok("sidebar is false".into()));
        assert!(c.set("group_by", "colour").is_err());
        assert!(c.set("group_by", "state").is_ok() && c.by_state());
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}
