//! The settings window: rules, lookup tables, recent links and general options.

use std::time::SystemTime;

use ely_gpui_component::buttons::{Button, ButtonVariant, IconButton, SegmentedControl};
use ely_gpui_component::forms::{Choice, Input, InputEvent, Select, Switch, TextInput};
use ely_gpui_component::layout::ScrollArea;
use ely_gpui_component::lists::{List, ListItem};
use ely_gpui_component::navigation::NavItem;
use ely_gpui_component::primitives::{FocusScope, Icon, IconName, Severity};
use ely_gpui_component::settings::{SettingsRow, SettingsSection};
use ely_gpui_component::shell::{ColumnShell, ShellColumn};
use ely_gpui_component::theme::{ActiveTheme, ControlSize, Radius};
use ely_gpui_component::feedback::InlineMessage;
use ely_gpui_component::typography::{Caption, Code, KbdCombo, Label, Paragraph};
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Task, Window, WindowHandle,
    WindowKind, div, img, px,
};
use url::Url;

use crate::app::{self, State};
use crate::config::{self, Modifier, OPEN_ASK, Rule};
use crate::extension;
use crate::mac;
use crate::router::compile;
use crate::template::{EvalError, Scope};

const SCHEME_DEFAULT: &str = "default";

#[derive(Clone, Copy)]
struct Open(WindowHandle<Settings>);

impl gpui::Global for Open {}

/// Shows the settings window, reusing it if it is already open.
pub fn show(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Open>().map(|o| o.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }
    app::state(cx).update(cx, |s, _| s.refresh_browsers());
    let options = super::window_options("Switchyard", 980., 640., WindowKind::Normal, cx);
    match cx.open_window(options, |window, cx| cx.new(|cx| Settings::new(window, cx))) {
        Ok(handle) => cx.set_global(Open(handle)),
        Err(e) => log::error!("could not open settings: {e:#}"),
    }
    cx.activate(true);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Rules,
    Lookups,
    Recent,
    General,
}

pub struct Settings {
    focus: FocusHandle,
    page: Page,
    rule: usize,
    name: Entity<TextInput>,
    pattern: Entity<TextInput>,
    rewrite: Entity<TextInput>,
    tester: Entity<TextInput>,
    table: Option<String>,
    new_key: Entity<TextInput>,
    new_value: Entity<TextInput>,
    new_table: Entity<TextInput>,
    /// Set while filling the inputs from the config, so the echo is not saved back.
    loading: bool,
    /// The browser whose extension is being set up, and what went wrong doing it.
    setup: Option<String>,
    setup_error: Option<String>,
    _subs: Vec<Subscription>,
    _ticker: Task<()>,
}

#[derive(Clone, Copy)]
enum Field {
    Name,
    Pattern,
    Rewrite,
}

impl Settings {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, rule: bool, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let input = TextInput::new(window, cx).placeholder(placeholder);
                if rule { input.highlighter(super::highlight_rule) } else { input }
            })
        };
        let name = input("Rule name", false, window, cx);
        let pattern = input("(SUBDOMAIN).slack.com/archives/(CHANNEL)", true, window, cx);
        let rewrite = input("slack://channel?team=(lookup #SlackTeamIds SUBDOMAIN)&id=(CHANNEL)", true, window, cx);
        let tester = input("Paste a link to try the rule", false, window, cx);
        let new_key = input("Key", false, window, cx);
        let new_value = input("Value", false, window, cx);
        let new_table = input("NewTable", false, window, cx);

        let mut subs = Vec::new();
        for (entity, field) in [(&name, Field::Name), (&pattern, Field::Pattern), (&rewrite, Field::Rewrite)] {
            subs.push(cx.subscribe(entity, move |this, input, event, cx| {
                if matches!(event, InputEvent::Changed) && !this.loading {
                    let text = input.read(cx).text().to_string();
                    this.write_field(field, text, cx);
                }
            }));
        }
        subs.push(cx.subscribe(&tester, |_, _, _: &InputEvent, cx| cx.notify()));
        subs.push(cx.subscribe(&new_value, |this, _, event, cx| {
            if matches!(event, InputEvent::Submit) {
                this.add_entry(cx);
            }
        }));
        subs.push(cx.subscribe(&new_table, |this, _, event, cx| {
            if matches!(event, InputEvent::Submit) {
                this.add_table(cx);
            }
        }));
        subs.push(cx.observe(&app::state(cx), |_, _, cx| cx.notify()));

        // Browsers check in on their own, so the General page polls to show them connecting.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.page == Page::General {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut this = Self {
            focus,
            page: Page::Rules,
            rule: 0,
            name,
            pattern,
            rewrite,
            tester,
            table: None,
            new_key,
            new_value,
            new_table,
            loading: false,
            setup: None,
            setup_error: None,
            _subs: subs,
            _ticker: ticker,
        };
        this.select_rule(0, cx);
        this.tester.update(cx, |t, cx| {
            t.set_text("https://acme.slack.com/archives/C0123ABCDEF", cx)
        });
        this
    }

    fn select_rule(&mut self, ix: usize, cx: &mut Context<Self>) {
        let rule = app::state(cx).read(cx).config.rules.get(ix).cloned();
        self.rule = ix;
        self.loading = true;
        let rule = rule.unwrap_or_else(|| Rule::new("", "", None));
        self.name.update(cx, |i, cx| i.set_text(rule.name, cx));
        self.pattern.update(cx, |i, cx| i.set_text(rule.pattern, cx));
        self.rewrite
            .update(cx, |i, cx| i.set_text(rule.rewrite.unwrap_or_default(), cx));
        self.loading = false;
        cx.notify();
    }

    fn edit_rules(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Vec<Rule>, &mut usize)) {
        let mut ix = self.rule;
        app::state(cx).update(cx, |s, cx| {
            edit(&mut s.config.rules, &mut ix);
            s.save_config();
            cx.notify();
        });
        self.rule = ix;
    }

    fn write_field(&mut self, field: Field, text: String, cx: &mut Context<Self>) {
        self.edit_rules(cx, |rules, ix| {
            if let Some(rule) = rules.get_mut(*ix) {
                match field {
                    Field::Name => rule.name = text,
                    Field::Pattern => rule.pattern = text,
                    Field::Rewrite => rule.rewrite = (!text.trim().is_empty()).then_some(text),
                }
            }
        });
    }

    fn add_rule(&mut self, cx: &mut Context<Self>) {
        self.edit_rules(cx, |rules, ix| {
            rules.push(Rule::new("New rule", "example.com/(PATH...)", Some("(url)")));
            *ix = rules.len() - 1;
        });
        let ix = self.rule;
        self.select_rule(ix, cx);
    }

    fn delete_rule(&mut self, cx: &mut Context<Self>) {
        self.edit_rules(cx, |rules, ix| {
            if *ix < rules.len() {
                rules.remove(*ix);
            }
            *ix = (*ix).min(rules.len().saturating_sub(1));
        });
        let ix = self.rule;
        self.select_rule(ix, cx);
    }

    fn move_rule(&mut self, up: bool, cx: &mut Context<Self>) {
        self.edit_rules(cx, |rules, ix| {
            let to = if up { ix.checked_sub(1) } else { Some(*ix + 1) };
            if let Some(to) = to.filter(|to| *to < rules.len()) {
                rules.swap(*ix, to);
                *ix = to;
            }
        });
        cx.notify();
    }

    fn add_entry(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else { return };
        let key = self.new_key.read(cx).text().trim().to_string();
        let value = self.new_value.read(cx).text().trim().to_string();
        if key.is_empty() {
            return;
        }
        app::state(cx).update(cx, |s, cx| {
            s.lookups.set(&table, &key, &value);
            s.save_lookups();
            cx.notify();
        });
        self.new_key.update(cx, |i, cx| i.set_text("", cx));
        self.new_value.update(cx, |i, cx| i.set_text("", cx));
    }

    fn add_table(&mut self, cx: &mut Context<Self>) {
        let name: String = self
            .new_table
            .read(cx)
            .text()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            return;
        }
        app::state(cx).update(cx, |s, cx| {
            s.lookups.0.entry(name.clone()).or_default();
            s.save_lookups();
            cx.notify();
        });
        self.table = Some(name);
        self.new_table.update(cx, |i, cx| i.set_text("", cx));
    }
}

fn update_config(cx: &mut App, edit: impl FnOnce(&mut config::Config)) {
    app::state(cx).update(cx, |s, cx| {
        edit(&mut s.config);
        s.save_config();
        cx.notify();
    });
}

fn mono(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .font_family(cx.theme().mono_family.clone())
        .text_sm()
        .child(text.into())
}

fn field(label: &str, body: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(Label::new(label.to_string()))
        .child(body)
}

impl Settings {
    fn nav(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let items = [
            (Page::Rules, IconName::Workflow, "Rules", Some(state.config.rules.len())),
            (Page::Lookups, IconName::Table, "Lookup tables", Some(state.lookups.0.len())),
            (Page::Recent, IconName::History, "Recent", None),
            (Page::General, IconName::Settings, "General", None),
        ];
        let entity = cx.entity();
        let nav = div().p_2().flex().flex_col().gap_0p5().children(items.into_iter().map(
            |(page, icon, label, count)| {
                let entity = entity.clone();
                let item = NavItem::new(label, icon, label)
                    .active(self.page == page)
                    .on_click(move |_, cx| {
                        entity.update(cx, |this, cx| {
                            this.page = page;
                            cx.notify();
                        })
                    });
                match count {
                    Some(n) => item.count(n.to_string()),
                    None => item,
                }
            },
        ));
        ShellColumn::new("nav")
            .width(cx.theme().sidebar_width(false))
            .child(nav)
    }

    fn rule_list(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let entity = cx.entity();
        let rows = state.config.rules.iter().enumerate().map(|(ix, rule)| {
            let broken = compile(rule).is_err();
            let entity = entity.clone();
            let toggle = entity.clone();
            ListItem::new(("rule", ix), if rule.name.is_empty() { "Untitled".into() } else { rule.name.clone() })
                .description(rule.pattern.clone())
                .selected(ix == self.rule)
                .quiet(!rule.enabled)
                .leading(Icon::new(if broken { IconName::TriangleAlert } else { IconName::Shuffle }))
                .trailing(
                    Switch::new(("enabled", ix), rule.enabled).on_change(move |on, _, cx| {
                        toggle.update(cx, |this, cx| {
                            this.edit_rules(cx, |rules, _| rules[ix].enabled = on);
                        })
                    }),
                )
                .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.select_rule(ix, cx)))
        });
        let add = cx.entity();
        ShellColumn::new("rules")
            .width(gpui::px(290.))
            .title("Rules")
            .action(
                IconButton::new("add-rule", IconName::Plus)
                    .tooltip("Add a rule")
                    .on_click(move |_, _, cx| add.update(cx, |this, cx| this.add_rule(cx))),
            )
            .child(
                ScrollArea::new("rule-scroll")
                    .vertical()
                    .size_full()
                    .child(div().p_2().child(List::new().children(rows))),
            )
    }

    fn rule_editor(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let Some(rule) = state.config.rules.get(self.rule).cloned() else {
            return ShellColumn::new("editor").child(
                div()
                    .p_6()
                    .child(Paragraph::new("No rules yet. Add one with +.")),
            );
        };
        let compiled = compile(&rule);
        let error = compiled.as_ref().err().cloned();
        let pattern_bad = error.as_deref().is_some_and(|e| e.starts_with("Match"));
        let rewrite_bad = error.as_deref().is_some_and(|e| e.starts_with("Rewrite"));

        let mut choices = vec![
            Choice::new(SCHEME_DEFAULT, "The app for the link’s scheme").icon(IconName::Zap),
            Choice::new(OPEN_ASK, "Ask every time").icon(IconName::CircleHelp),
        ];
        choices.extend(state.browsers.iter().map(|b| Choice::new(b.id.clone(), b.name.clone())));
        let open_in = rule.open_in.clone().unwrap_or_else(|| SCHEME_DEFAULT.into());

        let trial = self.trial(&rule, state, cx);
        let ix = self.rule;
        let count = state.config.rules.len();
        let (up, down, delete, select, route) = (cx.entity(), cx.entity(), cx.entity(), cx.entity(), cx.entity());

        let body = div()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(field("Name", Input::new(&self.name)))
            .child(field("Match", Input::new(&self.pattern).invalid(pattern_bad)))
            .child(field("Rewrite to", Input::new(&self.rewrite).invalid(rewrite_bad)))
            .children(error.map(|e| InlineMessage::new(Severity::Danger, e)))
            .child(field(
                "Open in",
                Select::new("open-in", choices).selected(open_in).on_change(move |value, _, cx| {
                    let value = value.to_string();
                    select.update(cx, |this, cx| {
                        this.edit_rules(cx, |rules, ix| {
                            rules[*ix].open_in = (value != SCHEME_DEFAULT).then_some(value);
                        })
                    })
                }),
            ))
            .child(div().h_px().bg(cx.theme().colors.border))
            .child(field("Try it", Input::new(&self.tester).clearable()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("route", "Open through Switchyard")
                            .icon(IconName::ExternalLink)
                            .variant(ButtonVariant::Outline)
                            .disabled(self.tester.read(cx).text().trim().is_empty())
                            .on_click(move |_, _, cx| {
                                let url = route.read(cx).tester.read(cx).text().trim().to_string();
                                cx.defer(move |cx| app::handle_url(&url, cx));
                            }),
                    )
                    .child(Caption::new(
                        "Runs every rule, asks for missing lookups and opens the result, as a real click would.",
                    )),
            )
            .child(trial)
            .child(div().h_px().bg(cx.theme().colors.border))
            .child(syntax_help(cx));

        ShellColumn::new("editor")
            .title(if rule.name.is_empty() { "Untitled".to_string() } else { rule.name.clone() })
            .action(
                IconButton::new("up", IconName::ArrowUp)
                    .tooltip("Move up (rules are tried top to bottom)")
                    .disabled(ix == 0)
                    .on_click(move |_, _, cx| up.update(cx, |this, cx| this.move_rule(true, cx))),
            )
            .action(
                IconButton::new("down", IconName::ArrowDown)
                    .tooltip("Move down")
                    .disabled(ix + 1 >= count)
                    .on_click(move |_, _, cx| down.update(cx, |this, cx| this.move_rule(false, cx))),
            )
            .action(
                IconButton::new("delete", IconName::Trash2)
                    .tooltip("Delete rule")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, _, cx| delete.update(cx, |this, cx| this.delete_rule(cx))),
            )
            .child(ScrollArea::new("editor-scroll").vertical().size_full().child(body))
    }

    /// Shows what the selected rule does to the link in the tester.
    fn trial(&self, rule: &Rule, state: &State, cx: &App) -> AnyElement {
        let text = self.tester.read(cx).text().trim().to_string();
        if text.is_empty() {
            return Caption::new("Paste a link above to see how this rule rewrites it.").into_any_element();
        }
        let Ok(compiled) = compile(rule) else {
            return Caption::new("Fix the rule to try it.").into_any_element();
        };
        let url = match Url::parse(&text) {
            Ok(url) => url,
            Err(e) => return InlineMessage::new(Severity::Warning, format!("Not a URL: {e}")).into_any_element(),
        };
        let Some(captures) = compiled.pattern.matches(&url) else {
            return InlineMessage::new(Severity::Info, "This rule does not match that link.").into_any_element();
        };
        let caps = div().flex().flex_wrap().gap_2().children(
            captures
                .iter()
                .map(|(k, v)| Code::new(format!("{k} = {v}"))),
        );
        let result = match &compiled.rewrite {
            None => Ok(url.to_string()),
            Some(template) => template.render(&Scope {
                url: &url,
                captures: &captures,
                lookup: &|t, k| state.lookups.get(t, k),
            }),
        };
        let outcome = match result {
            Ok(out) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Caption::new("Opens"))
                .child(mono(out, cx))
                .into_any_element(),
            Err(EvalError::Missing { table, key }) => InlineMessage::new(
                Severity::Warning,
                format!("#{table} has no “{key}” yet; you will be asked the first time you open such a link."),
            )
            .into_any_element(),
            Err(e) => InlineMessage::new(Severity::Danger, e.to_string()).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(InlineMessage::new(Severity::Success, "Matches"))
            .child(caps)
            .child(outcome)
            .into_any_element()
    }

    fn tables(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let entity = cx.entity();
        let rows = state.lookups.0.iter().map(|(name, entries)| {
            let entity = entity.clone();
            let name_ = name.clone();
            ListItem::new(SharedString::from(format!("table-{name}")), format!("#{name}"))
                .detail(entries.len().to_string())
                .leading(Icon::new(IconName::Table))
                .selected(self.table.as_ref() == Some(name))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.table = Some(name_.clone());
                        cx.notify();
                    })
                })
        });
        let add = cx.entity();
        ShellColumn::new("tables")
            .width(gpui::px(260.))
            .title("Tables")
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        ScrollArea::new("tables-scroll")
                            .vertical()
                            .flex_1()
                            .child(div().p_2().child(List::new().children(rows))),
                    )
                    .child(
                        div()
                            .p_2()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.new_table).size(ControlSize::Sm)))
                            .child(
                                IconButton::new("add-table", IconName::Plus)
                                    .tooltip("Add a table")
                                    .on_click(move |_, _, cx| add.update(cx, |this, cx| this.add_table(cx))),
                            ),
                    ),
            )
    }

    fn entries(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let Some((table, entries)) = self
            .table
            .as_ref()
            .and_then(|t| state.lookups.0.get_key_value(t))
        else {
            return ShellColumn::new("entries").child(div().p_6().child(Paragraph::new(
                "Tables fill themselves: when a rule looks up a key that is not there yet, \
                 Switchyard asks for the value and stores it here. Pick a table to edit it.",
            )));
        };
        let table = table.clone();
        let rows = entries.iter().enumerate().map(|(ix, (key, value))| {
            let (t, k) = (table.clone(), key.clone());
            let (fill, kk, vv) = (cx.entity(), key.clone(), value.clone());
            ListItem::new(("entry", ix), key.clone())
                .description(value.clone())
                .on_click(move |_, _, cx| {
                    let (k, v) = (kk.clone(), vv.clone());
                    fill.update(cx, |this, cx| {
                        this.new_key.update(cx, |i, cx| i.set_text(k, cx));
                        this.new_value.update(cx, |i, cx| i.set_text(v, cx));
                    })
                })
                .trailing(
                    IconButton::new(("remove", ix), IconName::Trash2)
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            app::state(cx).update(cx, |s, cx| {
                                s.lookups.remove(&t, &k);
                                s.save_lookups();
                                cx.notify();
                            })
                        }),
                )
        });
        let (add, drop, name) = (cx.entity(), cx.entity(), table.clone());
        ShellColumn::new("entries")
            .title(format!("#{table}"))
            .action(
                IconButton::new("drop-table", IconName::Trash2)
                    .tooltip("Delete this table")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, _, cx| {
                        app::state(cx).update(cx, |s, cx| {
                            s.lookups.remove_table(&name);
                            s.save_lookups();
                            cx.notify();
                        });
                        drop.update(cx, |this, cx| {
                            this.table = None;
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        ScrollArea::new("entries-scroll")
                            .vertical()
                            .flex_1()
                            .child(div().p_2().child(List::new().divided().children(rows))),
                    )
                    .child(
                        div()
                            .p_3()
                            .flex()
                            .items_end()
                            .gap_2()
                            .border_t_1()
                            .border_color(cx.theme().colors.border)
                            .child(div().flex_1().child(field("Key", Input::new(&self.new_key))))
                            .child(div().flex_1().child(field("Value", Input::new(&self.new_value))))
                            .child(
                                Button::new("add-entry", "Save")
                                    .icon(IconName::Check)
                                    .on_click(move |_, _, cx| add.update(cx, |this, cx| this.add_entry(cx))),
                            ),
                    ),
            )
    }

    fn recent(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let rows: Vec<_> = state
            .recent
            .iter()
            .enumerate()
            .map(|(ix, event)| {
                let ago = SystemTime::now()
                    .duration_since(event.at)
                    .map(|d| match d.as_secs() {
                        s if s < 60 => format!("{s}s ago"),
                        s if s < 3600 => format!("{}m ago", s / 60),
                        s => format!("{}h ago", s / 3600),
                    })
                    .unwrap_or_default();
                let (entity, url) = (cx.entity(), event.url.clone());
                ListItem::new(("event", ix), event.url.clone())
                    .description(event.result.clone())
                    .detail(ago)
                    .leading(Icon::new(if event.ok { IconName::Check } else { IconName::CircleAlert }))
                    .on_click(move |_, _, cx| {
                        let url = url.clone();
                        entity.update(cx, |this, cx| {
                            this.tester.update(cx, |t, cx| t.set_text(url, cx));
                            this.page = Page::Rules;
                            cx.notify();
                        })
                    })
            })
            .collect();
        let body = if rows.is_empty() {
            div()
                .p_6()
                .child(Paragraph::new(
                    "Links Switchyard routes show up here. Click one to try it against your rules.",
                ))
                .into_any_element()
        } else {
            div().p_2().child(List::new().divided().children(rows)).into_any_element()
        };
        ShellColumn::new("recent")
            .title("Recent links")
            .child(ScrollArea::new("recent-scroll").vertical().size_full().child(body))
    }

    fn general(&self, cx: &mut Context<Self>) -> ShellColumn {
        let state = app::state(cx).read(cx);
        let current = mac::default_browser();
        let is_default = current.is_some() && current.as_deref().map(str::to_lowercase)
            == state.self_id.as_deref().map(str::to_lowercase);
        let self_id = state.self_id.clone();
        let default_row = SettingsRow::new("Default browser")
            .description(match (&self_id, is_default) {
                (None, _) => "Running outside the app bundle. Install with scripts/bundle.sh to register Switchyard.".to_string(),
                (_, true) => "Switchyard receives every link you click.".to_string(),
                (_, false) => format!(
                    "Currently {}. Make Switchyard the default so it sees your links.",
                    current.as_deref().map(|id| state.browser_name(id).to_string()).unwrap_or_else(|| "unknown".into())
                ),
            })
            .control(if is_default {
                InlineMessage::new(Severity::Success, "Default").into_any_element()
            } else {
                Button::new("make-default", "Make default")
                    .primary()
                    .disabled(self_id.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(id) = &self_id
                            && let Err(e) = mac::set_default_browser(id)
                        {
                            log::error!("{e:#}");
                        }
                        app::state(cx).update(cx, |_, cx| cx.notify());
                    })
                    .into_any_element()
            });

        let browsers: Vec<Choice> = state
            .browsers
            .iter()
            .map(|b| Choice::new(b.id.clone(), b.name.clone()))
            .collect();
        let fallback = SettingsRow::new("Browser")
            .description("Opens every link no rule rewrites.")
            .control(if browsers.is_empty() {
                Caption::new("No browsers found").into_any_element()
            } else {
                let mut select = Select::new("browser", browsers)
                    .placeholder("Ask every time")
                    .on_change(|value, _, cx| {
                        let value = value.to_string();
                        update_config(cx, |c| c.browser = Some(value));
                    });
                if let Some(b) = &state.config.browser {
                    select = select.selected(b.clone());
                }
                select.into_any_element()
            });

        let mut segments = SegmentedControl::new(
            "modifier",
            format!("{:?}", state.config.picker_modifier).to_lowercase(),
        );
        for m in Modifier::ALL {
            segments = segments.segment(format!("{m:?}").to_lowercase(), m.label(), None);
        }
        let picker = SettingsRow::new("Show the browser picker")
            .description("Hold this key while clicking a link to choose where it opens.")
            .control(segments.on_change(|value, _, cx| {
                let m = Modifier::ALL
                    .into_iter()
                    .find(|m| format!("{m:?}").to_lowercase() == value.as_ref())
                    .unwrap_or_default();
                update_config(cx, |c| c.picker_modifier = m);
            }));

        let strip = SettingsRow::new("Remove tracking parameters")
            .description("Drops utm_*, fbclid, gclid and similar from web links.")
            .control(
                Switch::new("strip", state.config.strip_tracking)
                    .on_change(|on, _, cx| update_config(cx, |c| c.strip_tracking = on)),
            );

        let files = SettingsRow::new("Configuration")
            .description(config::dir().display().to_string())
            .control(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("reveal", "Show in Finder")
                            .variant(ButtonVariant::Outline)
                            .icon(IconName::FolderOpen)
                            .on_click(|_, _, _| mac::reveal(&config::config_path())),
                    )
                    .child(
                        Button::new("reload", "Reload")
                            .variant(ButtonVariant::Outline)
                            .icon(IconName::RefreshCw)
                            .on_click(|_, _, cx| {
                                app::state(cx).update(cx, |s, cx| {
                                    s.reload();
                                    cx.notify();
                                })
                            }),
                    ),
            );

        let problem = state.problem.clone();
        let extension_section = self.extension_section(cx);

        let body = div()
            .p_6()
            .flex()
            .flex_col()
            .gap_6()
            .children(problem.map(|p| InlineMessage::new(Severity::Danger, p)))
            .child(
                SettingsSection::new("Links")
                    .row(default_row)
                    .row(fallback)
                    .row(picker)
                    .row(strip),
            )
            .child(extension_section)
            .child(SettingsSection::new("Files").row(files));
        ShellColumn::new("general")
            .title("General")
            .child(ScrollArea::new("general-scroll").vertical().size_full().child(body))
    }
}

fn copy_extension_path(cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(extension::folder().display().to_string()));
}

fn step(n: usize, body: impl IntoElement, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(
            div()
                .flex_none()
                .size(px(20.))
                .rounded_full()
                .bg(cx.theme().colors.active)
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .child(n.to_string()),
        )
        .child(div().flex().flex_wrap().items_center().gap_1().child(body))
}

impl Settings {
    fn start_setup(&mut self, id: String, cx: &mut Context<Self>) {
        copy_extension_path(cx);
        self.setup_error = extension::set_up(&id).err().map(|e| format!("{e:#}"));
        self.setup = Some(id);
        cx.notify();
    }

    /// The steps left once the Extensions page is open and the path is on the clipboard.
    fn setup_steps(&self, id: &str, name: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let words = |text: &str| div().child(text.to_string());
        let strong = |text: &str| div().font_weight(gpui::FontWeight::SEMIBOLD).child(text.to_string());
        let again = id.to_string();
        div()
            .mb_3()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(theme.radius(Radius::Lg))
            .bg(theme.colors.sunken)
            .children(self.setup_error.clone().map(|e| InlineMessage::new(Severity::Danger, e)))
            .child(Caption::new(format!(
                "{name} is showing its Extensions page, and the extension's folder is on your clipboard."
            )))
            .child(step(
                1,
                div().flex().gap_1().child(words("Turn on")).child(strong("Developer mode")).child(words(
                    "at the top right of the page (in Edge, at the bottom left).",
                )),
                cx,
            ))
            .child(step(2, div().flex().gap_1().child(words("Click")).child(strong("Load unpacked")), cx))
            .child(step(
                3,
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(words("In the window that opens, press"))
                    .child(KbdCombo::new("cmd-shift-g cmd-v enter"))
                    .child(words("to go to the folder.")),
                cx,
            ))
            .child(step(4, div().flex().gap_1().child(words("Click")).child(strong("Select")).child(words(
                "and this row turns to Connected.",
            )), cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("setup-again", "Open Extensions page again")
                            .variant(ButtonVariant::Outline)
                            .on_click(cx.listener(move |this, _, _, cx| this.start_setup(again.clone(), cx))),
                    )
                    .child(
                        Button::new("setup-copy", "Copy folder path")
                            .variant(ButtonVariant::Outline)
                            .on_click(|_, _, cx| copy_extension_path(cx)),
                    )
                    .child(
                        Button::new("setup-cancel", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.setup = None;
                                this.setup_error = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn extension_section(&self, cx: &mut Context<Self>) -> SettingsSection {
        let connections = extension::connections();
        let browsers: Vec<app::Browser> = app::state(cx)
            .read(cx)
            .browsers
            .iter()
            .filter(|b| extension::supports(&b.id))
            .cloned()
            .collect();
        let mut section = SettingsSection::new("Browser extension").description(
            "Lets Chromium browsers hand links your rules send elsewhere straight to the right app, \
             instead of opening them in a tab. Safari and Firefox aren't supported yet.",
        );
        if browsers.is_empty() {
            section = section.row(
                SettingsRow::new("No Chromium browsers")
                    .description("Install Chrome, Arc, Dia, Brave, Edge or Vivaldi to use the extension."),
            );
        }

        for browser in &browsers {
            let connection = connections.iter().find(|c| c.browser.eq_ignore_ascii_case(&browser.id));
            let live = connection.is_some_and(|c| c.live());
            let setting_up = self.setup.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(&browser.id));
            let description = match connection {
                Some(c) if c.live() => format!("Connected, extension {}", c.version.as_deref().unwrap_or("?")),
                Some(c) => format!("Set up, not running. {}", last_seen(c)),
                None if setting_up => "Waiting for the extension to load…".to_string(),
                None => "Not set up".to_string(),
            };
            let id = browser.id.clone();
            let control = if live {
                InlineMessage::new(Severity::Success, "Connected").into_any_element()
            } else {
                let button = Button::new(SharedString::from(format!("setup-{id}")), "Set up")
                    .on_click(cx.listener(move |this, _, _, cx| this.start_setup(id.clone(), cx)));
                if setting_up { button.variant(ButtonVariant::Outline) } else { button.primary() }.into_any_element()
            };
            let icon = browser.icon.clone();
            section = section.row(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(icon.map(|icon| img(icon).flex_none().size(px(24.))))
                    .child(
                        div()
                            .flex_1()
                            .child(SettingsRow::new(browser.name.clone()).description(description).control(control)),
                    ),
            );
            if setting_up && !live {
                section = section.row(self.setup_steps(&browser.id, &browser.name, cx));
            } else if setting_up && live {
                section = section.row(
                    div().mb_3().child(InlineMessage::new(
                        Severity::Success,
                        format!("{} is connected. Links you click there now follow your rules.", browser.name),
                    )),
                );
            }
        }

        // Browsers that connected but aren't installed where Launch Services sees them.
        for c in connections
            .iter()
            .filter(|c| !browsers.iter().any(|b| b.id.eq_ignore_ascii_case(&c.browser)))
        {
            let mut row = SettingsRow::new(c.name.clone()).description(last_seen(c));
            if c.live() {
                row = row.description("Connected").control(InlineMessage::new(Severity::Success, "Connected"));
            }
            section = section.row(row);
        }

        let folder = extension::folder();
        section.row(
            SettingsRow::new("Extension folder")
                .description(format!(
                    "{}. After updating Switchyard, press reload on the extension in each browser.",
                    folder.display()
                ))
                .control(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("copy-extension", "Copy path")
                                .variant(ButtonVariant::Outline)
                                .on_click(|_, _, cx| copy_extension_path(cx)),
                        )
                        .child(
                            Button::new("reveal-extension", "Show in Finder")
                                .variant(ButtonVariant::Outline)
                                .icon(IconName::FolderOpen)
                                .on_click(move |_, _, _| mac::reveal(&folder.join("manifest.json"))),
                        ),
                ),
        )
    }
}

fn last_seen(c: &extension::Connection) -> String {
    match c.age().as_secs() {
        ..120 => "Last seen a minute ago".to_string(),
        ago @ ..7200 => format!("Last seen {} minutes ago", ago / 60),
        ago @ ..172_800 => format!("Last seen {} hours ago", ago / 3600),
        ago => format!("Last seen {} days ago", ago / 86_400),
    }
}

fn syntax_help(cx: &App) -> impl IntoElement {
    let rows: [(&str, &str); 10] = [
        ("(NAME)", "captures one host label or path segment"),
        ("(NAME...)", "captures the rest of the path"),
        ("* / **", "match within a segment / across segments"),
        ("**.example.com", "the domain and any subdomain"),
        ("?v=(ID)", "requires a query parameter, in any order"),
        ("(lookup #Table KEY)", "a value from a lookup table; asks once if missing"),
        ("(query \"name\")", "a query parameter of the original link"),
        ("(encode X) (decode X)", "percent-encode or decode"),
        ("(lower X) (upper X)", "change case"),
        ("(or A B)", "the first that is not empty; (url) (host) (path) also work"),
    ];
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(Label::new("Syntax"))
        .children(rows.into_iter().map(|(code, what)| {
            div()
                .flex()
                .gap_3()
                .child(div().w(gpui::px(170.)).flex_none().child(mono(code, cx)))
                .child(Caption::new(what))
        }))
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let (bg, fg) = (colors.bg, colors.fg);
        let mut shell = ColumnShell::new("settings").column(self.nav(cx));
        shell = match self.page {
            Page::Rules => shell.column(self.rule_list(cx)).column(self.rule_editor(cx)),
            Page::Lookups => shell.column(self.tables(cx)).column(self.entries(cx)),
            Page::Recent => shell.column(self.recent(cx)),
            Page::General => shell.column(self.general(cx)),
        };
        FocusScope::new(&self.focus)
            .root()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(shell)
    }
}
