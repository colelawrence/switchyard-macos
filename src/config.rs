//! Rules and lookup tables, stored as TOML in `~/Library/Application Support/Switchyard`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    None,
    #[default]
    Option,
    Shift,
    Control,
    Command,
}

impl Modifier {
    pub const ALL: [Modifier; 5] = [
        Modifier::None,
        Modifier::Option,
        Modifier::Shift,
        Modifier::Control,
        Modifier::Command,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Modifier::None => "Off",
            Modifier::Option => "⌥ Option",
            Modifier::Shift => "⇧ Shift",
            Modifier::Control => "⌃ Control",
            Modifier::Command => "⌘ Command",
        }
    }
}

/// Where a rewritten link opens.
pub const OPEN_ASK: &str = "ask";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(rename = "match")]
    pub pattern: String,
    /// The template the link is rewritten to. Without one the link is opened as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
    /// A bundle id, or `ask` for the picker. Without one, the system opens the
    /// rewritten URL with whatever app handles its scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_in: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Rule {
    pub fn new(name: &str, pattern: &str, rewrite: Option<&str>) -> Self {
        Self {
            name: name.into(),
            pattern: pattern.into(),
            rewrite: rewrite.map(Into::into),
            open_in: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Bundle id of the browser that opens links no rule claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Holding this while clicking a link shows the browser picker.
    #[serde(default)]
    pub picker_modifier: Modifier,
    /// Removes `utm_*`, `fbclid`, `gclid` and similar parameters before opening.
    #[serde(default = "yes")]
    pub strip_tracking: bool,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Default for Config {
    fn default() -> Self {
        let mut zoom = Rule::new(
            "Zoom meetings in the Zoom app",
            "**.zoom.us/j/(MEETING)",
            Some("zoommtg://zoom.us/join?confno=(MEETING)&pwd=(query \"pwd\")"),
        );
        zoom.enabled = false;
        let mut spotify = Rule::new(
            "Spotify in the Spotify app",
            "open.spotify.com/(KIND)/(ID)",
            Some("spotify:(KIND):(ID)"),
        );
        spotify.enabled = false;
        Self {
            browser: None,
            picker_modifier: Modifier::default(),
            strip_tracking: true,
            rules: vec![
                // Rules are tried top to bottom, so the most specific Slack link goes first.
                // The `message` parameter takes the permalink's `p1700000000123456` as-is.
                Rule::new(
                    "Slack thread replies in the Slack app",
                    "(SUBDOMAIN).slack.com/archives/(CHANNEL)/(MESSAGE)?thread_ts=(THREAD)",
                    Some(
                        "slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)\
                         &message=(MESSAGE)&thread_ts=(THREAD)",
                    ),
                ),
                Rule::new(
                    "Slack messages in the Slack app",
                    "(SUBDOMAIN).slack.com/archives/(CHANNEL)/(MESSAGE)",
                    Some(
                        "slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)\
                         &message=(MESSAGE)",
                    ),
                ),
                Rule::new(
                    "Slack channels in the Slack app",
                    "(SUBDOMAIN).slack.com/archives/(CHANNEL)/**",
                    Some("slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)"),
                ),
                zoom,
                spotify,
            ],
        }
    }
}

/// Named tables of key → value, filled in by the user the first time a lookup misses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lookups(pub BTreeMap<String, BTreeMap<String, String>>);

impl Lookups {
    pub fn get(&self, table: &str, key: &str) -> Option<String> {
        self.0.get(table)?.get(key).cloned()
    }

    /// Stores a value, creating the table if needed.
    pub fn set(&mut self, table: &str, key: &str, value: &str) {
        self.0
            .entry(table.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }

    pub fn remove(&mut self, table: &str, key: &str) {
        if let Some(entries) = self.0.get_mut(table) {
            entries.remove(key);
        }
    }

    pub fn remove_table(&mut self, table: &str) {
        self.0.remove(table);
    }
}

pub fn dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SWITCHYARD_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/Switchyard")
}

pub fn config_path() -> PathBuf {
    dir().join("config.toml")
}

pub fn lookups_path() -> PathBuf {
    dir().join("lookups.toml")
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("{} is not valid", path.display()))
            .map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(value)?;
    // Write then rename, so a crash never leaves a half-written file behind.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).with_context(|| format!("could not write {}", path.display()))
}

impl Config {
    /// Loads the config. The bool is true when the file did not exist yet.
    pub fn load() -> Result<(Self, bool)> {
        Ok(match read(&config_path())? {
            Some(config) => (config, false),
            None => (Self::default(), true),
        })
    }

    pub fn save(&self) -> Result<()> {
        write(&config_path(), self)
    }
}

impl Lookups {
    pub fn load() -> Result<Self> {
        Ok(read(&lookups_path())?.unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        write(&lookups_path(), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn minimal_config() {
        let config: Config = toml::from_str(
            r#"
            [[rules]]
            name = "x"
            match = "example.com"
            "#,
        )
        .unwrap();
        assert!(config.strip_tracking);
        assert!(config.rules[0].enabled);
        assert_eq!(config.picker_modifier, Modifier::Option);
    }

    #[test]
    fn lookups_create_tables() {
        let mut lookups = Lookups::default();
        lookups.set("SlackTeamIds", "acme", "T1");
        let text = toml::to_string_pretty(&lookups).unwrap();
        assert!(text.contains("[SlackTeamIds]"), "{text}");
        assert_eq!(toml::from_str::<Lookups>(&text).unwrap(), lookups);
        assert_eq!(lookups.get("SlackTeamIds", "acme").as_deref(), Some("T1"));
    }
}
