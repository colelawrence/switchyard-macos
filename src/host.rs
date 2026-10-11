//! Native messaging host: the browser starts `switchyard chrome-extension://…/` and talks
//! to it over stdin and stdout, each message a native-endian length and then JSON.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::extension::{self, Connection};
use crate::mac;

type Out = Arc<Mutex<std::io::Stdout>>;

fn read(input: &mut impl Read) -> Option<Value> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_ne_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write(out: &Out, message: &Value) {
    let body = serde_json::to_vec(message).expect("JSON values serialize");
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    let sent = out
        .write_all(&(body.len() as u32).to_ne_bytes())
        .and_then(|()| out.write_all(&body))
        .and_then(|()| out.flush());
    if sent.is_err() {
        // The browser went away.
        std::process::exit(0);
    }
}

/// The app bundle containing `path`, outermost first, like `/Applications/Dia.app`.
fn bundle_of(path: &str) -> Option<&str> {
    path.find(".app/").map(|i| &path[..i + 4])
}

/// The browser that started us: our parent process, or the app it belongs to.
fn calling_browser() -> (String, String) {
    let ppid = std::os::unix::process::parent_id();
    let path = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &ppid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let Some(bundle) = bundle_of(&path) else {
        return (path.clone(), path);
    };
    let name = bundle.rsplit('/').next().unwrap_or(bundle).trim_end_matches(".app");
    let id = mac::bundle_id_at(bundle).unwrap_or_else(|| bundle.to_string());
    (id, name.to_string())
}

fn index_message(kind: &str, browser: &str) -> Value {
    let self_id = mac::self_bundle_id();
    let (config, error) = match Config::load() {
        Ok((config, _)) => (config, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    let index = extension::index(&config, self_id.as_deref(), browser);
    json!({
        "type": kind,
        "version": env!("CARGO_PKG_VERSION"),
        "index": index,
        "error": error,
    })
}

/// Hands a link to the Switchyard app, which routes it like any other click.
fn open(url: &str) {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = exe.to_string_lossy();
    // `-g` keeps Switchyard in the background; the app it routes to comes forward itself.
    let mut open = std::process::Command::new("/usr/bin/open");
    open.arg("-g");
    match bundle_of(&exe) {
        Some(app) => open.args(["-a", app]),
        None => open.args(["-b", mac::self_bundle_id().as_deref().unwrap_or("dev.switchyard.Switchyard")]),
    };
    match open.arg(url).status() {
        Ok(status) if status.success() => {}
        Ok(status) => log::error!("open {url} exited with {status}"),
        Err(e) => log::error!("could not run open: {e}"),
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub fn run() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let (browser, name) = calling_browser();
    log::info!("extension connected from {name} ({browser})");

    let mut hello = index_message("hello", &browser);
    hello["browser"] = json!({ "id": browser, "name": name });
    write(&out, &hello);

    let connection = Arc::new(Mutex::new(Connection {
        browser: browser.clone(),
        name,
        seen: 0,
        version: None,
    }));

    // Resend the index whenever the rules change, and check in for the settings window.
    {
        let (out, connection, browser) = (out.clone(), connection.clone(), browser.clone());
        std::thread::spawn(move || {
            let path = config::config_path();
            let mut last = modified(&path);
            loop {
                if let Err(e) = extension::save_connection(&mut connection.lock().unwrap()) {
                    log::warn!("could not record the connection: {e:#}");
                }
                for _ in 0..5 {
                    std::thread::sleep(Duration::from_secs(1));
                    let now = modified(&path);
                    if now != last {
                        last = now;
                        write(&out, &index_message("index", &browser));
                    }
                }
            }
        });
    }

    let mut input = std::io::stdin().lock();
    while let Some(message) = read(&mut input) {
        let text = |key: &str| message[key].as_str().unwrap_or_default().to_string();
        match message["type"].as_str() {
            Some("hello") => {
                let mut connection = connection.lock().unwrap();
                connection.version = Some(text("version"));
                let _ = extension::save_connection(&mut connection);
            }
            Some("open") => open(&text("url")),
            Some("check") => write(&out, &json!({
                "type": "check",
                "id": message["id"],
                "leave": !extension::recently_handed(&text("url")),
            })),
            other => log::warn!("unknown message from the extension: {other:?}"),
        }
    }
}
