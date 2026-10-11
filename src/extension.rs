//! The browser extension: the rule index it keeps, the files it is loaded from, and the
//! native messaging manifests that let Chromium browsers start `switchyard` as its host.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::pattern::Exported;
use crate::router::{self, Router};

/// The extension's id, fixed by the public key in its manifest.
pub const ID: &str = "alnhbemkcgmcacfclafeemnlbigaefbk";
/// The native messaging host name the extension connects to.
pub const HOST: &str = "dev.switchyard";

/// Chromium browsers the extension runs in: bundle id, and their folder under
/// `~/Library/Application Support`, where they read `NativeMessagingHosts`.
const BROWSERS: &[(&str, &str)] = &[
    ("com.google.Chrome", "Google/Chrome"),
    ("com.google.Chrome.beta", "Google/Chrome Beta"),
    ("com.google.Chrome.dev", "Google/Chrome Dev"),
    ("com.google.Chrome.canary", "Google/Chrome Canary"),
    ("org.chromium.Chromium", "Chromium"),
    ("com.microsoft.edgemac", "Microsoft Edge"),
    ("com.microsoft.edgemac.Beta", "Microsoft Edge Beta"),
    ("com.microsoft.edgemac.Dev", "Microsoft Edge Dev"),
    ("com.microsoft.edgemac.Canary", "Microsoft Edge Canary"),
    ("com.brave.Browser", "BraveSoftware/Brave-Browser"),
    ("com.brave.Browser.beta", "BraveSoftware/Brave-Browser-Beta"),
    ("com.brave.Browser.nightly", "BraveSoftware/Brave-Browser-Nightly"),
    ("com.vivaldi.Vivaldi", "Vivaldi"),
    ("company.thebrowser.Browser", "Arc/User Data"),
    ("company.thebrowser.dia", "Dia/User Data"),
    ("com.operasoftware.Opera", "com.operasoftware.Opera"),
    ("ai.perplexity.comet", "Comet"),
    ("net.imput.helium", "net.imput.helium"),
    ("org.chromium.Thorium", "Thorium"),
];

fn support_dir(bundle_id: &str) -> Option<PathBuf> {
    BROWSERS
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(bundle_id))
        .map(|(_, dir)| home().join("Library/Application Support").join(dir))
}

/// Whether the extension can be loaded into this browser.
pub fn supports(bundle_id: &str) -> bool {
    support_dir(bundle_id).is_some()
}

const FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("../extension/manifest.json")),
    ("match.js", include_bytes!("../extension/match.js")),
    ("background.js", include_bytes!("../extension/background.js")),
    ("content.js", include_bytes!("../extension/content.js")),
    ("popup.html", include_bytes!("../extension/popup.html")),
    ("popup.js", include_bytes!("../extension/popup.js")),
    ("handoff.html", include_bytes!("../extension/handoff.html")),
    ("handoff.js", include_bytes!("../extension/handoff.js")),
    ("style.css", include_bytes!("../extension/style.css")),
    ("welcome.html", include_bytes!("../extension/welcome.html")),
    ("icons/icon-16.png", include_bytes!("../extension/icons/icon-16.png")),
    ("icons/icon-32.png", include_bytes!("../extension/icons/icon-32.png")),
    ("icons/icon-48.png", include_bytes!("../extension/icons/icon-48.png")),
    ("icons/icon-128.png", include_bytes!("../extension/icons/icon-128.png")),
];

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    /// Whether a matching link leaves the browser. Rules that keep it are listed too, so
    /// they shadow the rules after them.
    pub leave: bool,
    #[serde(flatten)]
    pub pattern: Exported,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    pub browser: String,
    pub strip_tracking: bool,
    pub tracking: &'static [&'static str],
    pub rules: Vec<Entry>,
}

/// What the extension in `browser` needs to decide on its own which links to hand over.
pub fn index(config: &Config, self_id: Option<&str>, browser: &str) -> Index {
    let lookups = config::Lookups::default();
    let router = Router { config, lookups: &lookups, self_id };
    let rules = config
        .rules
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|rule| {
            let compiled = router::compile(rule).ok()?;
            Some(Entry {
                name: rule.name.clone(),
                leave: router.leaves(rule, browser),
                pattern: compiled.pattern.export(),
            })
        })
        .collect();
    Index {
        browser: browser.to_string(),
        strip_tracking: config.strip_tracking,
        tracking: router::TRACKING,
        rules,
    }
}

/// Where the unpacked extension lives, for "Load unpacked".
pub fn folder() -> PathBuf {
    config::dir().join("Extension")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    std::fs::write(path, contents).with_context(|| format!("could not write {}", path.display()))
}

fn write_files() -> Result<()> {
    let folder = folder();
    for (name, contents) in FILES {
        let path = folder.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&folder))?;
        write_if_changed(&path, contents)?;
    }
    Ok(())
}

/// The manifest that lets a browser start this binary as the extension's host, or `None`
/// for a development build, which would leave browsers pointing at a binary that
/// `cargo clean` removes.
fn host_manifest() -> Result<Option<Vec<u8>>> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if !exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        return Ok(None);
    }
    let manifest = serde_json::json!({
        "name": HOST,
        "description": "Switchyard link router",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{ID}/")],
    });
    Ok(Some(serde_json::to_vec_pretty(&manifest)?))
}

fn register(dir: &Path, manifest: &[u8]) -> Result<()> {
    let hosts = dir.join("NativeMessagingHosts");
    std::fs::create_dir_all(&hosts)?;
    write_if_changed(&hosts.join(format!("{HOST}.json")), manifest)
}

/// Writes the extension folder and registers this binary as its native messaging host in
/// every Chromium browser that has been run.
pub fn install() -> Result<()> {
    write_files()?;
    let Some(manifest) = host_manifest()? else {
        return Ok(());
    };
    for (_, dir) in BROWSERS {
        let dir = home().join("Library/Application Support").join(dir);
        if dir.is_dir() {
            register(&dir, &manifest)?;
        }
    }
    Ok(())
}

/// Gets one browser ready to load the extension, even one that has never been run, and
/// opens its Extensions page.
pub fn set_up(bundle_id: &str) -> Result<()> {
    write_files()?;
    let dir = support_dir(bundle_id).context("this browser can't run the extension")?;
    let manifest = host_manifest()?
        .context("Switchyard is running outside its app bundle; install it with scripts/bundle.sh")?;
    register(&dir, &manifest)?;
    // Every Chromium browser answers to chrome:// and shows its own Extensions page.
    crate::mac::open_with("chrome://extensions/", bundle_id)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn handoffs_path() -> PathBuf {
    config::dir().join("handoffs")
}

/// Remembers that the app sent `url` to a browser, so that browser's extension lets it
/// load instead of handing it straight back.
pub fn note_handoff(url: &str) {
    let path = handoffs_path();
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().collect();
    let line = format!("{} {url}", now());
    lines.push(&line);
    let keep = lines.len().saturating_sub(20);
    if let Err(e) = std::fs::write(&path, lines[keep..].join("\n")) {
        log::warn!("could not note a handoff: {e}");
    }
}

pub fn recently_handed(url: &str) -> bool {
    let text = std::fs::read_to_string(handoffs_path()).unwrap_or_default();
    let now = now();
    text.lines().any(|line| {
        line.split_once(' ').is_some_and(|(at, handed)| {
            handed == url && at.parse::<u64>().is_ok_and(|at| now.saturating_sub(at) <= 15)
        })
    })
}

/// A browser whose extension is, or was, connected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub browser: String,
    pub name: String,
    /// Unix seconds when the host last checked in.
    pub seen: u64,
    /// The extension's version, when it said.
    #[serde(default)]
    pub version: Option<String>,
}

impl Connection {
    pub fn live(&self) -> bool {
        now().saturating_sub(self.seen) <= 15
    }

    pub fn age(&self) -> Duration {
        Duration::from_secs(now().saturating_sub(self.seen))
    }
}

fn connections_dir() -> PathBuf {
    config::dir().join("connections")
}

pub fn save_connection(connection: &mut Connection) -> Result<()> {
    connection.seen = now();
    let dir = connections_dir();
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.json", connection.browser));
    std::fs::write(file, serde_json::to_vec(connection)?)?;
    Ok(())
}

pub fn connections() -> Vec<Connection> {
    let Ok(entries) = std::fs::read_dir(connections_dir()) else {
        return Vec::new();
    };
    let mut list: Vec<Connection> = entries
        .flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Lookups, Rule};
    use crate::router::{Outcome, Target};

    fn config() -> Config {
        let mut config = Config {
            browser: Some("dia".into()),
            ..Config::default()
        };
        for rule in &mut config.rules {
            rule.enabled = true;
        }
        config.rules.extend([
            Rule::new("bb", "https://bb.example.net", Some("bb-desktop://open?url=(encode url)")),
            Rule {
                open_in: Some("arc".into()),
                ..Rule::new("docs in Arc", "**.docs.example.com/(PAGE...)", None)
            },
            Rule::new("stay", "Example.org/keep/*", None),
            Rule::new("odd", "example.org/k(EEP)/**#(F)", Some("x-odd:(EEP)")),
            Rule::new("query", "example.org/q?a=(A)&b", Some("x-q:(A)")),
            Rule::new("mail", "mailto:(ADDR)", Some("x-mail:(ADDR)")),
            Rule::new("port", "localhost:3000/(P)", Some("x-port:(P)")),
        ]);
        config
    }

    const URLS: &[&str] = &[
        "https://acme.slack.com/archives/C9/p1791623821542079?thread_ts=1791601801.938149&cid=C9",
        "https://acme.slack.com/archives/C9/p1791623821542079",
        "https://acme.slack.com/archives/C9",
        "https://ACME.Slack.com/archives/C9/",
        "https://acme.slack.com/messages",
        "https://us02web.zoom.us/j/123?pwd=x",
        "https://open.spotify.com/track/abc",
        "https://bb.example.net/a/b?c=d",
        "http://bb.example.net/a",
        "https://bb.example.net:8443/",
        "https://a.b.docs.example.com/x/y",
        "https://docs.example.com/",
        "https://example.org/keep/this",
        "https://example.org/keep/this/too",
        "https://example.org/keep/x#top",
        "https://example.org/q?b&a=1",
        "https://example.org/q?a=1&utm_source=b",
        "https://example.org/q?a=1",
        "mailto:a@b.c",
        "http://localhost:3000/x",
        "http://localhost:4000/x",
        "https://example.com/caf%C3%A9",
    ];

    /// What the extension in `browser` should do with `url`, according to the router.
    fn expected(config: &Config, browser: &str, url: &str) -> Option<bool> {
        let mut lookups = Lookups::default();
        lookups.set("SlackTeamIds", "acme", "T1");
        let router = Router { config, lookups: &lookups, self_id: Some("me") };
        match router.route(url, false) {
            Outcome::Open { rule: None, .. } => None,
            Outcome::Open { target, .. } => Some(target != Target::App(browser.into())),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn manifest_version_matches_the_app() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../extension/manifest.json")).unwrap();
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    }

    /// Runs `extension/match.js` under Node on the index and checks it agrees with the router.
    #[test]
    fn javascript_agrees_with_the_router() {
        let node = std::process::Command::new("node").arg("--version").output();
        if !node.is_ok_and(|o| o.status.success()) {
            eprintln!("skipping: node is not installed");
            return;
        }
        let config = config();
        for browser in ["dia", "arc"] {
            let index = serde_json::to_string(&index(&config, Some("me"), browser)).unwrap();
            let script = format!(
                "const m = require({:?});\
                 const index = {index};\
                 const urls = {};\
                 console.log(JSON.stringify(urls.map(u => {{ const r = m.decide(index, u); return r && r.leave; }})));",
                concat!(env!("CARGO_MANIFEST_DIR"), "/extension/match.js"),
                serde_json::to_string(URLS).unwrap(),
            );
            let out = std::process::Command::new("node").args(["-e", &script]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let got: Vec<Option<bool>> = serde_json::from_slice(&out.stdout).unwrap();
            let want: Vec<Option<bool>> = URLS.iter().map(|u| expected(&config, browser, u)).collect();
            for want_one in [None, Some(true), Some(false)] {
                assert!(want.contains(&want_one), "the samples should include {want_one:?}");
            }
            for ((url, got), want) in URLS.iter().zip(got).zip(want) {
                assert_eq!(got, want, "{browser}: {url}");
            }
        }
    }
}
