//! Switchyard: a default browser that routes links by rule.

mod app;
mod config;
mod extension;
mod host;
mod mac;
mod pattern;
mod router;
mod template;
mod ui;

use std::time::Duration;

use ely_gpui_component::shell::{TrayIcon, TrayItem};
use futures::StreamExt as _;
use gpui::{App, AsyncApp};

struct Tray(#[allow(dead_code)] TrayIcon);

impl gpui::Global for Tray {}

/// `switchyard test <url>` prints what would happen to a link, without opening it.
fn test(url: &str) {
    let (config, _) = config::Config::load().unwrap_or_default();
    let lookups = config::Lookups::load().unwrap_or_default();
    let self_id = mac::self_bundle_id();
    let router = router::Router {
        config: &config,
        lookups: &lookups,
        self_id: self_id.as_deref(),
    };
    println!("{:#?}", router.route(url, false));
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    // Started by a browser as the extension's native messaging host.
    if args.first().is_some_and(|a| a.starts_with("chrome-extension://") || a == "--native-host") {
        return host::run();
    }
    if let [cmd, url] = args.as_slice()
        && cmd == "test"
    {
        return test(url);
    }
    let launch_urls: Vec<String> = args.into_iter().filter(|a| a.contains(':')).collect();

    let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
    for url in launch_urls {
        let _ = tx.unbounded_send(url);
    }

    let application = gpui_platform::application().with_assets(ely_gpui_component::Assets);
    let urls = tx.clone();
    application.on_open_urls(move |list| {
        for url in list {
            let _ = urls.unbounded_send(url);
        }
    });
    application.on_reopen(ui::settings::show);

    application.run(move |cx: &mut App| {
        if let Err(e) = ely_gpui_component::init(cx) {
            log::error!("could not initialise the UI: {e:#}");
        }
        app::init(cx);

        let tray = TrayIcon::new(
            "arrow.triangle.branch",
            vec![
                TrayItem::Action("Open Link from Clipboard".into()),
                TrayItem::Action("Settings…".into()),
                TrayItem::Separator,
                TrayItem::Action("Quit Switchyard".into()),
            ],
            |ix, cx| match ix {
                0 => match cx.read_from_clipboard().and_then(|c| c.text()) {
                    Some(text) => app::handle_url(text.trim(), cx),
                    None => log::warn!("the clipboard has no text"),
                },
                1 => ui::settings::show(cx),
                _ => cx.quit(),
            },
            cx,
        );
        match tray {
            Ok(tray) => cx.set_global(Tray(tray)),
            Err(e) => log::warn!("no menu bar icon: {e:#}"),
        }

        cx.spawn(async move |cx: &mut AsyncApp| {
            // A launch caused by a click delivers its link right away; a launch
            // by hand delivers none, so show the settings instead.
            let first = futures::select! {
                url = rx.next() => url,
                _ = futures::FutureExt::fuse(cx.background_executor().timer(Duration::from_millis(400))) => None,
            };
            match first {
                Some(url) => cx.update(|cx| app::handle_url(&url, cx)),
                None => cx.update(ui::settings::show),
            }
            while let Some(url) = rx.next().await {
                cx.update(|cx| app::handle_url(&url, cx));
            }
        })
        .detach();
    });
}
