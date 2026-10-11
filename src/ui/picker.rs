//! Lets the user choose a browser for one link.

use ely_gpui_component::lists::{List, ListItem};
use ely_gpui_component::primitives::{FocusScope, Icon, IconName};
use ely_gpui_component::theme::ActiveTheme;
use ely_gpui_component::typography::{Caption, Kbd, MiddleEllipsis};
use gpui::{
    App, AppContext as _, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, Styled, Subscription, Window, WindowKind, div, img, px,
};

use crate::app;

pub fn open(url: String, cx: &mut App) {
    let count = app::state(cx).read(cx).browsers.len().max(1);
    let height = 120. + 44. * count.min(9) as f32;
    let options = super::window_options("Open with", 420., height, WindowKind::Floating, cx);
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            // Clicking elsewhere dismisses the picker, as with a menu.
            let blur = cx.observe_window_activation(window, |_: &mut Picker, window, _| {
                if !window.is_window_active() {
                    window.remove_window();
                }
            });
            Picker {
                focus,
                url,
                _blur: blur,
            }
        })
    });
    if let Err(e) = opened {
        log::error!("could not open the picker: {e:#}");
    }
    cx.activate(true);
}

struct Picker {
    focus: FocusHandle,
    url: String,
    _blur: Subscription,
}

fn pick(url: String, id: String, window: &mut Window, cx: &mut App) {
    window.remove_window();
    cx.defer(move |cx| app::open_picked(&url, &id, cx));
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let state = app::state(cx).read(cx);
        let ids: Vec<String> = state.browsers.iter().map(|b| b.id.clone()).collect();
        let url = self.url.clone();

        let rows = state.browsers.iter().enumerate().map(|(ix, browser)| {
            let leading = match &browser.icon {
                Some(icon) => img(icon.clone()).size(px(24.)).into_any_element(),
                None => Icon::new(IconName::Globe).into_any_element(),
            };
            let (url, id) = (self.url.clone(), browser.id.clone());
            let mut item = ListItem::new(("browser", ix), browser.name.clone())
                .leading(leading)
                .on_click(move |_, window, cx| pick(url.clone(), id.clone(), window, cx));
            if ix < 9 {
                item = item.trailing(Kbd::new(&(ix + 1).to_string()));
            }
            item
        });

        div().size_full().on_key_down(cx.listener(move |_, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" {
                    window.remove_window();
                } else if let Ok(n) = key.parse::<usize>()
                    && (1..=ids.len()).contains(&n)
                {
                    pick(url.clone(), ids[n - 1].clone(), window, cx);
                }
            }))
            .child(
        FocusScope::new(&self.focus)
            .root()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pt_8()
            .pb_4()
            .bg(colors.bg)
            .text_color(colors.fg)
            .child(MiddleEllipsis::new(self.url.clone()))
            .child(if state.browsers.is_empty() {
                Caption::new("No browsers found.").into_any_element()
            } else {
                List::new().children(rows).into_any_element()
            })
            .child(div().flex_1())
            .child(Caption::new("Press a number to choose, Esc to cancel.")))
    }
}
