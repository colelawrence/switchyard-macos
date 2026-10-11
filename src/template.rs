//! Rewrite templates such as `slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)`.
//!
//! Everything outside parentheses is literal. Inside them:
//! - `(NAME)` inserts a capture from the match pattern, or one of the built-ins
//!   `url`, `scheme`, `host`, `path`, `query`, `fragment`.
//! - `(function arg arg ...)` calls a function. Arguments are names, `#Tables`,
//!   `"strings"`, or nested `(calls)`.
//!
//! Functions: `lookup #Table KEY`, `encode`, `decode`, `lower`, `upper`,
//! `or A B ...` (first non-empty), `query "param"`, `replace X "from" "to"`.
//! `\` escapes the next character.

use std::fmt;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use url::Url;

use crate::pattern::Captures;

pub const BUILTINS: &[&str] = &["url", "scheme", "host", "path", "query", "fragment"];
const FUNCTIONS: &[&str] = &[
    "lookup", "encode", "decode", "lower", "upper", "or", "query", "replace",
];

/// Characters `encode` leaves alone, matching `encodeURIComponent`.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A lookup table has no entry for this key yet. The app asks the user for it.
    Missing { table: String, key: String },
    UnknownVar(String),
    Invalid(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::Missing { table, key } => write!(f, "#{table} has no entry for `{key}`"),
            EvalError::UnknownVar(name) => write!(f, "`({name})` is not captured by the pattern"),
            EvalError::Invalid(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for EvalError {}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Var(String),
    Table(String),
    Str(String),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    parts: Vec<Part>,
}

/// What a template can read while it is evaluated.
pub struct Scope<'a> {
    pub url: &'a Url,
    pub captures: &'a Captures,
    pub lookup: &'a dyn Fn(&str, &str) -> Option<String>,
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    src: &'a str,
}

impl<'a> Parser<'a> {
    fn error(&self, msg: impl Into<String>) -> EvalError {
        EvalError::Invalid(msg.into())
    }

    fn skip_space(&mut self) {
        while self.chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn word(&mut self) -> String {
        let mut word = String::new();
        while let Some(&(_, c)) = self.chars.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                word.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        word
    }

    /// Parses the inside of `( ... )`; the `(` has been consumed.
    fn group(&mut self) -> Result<Expr, EvalError> {
        self.skip_space();
        let head = self.word();
        if head.is_empty() {
            let rest: String = self.src[self.chars.peek().map_or(self.src.len(), |(i, _)| *i)..]
                .chars()
                .take(12)
                .collect();
            return Err(self.error(format!("expected a name or function after `(`, found `{rest}`")));
        }
        let mut args = Vec::new();
        loop {
            self.skip_space();
            match self.chars.next() {
                Some((_, ')')) => break,
                Some((_, '(')) => args.push(self.group()?),
                Some((_, '#')) => {
                    let name = self.word();
                    if name.is_empty() {
                        return Err(self.error("`#` must be followed by a table name"));
                    }
                    args.push(Expr::Table(name));
                }
                Some((_, '"')) => {
                    let mut text = String::new();
                    loop {
                        match self.chars.next() {
                            Some((_, '"')) => break,
                            Some((_, '\\')) => match self.chars.next() {
                                Some((_, c)) => text.push(c),
                                None => return Err(self.error("unterminated string")),
                            },
                            Some((_, c)) => text.push(c),
                            None => return Err(self.error("unterminated string")),
                        }
                    }
                    args.push(Expr::Str(text));
                }
                Some((_, c)) if c.is_ascii_alphanumeric() || c == '_' => {
                    let mut name = c.to_string();
                    name.push_str(&self.word());
                    args.push(Expr::Var(name));
                }
                Some((_, c)) => {
                    return Err(self.error(format!("unexpected `{c}` inside `({head} ...)`")));
                }
                None => return Err(self.error(format!("`({head}` is missing its `)`"))),
            }
        }
        if args.is_empty() {
            return Ok(Expr::Var(head));
        }
        if !FUNCTIONS.contains(&head.as_str()) {
            return Err(self.error(format!(
                "`{head}` is not a function; try one of {}",
                FUNCTIONS.join(", ")
            )));
        }
        let arity_ok = match head.as_str() {
            "lookup" => args.len() == 2 && matches!(args[0], Expr::Table(_)),
            "replace" => args.len() == 3,
            "or" => true,
            _ => args.len() == 1,
        };
        if !arity_ok {
            let usage = match head.as_str() {
                "lookup" => "(lookup #Table KEY)",
                "replace" => "(replace VALUE \"from\" \"to\")",
                "query" => "(query \"param\")",
                _ => "one argument",
            };
            return Err(self.error(format!("`{head}` expects {usage}")));
        }
        Ok(Expr::Call(head, args))
    }
}

impl Template {
    pub fn parse(src: &str) -> Result<Self, EvalError> {
        let mut parser = Parser {
            chars: src.char_indices().peekable(),
            src,
        };
        let mut parts = Vec::new();
        let mut lit = String::new();
        while let Some((_, c)) = parser.chars.next() {
            match c {
                '\\' => match parser.chars.next() {
                    Some((_, c)) => lit.push(c),
                    None => return Err(parser.error("a `\\` at the end escapes nothing")),
                },
                '(' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Expr(parser.group()?));
                }
                ')' => return Err(parser.error("a `)` has no matching `(`")),
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Self { parts })
    }

    /// Checks that every name the template reads is a capture or a built-in.
    pub fn check(&self, captures: &[String]) -> Result<(), EvalError> {
        fn walk(expr: &Expr, captures: &[String]) -> Result<(), EvalError> {
            match expr {
                Expr::Var(name) => {
                    if captures.contains(name) || BUILTINS.contains(&name.as_str()) {
                        Ok(())
                    } else {
                        Err(EvalError::UnknownVar(name.clone()))
                    }
                }
                Expr::Call(_, args) => args.iter().try_for_each(|a| walk(a, captures)),
                Expr::Table(_) | Expr::Str(_) => Ok(()),
            }
        }
        self.parts.iter().try_for_each(|part| match part {
            Part::Lit(_) => Ok(()),
            Part::Expr(expr) => walk(expr, captures),
        })
    }

    /// The lookup tables this template reads.
    #[allow(dead_code)]
    pub fn tables(&self) -> Vec<String> {
        fn walk(expr: &Expr, out: &mut Vec<String>) {
            match expr {
                Expr::Table(name) if !out.contains(name) => out.push(name.clone()),
                Expr::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for part in &self.parts {
            if let Part::Expr(expr) = part {
                walk(expr, &mut out);
            }
        }
        out
    }

    pub fn render(&self, scope: &Scope) -> Result<String, EvalError> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(text) => out.push_str(text),
                Part::Expr(expr) => out.push_str(&eval(expr, scope)?),
            }
        }
        Ok(out)
    }
}

fn var(name: &str, scope: &Scope) -> Result<String, EvalError> {
    if let Some(value) = scope.captures.get(name) {
        return Ok(value.clone());
    }
    let url = scope.url;
    Ok(match name {
        "url" => url.as_str().to_string(),
        "scheme" => url.scheme().to_string(),
        "host" => url.host_str().unwrap_or("").to_string(),
        "path" => url.path().to_string(),
        "query" => url.query().unwrap_or("").to_string(),
        "fragment" => url.fragment().unwrap_or("").to_string(),
        _ => return Err(EvalError::UnknownVar(name.to_string())),
    })
}

fn eval(expr: &Expr, scope: &Scope) -> Result<String, EvalError> {
    match expr {
        Expr::Var(name) => var(name, scope),
        Expr::Str(text) => Ok(text.clone()),
        Expr::Table(name) => Err(EvalError::Invalid(format!(
            "#{name} can only be used inside (lookup #{name} KEY)"
        ))),
        Expr::Call(name, args) => {
            let arg = |i: usize| eval(&args[i], scope);
            match name.as_str() {
                "lookup" => {
                    let Expr::Table(table) = &args[0] else {
                        unreachable!("checked when parsed")
                    };
                    let key = arg(1)?;
                    (scope.lookup)(table, &key).ok_or_else(|| EvalError::Missing {
                        table: table.clone(),
                        key,
                    })
                }
                "encode" => Ok(utf8_percent_encode(&arg(0)?, COMPONENT).to_string()),
                "decode" => Ok(percent_decode_str(&arg(0)?).decode_utf8_lossy().into_owned()),
                "lower" => Ok(arg(0)?.to_lowercase()),
                "upper" => Ok(arg(0)?.to_uppercase()),
                "replace" => Ok(arg(0)?.replace(&arg(1)?, &arg(2)?)),
                "query" => {
                    let wanted = arg(0)?;
                    Ok(scope
                        .url
                        .query()
                        .unwrap_or("")
                        .split('&')
                        .filter_map(|p| p.split_once('=').or(Some((p, ""))))
                        .find(|(k, _)| *k == wanted)
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default())
                }
                "or" => {
                    // A missing lookup falls through to the next choice instead of prompting.
                    for (i, a) in args.iter().enumerate() {
                        match eval(a, scope) {
                            Ok(v) if !v.is_empty() => return Ok(v),
                            Ok(_) => {}
                            Err(EvalError::Missing { .. }) if i + 1 < args.len() => {}
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(String::new())
                }
                _ => unreachable!("checked when parsed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(template: &str, url: &str, caps: &[(&str, &str)], table: &[(&str, &str, &str)]) -> Result<String, EvalError> {
        let url = Url::parse(url).unwrap();
        let captures: Captures = caps.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let lookup = |t: &str, k: &str| {
            table
                .iter()
                .find(|(tt, kk, _)| *tt == t && *kk == k)
                .map(|(_, _, v)| v.to_string())
        };
        Template::parse(template)?.render(&Scope {
            url: &url,
            captures: &captures,
            lookup: &lookup,
        })
    }

    const SLACK: &str = "slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)";
    const SLACK_URL: &str = "https://acme.slack.com/archives/C0123ABCDEF";
    const SLACK_CAPS: &[(&str, &str)] = &[("SUBDOMAIN", "acme"), ("CHANNEL", "C0123ABCDEF")];

    #[test]
    fn slack_missing_lookup() {
        assert_eq!(
            render(SLACK, SLACK_URL, SLACK_CAPS, &[]),
            Err(EvalError::Missing {
                table: "SlackTeamIds".into(),
                key: "acme".into()
            })
        );
    }

    #[test]
    fn slack_with_lookup() {
        assert_eq!(
            render(SLACK, SLACK_URL, SLACK_CAPS, &[("SlackTeamIds", "acme", "T012345")]),
            Ok("slack://channel?team=T012345&id=C0123ABCDEF".into())
        );
    }

    #[test]
    fn functions() {
        let url = "https://x.com/a%20b?pwd=s3cret&q=Hi";
        assert_eq!(render("(query \"pwd\")", url, &[], &[]), Ok("s3cret".into()));
        assert_eq!(render("(query \"none\")", url, &[], &[]), Ok("".into()));
        assert_eq!(render("(decode path)", url, &[], &[]), Ok("/a b".into()));
        assert_eq!(render("(encode url)", "https://x.com/?a=b", &[], &[]), Ok("https%3A%2F%2Fx.com%2F%3Fa%3Db".into()));
        assert_eq!(render("(upper (query \"q\"))", url, &[], &[]), Ok("HI".into()));
        assert_eq!(render("(replace host \".\" \"-\")", url, &[], &[]), Ok("x-com".into()));
        assert_eq!(render("(or (lookup #T host) \"dflt\")", url, &[], &[]), Ok("dflt".into()));
        assert_eq!(render("(or (query \"none\") host)", url, &[], &[]), Ok("x.com".into()));
        assert_eq!(render(r"lit\(eral\)", url, &[], &[]), Ok("lit(eral)".into()));
    }

    #[test]
    fn check_and_tables() {
        let t = Template::parse(SLACK).unwrap();
        assert_eq!(t.check(&["SUBDOMAIN".into(), "CHANNEL".into()]), Ok(()));
        assert_eq!(t.check(&["SUBDOMAIN".into()]), Err(EvalError::UnknownVar("CHANNEL".into())));
        assert_eq!(t.tables(), vec!["SlackTeamIds".to_string()]);
    }

    #[test]
    fn parse_errors() {
        assert!(Template::parse("(lookup SUBDOMAIN)").is_err());
        assert!(Template::parse("(frob X)").is_err());
        assert!(Template::parse("(X").is_err());
        assert!(Template::parse("x)").is_err());
        assert!(Template::parse("( )").is_err());
    }
}
