//! Decides what happens to a clicked link.

use url::Url;

use crate::config::{Config, Lookups, OPEN_ASK, Rule};
use crate::pattern::Pattern;
use crate::template::{EvalError, Scope, Template};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A specific app, by bundle id.
    App(String),
    /// Whatever app the system has registered for the URL's scheme.
    SchemeDefault,
    /// Show the browser picker.
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Open {
        url: String,
        target: Target,
        /// The rule that claimed the link, if any.
        rule: Option<String>,
        /// Something the user should know about, such as a broken rule that was skipped.
        note: Option<String>,
    },
    /// A lookup has no entry yet. Ask the user, store the answer and route again.
    NeedValue {
        table: String,
        key: String,
        rule: String,
        url: String,
    },
    /// The input was not a URL at all.
    Invalid(String),
}

pub const TRACKING: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid", "igshid",
    "yclid", "_hsenc", "_hsmi", "mkt_tok", "oly_anon_id", "oly_enc_id", "vero_id",
];

/// Removes tracking parameters from http(s) URLs.
pub fn strip_tracking(url: &mut Url) {
    if !matches!(url.scheme(), "http" | "https") || url.query().is_none() {
        return;
    }
    let query = url.query().unwrap_or("");
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or("");
            !(key.starts_with("utm_") || TRACKING.contains(&key))
        })
        .collect();
    if kept.len() != query.split('&').count() {
        let kept = kept.join("&");
        url.set_query((!kept.is_empty()).then_some(kept.as_str()));
    }
}

pub struct Compiled<'a> {
    #[allow(dead_code)]
    pub rule: &'a Rule,
    pub pattern: Pattern,
    pub rewrite: Option<Template>,
}

/// Parses a rule and checks that its rewrite only uses names the pattern captures.
pub fn compile(rule: &Rule) -> Result<Compiled<'_>, String> {
    let pattern = Pattern::parse(&rule.pattern).map_err(|e| format!("Match: {e}"))?;
    let rewrite = match rule.rewrite.as_deref().map(str::trim) {
        Some(src) if !src.is_empty() => {
            let template = Template::parse(src).map_err(|e| format!("Rewrite: {e}"))?;
            template
                .check(pattern.names())
                .map_err(|e| format!("Rewrite: {e}"))?;
            Some(template)
        }
        _ => None,
    };
    Ok(Compiled {
        rule,
        pattern,
        rewrite,
    })
}

fn is_web(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

pub struct Router<'a> {
    pub config: &'a Config,
    pub lookups: &'a Lookups,
    /// Our own bundle id. Handing a web link to it, directly or through the
    /// system default, would loop forever.
    pub self_id: Option<&'a str>,
}

impl Router<'_> {
    fn browser(&self) -> Target {
        match self.config.browser.as_deref() {
            Some(id) if Some(id) != self.self_id && !id.is_empty() => Target::App(id.to_string()),
            _ => Target::Ask,
        }
    }

    fn target(&self, url: &str, open_in: Option<&str>) -> Target {
        match open_in {
            Some(OPEN_ASK) => Target::Ask,
            Some(id) if !id.is_empty() && Some(id) != self.self_id => Target::App(id.to_string()),
            _ if is_web(url) => self.browser(),
            _ => Target::SchemeDefault,
        }
    }

    /// Whether a link this rule matches, clicked in `browser`, should leave it. Rewrites that
    /// start with an expression could produce any scheme, so they leave to be safe; the app
    /// sends them back if they turn out to belong here.
    pub fn leaves(&self, rule: &Rule, browser: &str) -> bool {
        let rewrite = rule.rewrite.as_deref().map(str::trim).filter(|r| !r.is_empty());
        let out = match rewrite {
            None => "https://".to_string(),
            Some(src) => {
                let end = src
                    .find(|c: char| !(c.is_ascii_alphanumeric() || "+.-".contains(c)))
                    .unwrap_or(src.len());
                if end == 0 || !src[end..].starts_with(':') {
                    return true;
                }
                src[..end + 1].to_ascii_lowercase() + "//"
            }
        };
        self.target(&out, rule.open_in.as_deref()) != Target::App(browser.to_string())
    }

    /// Where a web link goes when no rule applies.
    pub fn route_unruled(&self, url: &str) -> Target {
        self.target(url, None)
    }

    pub fn route(&self, input: &str, picker_held: bool) -> Outcome {
        let mut url = match Url::parse(input.trim()) {
            Ok(url) => url,
            Err(e) => return Outcome::Invalid(format!("`{input}` is not a URL: {e}")),
        };
        if self.config.strip_tracking {
            strip_tracking(&mut url);
        }
        if picker_held {
            return Outcome::Open {
                url: url.to_string(),
                target: Target::Ask,
                rule: None,
                note: None,
            };
        }
        let mut notes = Vec::new();
        for rule in self.config.rules.iter().filter(|r| r.enabled) {
            let compiled = match compile(rule) {
                Ok(c) => c,
                Err(e) => {
                    notes.push(format!("Skipped “{}”: {e}", rule.name));
                    continue;
                }
            };
            let Some(captures) = compiled.pattern.matches(&url) else {
                continue;
            };
            let rewritten = match &compiled.rewrite {
                None => Ok(url.to_string()),
                Some(template) => template.render(&Scope {
                    url: &url,
                    captures: &captures,
                    lookup: &|t, k| self.lookups.get(t, k),
                }),
            };
            match rewritten {
                Ok(out) => {
                    return Outcome::Open {
                        target: self.target(&out, rule.open_in.as_deref()),
                        url: out,
                        rule: Some(rule.name.clone()),
                        note: (!notes.is_empty()).then(|| notes.join("\n")),
                    };
                }
                Err(EvalError::Missing { table, key }) => {
                    return Outcome::NeedValue {
                        table,
                        key,
                        rule: rule.name.clone(),
                        url: url.to_string(),
                    };
                }
                Err(e) => notes.push(format!("Skipped “{}”: {e}", rule.name)),
            }
        }
        Outcome::Open {
            target: self.browser(),
            url: url.to_string(),
            rule: None,
            note: (!notes.is_empty()).then(|| notes.join("\n")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLACK_URL: &str = "https://acme.slack.com/archives/C0123ABCDEF";

    fn config() -> Config {
        Config {
            browser: Some("com.apple.Safari".into()),
            rules: vec![Rule::new(
                "Slack",
                "(SUBDOMAIN).slack.com/archives/(CHANNEL)",
                Some("slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)"),
            )],
            ..Config::default()
        }
    }

    #[test]
    fn slack_end_to_end() {
        let config = config();
        let mut lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        assert_eq!(
            router.route(SLACK_URL, false),
            Outcome::NeedValue {
                table: "SlackTeamIds".into(),
                key: "acme".into(),
                rule: "Slack".into(),
                url: SLACK_URL.into(),
            }
        );
        lookups.set("SlackTeamIds", "acme", "T0123ABCD");
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        assert_eq!(
            router.route(SLACK_URL, false),
            Outcome::Open {
                url: "slack://channel?team=T0123ABCD&id=C0123ABCDEF".into(),
                target: Target::SchemeDefault,
                rule: Some("Slack".into()),
                note: None,
            }
        );
    }

    #[test]
    fn unmatched_goes_to_browser_without_tracking() {
        let config = config();
        let lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        assert_eq!(
            router.route("https://example.com/?utm_source=x&a=1&fbclid=y", false),
            Outcome::Open {
                url: "https://example.com/?a=1".into(),
                target: Target::App("com.apple.Safari".into()),
                rule: None,
                note: None,
            }
        );
    }

    #[test]
    fn modifier_shows_picker() {
        let config = config();
        let lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        assert!(matches!(
            router.route(SLACK_URL, true),
            Outcome::Open { target: Target::Ask, .. }
        ));
    }

    #[test]
    fn never_loops_back_to_self() {
        let mut config = config();
        config.browser = Some("dev.switchyard".into());
        config.rules.push(Rule::new("same", "example.com", None));
        let lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: Some("dev.switchyard") };
        assert!(matches!(
            router.route("https://example.com/", false),
            Outcome::Open { target: Target::Ask, .. }
        ));
    }

    #[test]
    fn broken_rules_are_skipped_with_a_note() {
        let mut config = config();
        config.rules.insert(0, Rule::new("bad", "(oops", None));
        let lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        let Outcome::Open { note, .. } = router.route("https://example.com/", false) else {
            panic!()
        };
        assert!(note.unwrap().contains("bad"));
    }

    #[test]
    fn default_slack_rules() {
        let config = Config::default();
        let mut lookups = Lookups::default();
        lookups.set("SlackTeamIds", "acme", "T1");
        let router = Router { config: &config, lookups: &lookups, self_id: None };
        let open = |url: &str| match router.route(url, false) {
            Outcome::Open { url, .. } => url,
            other => panic!("{other:?}"),
        };
        let base = "https://acme.slack.com/archives/C9";
        assert_eq!(
            open(&format!("{base}/p1791623821542079?thread_ts=1791601801.938149&cid=C9")),
            "slack://channel?team=T1&id=C9&message=p1791623821542079&thread_ts=1791601801.938149"
        );
        assert_eq!(
            open(&format!("{base}/p1791623821542079")),
            "slack://channel?team=T1&id=C9&message=p1791623821542079"
        );
        assert_eq!(open(base), "slack://channel?team=T1&id=C9");
    }

    #[test]
    fn leaving_the_browser() {
        let config = Config {
            browser: Some("dia".into()),
            ..Config::default()
        };
        let lookups = Lookups::default();
        let router = Router { config: &config, lookups: &lookups, self_id: Some("me") };
        let rule = |rewrite: Option<&str>, open_in: Option<&str>| Rule {
            open_in: open_in.map(Into::into),
            ..Rule::new("r", "example.com", rewrite)
        };
        assert!(!router.leaves(&rule(None, None), "dia"));
        assert!(router.leaves(&rule(None, None), "arc"));
        assert!(router.leaves(&rule(None, Some("arc")), "dia"));
        assert!(!router.leaves(&rule(None, Some("arc")), "arc"));
        assert!(!router.leaves(&rule(None, Some("me")), "dia"));
        assert!(router.leaves(&rule(None, Some("ask")), "dia"));
        assert!(router.leaves(&rule(Some("slack://x"), None), "dia"));
        assert!(!router.leaves(&rule(Some("HTTPS://x.com/(url)"), None), "dia"));
        assert!(router.leaves(&rule(Some("(url)"), None), "dia"));
    }

    #[test]
    fn default_rules_compile() {
        for rule in &Config::default().rules {
            compile(rule).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        }
    }
}
