//! Asks for a lookup value the first time a rule needs it.

use ely_gpui_component::buttons::{Button, ButtonVariant};
use ely_gpui_component::forms::{Input, InputEvent, TextInput};
use ely_gpui_component::primitives::FocusScope;
use ely_gpui_component::theme::ActiveTheme;
use ely_gpui_component::typography::{Caption, Code, Heading, MiddleEllipsis, Paragraph};
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, Styled, Subscription, Window, WindowKind, div,
};

use crate::app;

pub struct Ask {
    pub table: String,
    pub key: String,
    pub rule: String,
    pub url: String,
}

pub fn open(ask: Ask, cx: &mut App) {
    let options = super::window_options("Switchyard", 520., 330., WindowKind::Floating, cx);
    let opened = cx.open_window(options, |window, cx| cx.new(|cx| Prompt::new(ask, window, cx)));
    if let Err(e) = opened {
        log::error!("could not open the lookup prompt: {e:#}");
    }
    cx.activate(true);
}

struct Prompt {
    focus: FocusHandle,
    ask: Ask,
    input: Entity<TextInput>,
    _submit: Subscription,
}

impl Prompt {
    fn new(ask: Ask, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = format!("Value for {}", ask.key);
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder(placeholder));
        let submit = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::Submit => this.save(window, cx),
            InputEvent::Changed => cx.notify(),
            _ => {}
        });
        let focus = cx.focus_handle();
        input.update(cx, |input, cx| {
            let handle = gpui::Focusable::focus_handle(input, cx);
            window.focus(&handle, cx);
        });
        Self {
            focus,
            ask,
            input,
            _submit: submit,
        }
    }

    fn value(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.value(cx);
        if value.is_empty() {
            return;
        }
        let Ask { table, key, url, .. } = &self.ask;
        let (table, key, url) = (table.clone(), key.clone(), url.clone());
        window.remove_window();
        cx.defer(move |cx| app::supply(&table, &key, &value, &url, cx));
    }

    fn open_original(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.ask.url.clone();
        window.remove_window();
        cx.defer(move |cx| app::open_in_browser(&url, cx));
    }
}

impl Render for Prompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let Ask { table, key, rule, url } = &self.ask;
        let empty = self.value(cx).is_empty();
        let this = cx.entity();
        let (save, original) = (this.clone(), this.clone());

        div().size_full().on_key_down(cx.listener(|_, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
        FocusScope::new(&self.focus)
            .root()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .px_6()
            .pt_10()
            .pb_5()
            .bg(colors.bg)
            .text_color(colors.fg)
            .child(Heading::h4(format!("What is “{key}” in #{table}?")))
            .child(Paragraph::new(format!(
                "The rule “{rule}” looks up “{key}” in #{table} to rewrite this link, \
                 and there is no entry yet. Switchyard will remember it."
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(Caption::new("Link"))
                    .child(div().min_w_0().flex_1().child(MiddleEllipsis::new(url.clone()))),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Caption::new("Edit later under Lookup tables →"))
                    .child(Code::new(format!("#{table}"))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("original", "Open original link")
                            .variant(ButtonVariant::Outline)
                            .on_click(move |_, window, cx| {
                                original.update(cx, |p, cx| p.open_original(window, cx))
                            }),
                    )
                    .child(
                        Button::new("save", "Save and open")
                            .primary()
                            .disabled(empty)
                            .on_click(move |_, window, cx| {
                                save.update(cx, |p, cx| p.save(window, cx))
                            }),
                    ),
            ))
    }
}
