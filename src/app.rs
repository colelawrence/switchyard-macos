//! Shared state and what happens when a link arrives.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::{App, AppContext as _, Entity, Global, Image, ImageFormat, SharedString};

use crate::config::{Config, Lookups, Modifier};
use crate::extension;
use crate::mac;
use crate::router::{Outcome, Router, Target};
use crate::ui;

const RECENT: usize = 50;

#[derive(Clone)]
pub struct Browser {
    pub id: String,
    pub name: SharedString,
    pub icon: Option<Arc<Image>>,
}

#[derive(Clone)]
pub struct Event {
    pub at: SystemTime,
    pub url: String,
    pub result: String,
    pub ok: bool,
}

pub struct State {
    pub config: Config,
    pub lookups: Lookups,
    pub recent: VecDeque<Event>,
    pub browsers: Vec<Browser>,
    pub self_id: Option<String>,
    /// A config file that failed to load or save.
    pub problem: Option<String>,
}

struct Shared(Entity<State>);

impl Global for Shared {}

pub fn state(cx: &App) -> Entity<State> {
    cx.global::<Shared>().0.clone()
}

impl State {
    pub fn load() -> Self {
        let self_id = mac::self_bundle_id();
        let mut problem = None;
        let config = match Config::load() {
            Ok((mut config, fresh)) => {
                if fresh {
                    // Start by sending unmatched links where they went before.
                    config.browser = mac::default_browser().filter(|b| Some(b) != self_id.as_ref());
                    if let Err(e) = config.save() {
                        problem = Some(format!("{e:#}"));
                    }
                }
                config
            }
            Err(e) => {
                problem = Some(format!("{e:#}"));
                Config::default()
            }
        };
        let lookups = Lookups::load().unwrap_or_else(|e| {
            problem = Some(format!("{e:#}"));
            Lookups::default()
        });
        let mut state = Self {
            config,
            lookups,
            recent: VecDeque::new(),
            browsers: Vec::new(),
            self_id,
            problem,
        };
        state.refresh_browsers();
        state
    }

    pub fn refresh_browsers(&mut self) {
        self.browsers = mac::browsers()
            .into_iter()
            .filter(|id| Some(id) != self.self_id.as_ref())
            .map(|id| Browser {
                name: mac::app_name(&id).into(),
                icon: mac::app_icon_png(&id, 32.)
                    .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png))),
                id,
            })
            .collect();
        self.browsers.sort_by_key(|b| b.name.to_lowercase());
    }

    pub fn browser_name(&self, id: &str) -> SharedString {
        self.browsers
            .iter()
            .find(|b| b.id.eq_ignore_ascii_case(id))
            .map(|b| b.name.clone())
            .unwrap_or_else(|| mac::app_name(id).into())
    }

    pub fn router(&self) -> Router<'_> {
        Router {
            config: &self.config,
            lookups: &self.lookups,
            self_id: self.self_id.as_deref(),
        }
    }

    pub fn save_config(&mut self) {
        self.problem = self.config.save().err().map(|e| format!("{e:#}"));
    }

    pub fn save_lookups(&mut self) {
        self.problem = self.lookups.save().err().map(|e| format!("{e:#}"));
    }

    pub fn reload(&mut self) {
        *self = Self::load();
    }

    fn record(&mut self, url: &str, result: impl Into<String>, ok: bool) {
        let result = result.into();
        if ok {
            log::info!("{url} → {result}");
        } else {
            log::warn!("{url} → {result}");
        }
        self.recent.push_front(Event {
            at: SystemTime::now(),
            url: url.to_string(),
            result,
            ok,
        });
        self.recent.truncate(RECENT);
    }
}

pub fn init(cx: &mut App) {
    let state = cx.new(|_| State::load());
    cx.set_global(Shared(state));
    if let Err(e) = extension::install() {
        log::warn!("could not set up the browser extension: {e:#}");
    }
}

fn picker_held(modifier: Modifier) -> bool {
    let held = mac::modifiers();
    match modifier {
        Modifier::None => false,
        Modifier::Option => held.option,
        Modifier::Shift => held.shift,
        Modifier::Control => held.control,
        Modifier::Command => held.command,
    }
}

/// Routes a link that was clicked somewhere.
pub fn handle_url(url: &str, cx: &mut App) {
    let state = state(cx);
    let held = picker_held(state.read(cx).config.picker_modifier);
    let outcome = state.read(cx).router().route(url, held);
    dispatch(url, outcome, cx);
}

fn describe(target: &Target, state: &State) -> String {
    match target {
        Target::App(id) => format!("opened in {}", state.browser_name(id)),
        Target::SchemeDefault => "opened in its app".into(),
        Target::Ask => "asked which browser".into(),
    }
}

pub fn dispatch(original: &str, outcome: Outcome, cx: &mut App) {
    let state = state(cx);
    match outcome {
        Outcome::Open {
            url,
            target,
            rule,
            note,
        } => {
            let result = match &target {
                Target::App(id) => {
                    extension::note_handoff(&url);
                    mac::open_with(&url, id)
                }
                Target::SchemeDefault => mac::open(&url),
                Target::Ask => {
                    ui::picker::open(url.clone(), cx);
                    Ok(())
                }
            };
            let mut summary = match &rule {
                Some(rule) => format!("“{rule}”: {url}, {}", describe(&target, state.read(cx))),
                None => describe(&target, state.read(cx)),
            };
            if let Some(note) = note {
                summary.push_str(&format!(" ({note})"));
            }
            match result {
                Ok(()) => state.update(cx, |s, cx| {
                    s.record(original, summary, true);
                    cx.notify();
                }),
                Err(e) => {
                    // A rewritten link whose app is missing still opens, just in the browser.
                    state.update(cx, |s, cx| {
                        s.record(original, format!("{summary} failed: {e:#}; used the browser"), false);
                        cx.notify();
                    });
                    open_in_browser(original, cx);
                }
            }
        }
        Outcome::NeedValue {
            table,
            key,
            rule,
            url,
        } => {
            state.update(cx, |s, cx| {
                s.record(original, format!("“{rule}” needs #{table} → {key}"), true);
                cx.notify();
            });
            ui::prompt::open(ui::prompt::Ask { table, key, rule, url }, cx);
        }
        Outcome::Invalid(message) => state.update(cx, |s, cx| {
            s.record(original, message, false);
            cx.notify();
        }),
    }
}

/// Opens the link in the fallback browser, skipping every rule.
pub fn open_in_browser(url: &str, cx: &mut App) {
    let target = state(cx)
        .read(cx)
        .router()
        .route_unruled(url);
    match target {
        Target::App(id) => {
            extension::note_handoff(url);
            if let Err(e) = mac::open_with(url, &id) {
                state(cx).update(cx, |s, _| s.record(url, format!("{e:#}"), false));
                ui::picker::open(url.to_string(), cx);
            }
        }
        _ => ui::picker::open(url.to_string(), cx),
    }
}

/// Opens the link in a browser the user picked.
pub fn open_picked(url: &str, id: &str, cx: &mut App) {
    extension::note_handoff(url);
    let result = mac::open_with(url, id);
    state(cx).update(cx, |s, cx| {
        match result {
            Ok(()) => {
                let name = s.browser_name(id);
                s.record(url, format!("opened in {name}"), true);
            }
            Err(e) => s.record(url, format!("{e:#}"), false),
        }
        cx.notify();
    });
}

/// Stores a lookup value the user supplied, then routes the link again.
pub fn supply(table: &str, key: &str, value: &str, url: &str, cx: &mut App) {
    state(cx).update(cx, |s, cx| {
        s.lookups.set(table, key, value);
        s.save_lookups();
        cx.notify();
    });
    let outcome = state(cx).read(cx).router().route(url, false);
    dispatch(url, outcome, cx);
}
