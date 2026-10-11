//! Match patterns such as `(SUBDOMAIN).slack.com/archives/(CHANNEL)`.
//!
//! Grammar, piece by piece:
//! - `scheme://` is optional; without it a pattern matches `http` and `https`. `*://` matches any.
//! - `(NAME)` captures one host label or path segment; `(NAME...)` captures the rest, slashes too.
//! - `*` matches within one label or segment, `**` across them. A leading `**.` in the host
//!   matches any number of subdomains, including none.
//! - A pattern without a path matches every path on the host. With one, the whole path must
//!   match (a trailing `/` is optional, and a trailing `/**` also matches nothing at all).
//! - `?key=(VALUE)&other` requires those query parameters in any order; others may be present.
//!   Without `?` the query is ignored. `#(FRAGMENT)` works the same way for the fragment.
//! - `\` escapes the next character.

use std::collections::BTreeMap;
use std::fmt;

use regex::Regex;
use serde::Serialize;
use url::Url;

pub type Captures = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(pub String);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PatternError {}

#[derive(Debug, Clone)]
enum Scheme {
    Web,
    Any,
    Exact(String),
}

#[derive(Debug, Clone)]
pub struct Pattern {
    scheme: Scheme,
    opaque: bool,
    host: Option<Regex>,
    path: Option<Regex>,
    query: Vec<(String, Option<Regex>)>,
    fragment: Option<Regex>,
    names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Lit(String),
    Capture { name: String, rest: bool },
    Star,
    Glob,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn tokenize(src: &str) -> Result<Vec<Token>, PatternError> {
    let mut tokens = Vec::new();
    let mut lit = String::new();
    let mut chars = src.chars().peekable();
    let flush = |lit: &mut String, tokens: &mut Vec<Token>| {
        if !lit.is_empty() {
            tokens.push(Token::Lit(std::mem::take(lit)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(next) => lit.push(next),
                None => return Err(PatternError("a `\\` at the end escapes nothing".into())),
            },
            '*' => {
                flush(&mut lit, &mut tokens);
                if chars.peek() == Some(&'*') {
                    chars.next();
                    tokens.push(Token::Glob);
                } else {
                    tokens.push(Token::Star);
                }
            }
            '(' => {
                flush(&mut lit, &mut tokens);
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some(')') => break,
                        Some(c) => inner.push(c),
                        None => {
                            return Err(PatternError(format!("`({inner}` is missing its `)`")));
                        }
                    }
                }
                let (name, rest) = match inner.strip_suffix("...") {
                    Some(name) => (name, true),
                    None => (inner.as_str(), false),
                };
                let valid = name.chars().next().is_some_and(is_name_start)
                    && name.chars().all(is_name_char);
                if !valid {
                    return Err(PatternError(format!(
                        "`({inner})` should be a name like `(CHANNEL)` or `(PATH...)`"
                    )));
                }
                tokens.push(Token::Capture {
                    name: name.to_string(),
                    rest,
                });
            }
            ')' => return Err(PatternError("a `)` has no matching `(`".into())),
            c => lit.push(c),
        }
    }
    flush(&mut lit, &mut tokens);
    Ok(tokens)
}

/// Turns tokens into a regex body. `one` matches a char within a segment, `many` across.
fn compile(
    tokens: &[Token],
    one: &str,
    many: &str,
    names: &mut Vec<String>,
) -> Result<String, PatternError> {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Lit(text) => out.push_str(&regex::escape(text)),
            Token::Star => out.push_str(&format!("{one}*")),
            Token::Glob => out.push_str(&format!("{many}*")),
            Token::Capture { name, rest } => {
                if names.contains(name) {
                    return Err(PatternError(format!("`({name})` is captured twice")));
                }
                names.push(name.clone());
                let body = if *rest {
                    format!("{many}*")
                } else {
                    format!("{one}+")
                };
                out.push_str(&format!("(?P<{name}>{body})"));
            }
        }
    }
    Ok(out)
}

fn anchored(body: &str, case_insensitive: bool) -> Regex {
    let flags = if case_insensitive { "(?i)" } else { "" };
    Regex::new(&format!("{flags}^(?:{body})$")).expect("pattern pieces compile to valid regex")
}

/// Splits at the first `sep` not escaped by `\`.
fn split_unescaped(src: &str, sep: char) -> (&str, Option<&str>) {
    let mut escaped = false;
    let mut depth = 0;
    for (i, c) in src.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => return (&src[..i], Some(&src[i + 1..])),
            _ => {}
        }
    }
    (src, None)
}

fn scheme_prefix(src: &str) -> Option<(&str, &str, bool)> {
    let colon = src.find(':')?;
    let scheme = &src[..colon];
    let valid = scheme == "*"
        || (scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-".contains(c)));
    if !valid {
        return None;
    }
    let after = &src[colon + 1..];
    if let Some(rest) = after.strip_prefix("//") {
        return Some((scheme, rest, false));
    }
    // `localhost:3000` is a host and port, not a scheme.
    if after.starts_with(|c: char| c.is_ascii_digit()) || scheme == "*" {
        return None;
    }
    Some((scheme, after, true))
}

/// A pattern in a form JavaScript's `RegExp` can run, for the browser extension.
/// Each regex is anchored; `host` is matched case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exported {
    /// `web` for http and https, `*` for any scheme, or the scheme itself.
    pub scheme: String,
    pub opaque: bool,
    pub host: Option<String>,
    pub path: Option<String>,
    pub query: Vec<(String, Option<String>)>,
    pub fragment: Option<String>,
}

/// Rewrites a regex built by `compile` for JavaScript. The pieces it uses mean the same in
/// both, apart from the inline case flag and the spelling of named groups.
fn js_source(re: &Regex) -> String {
    let src = re.as_str();
    src.strip_prefix("(?i)").unwrap_or(src).replace("(?P<", "(?<")
}

impl Pattern {
    pub fn export(&self) -> Exported {
        Exported {
            scheme: match &self.scheme {
                Scheme::Web => "web".into(),
                Scheme::Any => "*".into(),
                Scheme::Exact(s) => s.clone(),
            },
            opaque: self.opaque,
            host: self.host.as_ref().map(js_source),
            path: self.path.as_ref().map(js_source),
            query: self
                .query
                .iter()
                .map(|(k, v)| (k.clone(), v.as_ref().map(js_source)))
                .collect(),
            fragment: self.fragment.as_ref().map(js_source),
        }
    }

    pub fn parse(src: &str) -> Result<Self, PatternError> {
        let src = src.trim();
        if src.is_empty() {
            return Err(PatternError("the pattern is empty".into()));
        }
        let (scheme, rest, opaque) = match scheme_prefix(src) {
            Some((s, rest, opaque)) if s == "*" => (Scheme::Any, rest, opaque),
            Some((s, rest, opaque)) => (Scheme::Exact(s.to_ascii_lowercase()), rest, opaque),
            None => (Scheme::Web, src, false),
        };
        let mut names = Vec::new();
        let (rest, fragment) = split_unescaped(rest, '#');
        let (rest, query) = split_unescaped(rest, '?');

        let (host, path) = if opaque {
            let body = compile(&tokenize(rest)?, "[^?#]", "[^?#]", &mut names)?;
            (None, Some(anchored(&body, false)))
        } else {
            let (host_src, path_src) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            if host_src.is_empty() {
                return Err(PatternError("the pattern needs a host, like `example.com`".into()));
            }
            let (subdomains, host_src) = match host_src.strip_prefix("**.") {
                Some(host) => (true, host),
                None => (false, host_src),
            };
            let mut body = compile(&tokenize(host_src)?, "[^./:]", "[^/:]", &mut names)?;
            if subdomains {
                body = format!("(?:[^/:]*\\.)?{body}");
            }
            if !host_src.contains(':') {
                body.push_str("(?::[0-9]+)?");
            }
            let host = anchored(&body, true);
            let path = if path_src.is_empty() {
                None
            } else {
                let (trimmed, tail) = match path_src.strip_suffix("/**") {
                    Some(p) => (p, "(?:/.*)?"),
                    None => (path_src.strip_suffix('/').unwrap_or(path_src), "/?"),
                };
                let body = compile(&tokenize(trimmed)?, "[^/]", ".", &mut names)?;
                Some(anchored(&format!("{body}{tail}"), false))
            };
            (Some(host), path)
        };

        let mut params = Vec::new();
        if let Some(query) = query {
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                let (key, value) = match pair.split_once('=') {
                    Some((k, v)) => (k, Some(v)),
                    None => (pair, None),
                };
                if key.contains(['(', '*']) {
                    return Err(PatternError(format!(
                        "query key `{key}` must be literal; capture its value instead, like `{key}=(VALUE)`"
                    )));
                }
                let value = match value {
                    Some(v) => Some(anchored(
                        &compile(&tokenize(v)?, "[^&]", "[^&]", &mut names)?,
                        false,
                    )),
                    None => None,
                };
                params.push((key.replace('\\', ""), value));
            }
        }
        let fragment = match fragment {
            Some(f) => Some(anchored(&compile(&tokenize(f)?, ".", ".", &mut names)?, false)),
            None => None,
        };
        Ok(Self {
            scheme,
            opaque,
            host,
            path,
            query: params,
            fragment,
            names,
        })
    }

    /// The names this pattern captures, in order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn matches(&self, url: &Url) -> Option<Captures> {
        let scheme = url.scheme();
        let scheme_ok = match &self.scheme {
            Scheme::Web => scheme == "http" || scheme == "https",
            Scheme::Any => true,
            Scheme::Exact(s) => s == scheme,
        };
        if !scheme_ok || self.opaque != url.cannot_be_a_base() {
            return None;
        }
        let mut caps = Captures::new();
        let mut take = |re: &Regex, text: &str| -> bool {
            let Some(found) = re.captures(text) else {
                return false;
            };
            for name in re.capture_names().flatten() {
                if let Some(m) = found.name(name) {
                    caps.insert(name.to_string(), m.as_str().to_string());
                }
            }
            true
        };
        if let Some(host_re) = &self.host {
            let mut host = url.host_str().unwrap_or("").to_string();
            if let Some(port) = url.port() {
                host.push_str(&format!(":{port}"));
            }
            if !take(host_re, &host) {
                return None;
            }
        }
        if let Some(path_re) = &self.path
            && !take(path_re, url.path())
        {
            return None;
        }
        if !self.query.is_empty() {
            let pairs: Vec<(&str, &str)> = url
                .query()
                .unwrap_or("")
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|p| p.split_once('=').unwrap_or((p, "")))
                .collect();
            for (key, value_re) in &self.query {
                let found = pairs.iter().any(|(k, v)| {
                    k == key && value_re.as_ref().is_none_or(|re| take(re, v))
                });
                if !found {
                    return None;
                }
            }
        }
        if let Some(frag_re) = &self.fragment
            && !take(frag_re, url.fragment().unwrap_or(""))
        {
            return None;
        }
        Some(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_match(pattern: &str, url: &str) -> Option<Captures> {
        Pattern::parse(pattern)
            .unwrap()
            .matches(&Url::parse(url).unwrap())
    }

    fn caps(pairs: &[(&str, &str)]) -> Option<Captures> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn slack_channel() {
        assert_eq!(
            try_match(
                "(SUBDOMAIN).slack.com/archives/(CHANNEL)",
                "https://acme.slack.com/archives/C0123ABCDEF"
            ),
            caps(&[("SUBDOMAIN", "acme"), ("CHANNEL", "C0123ABCDEF")])
        );
    }

    #[test]
    fn path_is_anchored_with_optional_trailing_slash() {
        let p = "(S).slack.com/archives/(C)";
        assert!(try_match(p, "https://a.slack.com/archives/C1/").is_some());
        assert!(try_match(p, "https://a.slack.com/archives/C1/p123").is_none());
        assert!(try_match(p, "https://a.slack.com/archives").is_none());
    }

    #[test]
    fn trailing_glob_is_optional() {
        let p = "(S).slack.com/archives/(C)/**";
        assert!(try_match(p, "https://a.slack.com/archives/C1").is_some());
        assert!(try_match(p, "https://a.slack.com/archives/C1/p123").is_some());
    }

    #[test]
    fn host_only_matches_any_path() {
        assert!(try_match("github.com", "https://github.com/a/b?c=d").is_some());
        assert!(try_match("github.com", "https://gist.github.com/").is_none());
        assert!(try_match("GitHub.com", "https://github.com").is_some());
    }

    #[test]
    fn scheme_rules() {
        assert!(try_match("example.com", "ftp://example.com/").is_none());
        assert!(try_match("*://example.com", "ftp://example.com/").is_some());
        assert!(try_match("https://example.com", "http://example.com/").is_none());
        assert!(try_match("localhost:3000/x", "http://localhost:3000/x").is_some());
        assert!(try_match("localhost/x", "http://localhost:3000/x").is_some());
    }

    #[test]
    fn subdomain_glob() {
        let p = "**.zoom.us/j/(ID)";
        assert_eq!(
            try_match(p, "https://us02web.zoom.us/j/123"),
            caps(&[("ID", "123")])
        );
        assert!(try_match(p, "https://zoom.us/j/123").is_some());
        assert!(try_match(p, "https://notzoom.us/j/123").is_none());
        assert!(try_match("*.zoom.us", "https://zoom.us/").is_none());
    }

    #[test]
    fn rest_capture() {
        assert_eq!(
            try_match("github.com/(REPO...)", "https://github.com/a/b/tree/main"),
            caps(&[("REPO", "a/b/tree/main")])
        );
    }

    #[test]
    fn query_params_any_order() {
        let p = "example.com/watch?v=(ID)&list";
        assert_eq!(
            try_match(p, "https://example.com/watch?list=x&t=4&v=abc"),
            caps(&[("ID", "abc")])
        );
        assert!(try_match(p, "https://example.com/watch?v=abc").is_none());
    }

    #[test]
    fn fragment_and_opaque() {
        assert_eq!(
            try_match("example.com#(F)", "https://example.com/#top"),
            caps(&[("F", "top")])
        );
        assert_eq!(
            try_match("mailto:(ADDR)", "mailto:a@b.c"),
            caps(&[("ADDR", "a@b.c")])
        );
    }

    #[test]
    fn errors() {
        assert!(Pattern::parse("(A.slack.com").is_err());
        assert!(Pattern::parse("(a b).com").is_err());
        assert!(Pattern::parse("(A).com/(A)").is_err());
        assert!(Pattern::parse("/just/a/path").is_err());
        assert!(Pattern::parse("").is_err());
    }

    #[test]
    fn escapes() {
        assert!(try_match(r"example.com/\(x\)", "https://example.com/(x)").is_some());
        assert!(try_match(r"example.com/a\*b", "https://example.com/a*b").is_some());
        assert!(try_match(r"example.com/a\*b", "https://example.com/aXb").is_none());
    }
}
