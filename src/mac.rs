//! The few AppKit and LaunchServices calls Switchyard needs.
#![allow(deprecated, unexpected_cfgs)]

use std::ffi::CStr;
use std::os::raw::c_char;

use anyhow::{Result, anyhow};
use cocoa::base::{BOOL, NO, YES, id, nil};
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use objc::{class, msg_send, sel, sel_impl};

// CFStringRef and CFArrayRef are toll-free bridged to NSString and NSArray.
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: id) -> id;
    fn LSCopyAllHandlersForURLScheme(scheme: id) -> id;
    fn LSSetDefaultHandlerForURLScheme(scheme: id, bundle_id: id) -> i32;
    fn LSSetDefaultRoleHandlerForContentType(content_type: id, role: u32, bundle_id: id) -> i32;
}

const ROLE_VIEWER: u32 = 0x2;
const PNG: usize = 4;

fn ns(s: &str) -> id {
    unsafe {
        let s = NSString::alloc(nil).init_str(s);
        msg_send![s, autorelease]
    }
}

fn string(obj: id) -> Option<String> {
    if obj == nil {
        return None;
    }
    unsafe {
        let ptr: *const c_char = msg_send![obj, UTF8String];
        (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

fn workspace() -> id {
    unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] }
}

fn ns_url(url: &str) -> Option<id> {
    let obj: id = unsafe { msg_send![class!(NSURL), URLWithString: ns(url)] };
    (obj != nil).then_some(obj)
}

/// Our bundle id, or `None` when running outside an app bundle (e.g. via `cargo run`).
pub fn self_bundle_id() -> Option<String> {
    unsafe {
        let bundle: id = msg_send![class!(NSBundle), mainBundle];
        string(msg_send![bundle, bundleIdentifier])
    }
}

/// The bundle id of the app at `path`, such as `/Applications/Safari.app`.
pub fn bundle_id_at(path: &str) -> Option<String> {
    unsafe {
        let bundle: id = msg_send![class!(NSBundle), bundleWithPath: ns(path)];
        if bundle == nil {
            return None;
        }
        string(msg_send![bundle, bundleIdentifier])
    }
}

pub fn default_browser() -> Option<String> {
    unsafe {
        let handler = LSCopyDefaultHandlerForURLScheme(ns("https"));
        let id = string(handler);
        if handler != nil {
            let _: () = msg_send![handler, release];
        }
        id
    }
}

/// Asks macOS to make `bundle_id` the default browser. macOS confirms with the user.
pub fn set_default_browser(bundle_id: &str) -> Result<()> {
    let target = ns(bundle_id);
    for scheme in ["http", "https"] {
        let status = unsafe { LSSetDefaultHandlerForURLScheme(ns(scheme), target) };
        if status != 0 {
            return Err(anyhow!("LaunchServices refused ({status}) for {scheme}"));
        }
    }
    unsafe {
        LSSetDefaultRoleHandlerForContentType(ns("public.html"), ROLE_VIEWER, target);
    }
    Ok(())
}

/// Bundle ids of every installed app that opens https links.
pub fn browsers() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    unsafe {
        let array = LSCopyAllHandlersForURLScheme(ns("https"));
        if array == nil {
            return out;
        }
        let count: usize = msg_send![array, count];
        for i in 0..count {
            let item: id = msg_send![array, objectAtIndex: i];
            if let Some(id) = string(item)
                && !out.iter().any(|o| o.eq_ignore_ascii_case(&id))
                && app_path(&id).is_some()
            {
                out.push(id);
            }
        }
        let _: () = msg_send![array, release];
    }
    out
}

fn app_url(bundle_id: &str) -> Option<id> {
    let url: id =
        unsafe { msg_send![workspace(), URLForApplicationWithBundleIdentifier: ns(bundle_id)] };
    (url != nil).then_some(url)
}

pub fn app_path(bundle_id: &str) -> Option<String> {
    app_url(bundle_id).and_then(|url| string(unsafe { msg_send![url, path] }))
}

/// The name Finder shows for the app, like "Safari".
pub fn app_name(bundle_id: &str) -> String {
    let Some(path) = app_path(bundle_id) else {
        return bundle_id.to_string();
    };
    let name = unsafe {
        let fm: id = msg_send![class!(NSFileManager), defaultManager];
        string(msg_send![fm, displayNameAtPath: ns(&path)])
    };
    let name = name.unwrap_or(path);
    name.strip_suffix(".app").unwrap_or(&name).to_string()
}

/// The app's icon as PNG bytes, rendered at `size` points (2x pixels).
pub fn app_icon_png(bundle_id: &str, size: f64) -> Option<Vec<u8>> {
    let path = app_path(bundle_id)?;
    unsafe {
        let image: id = msg_send![workspace(), iconForFile: ns(&path)];
        if image == nil {
            return None;
        }
        let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(size * 2., size * 2.));
        let cg: *mut std::ffi::c_void = msg_send![image,
            CGImageForProposedRect: &mut rect as *mut NSRect
            context: nil
            hints: nil];
        if cg.is_null() {
            return None;
        }
        let rep: id = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: id = msg_send![rep, initWithCGImage: cg];
        let rep: id = msg_send![rep, autorelease];
        let props: id = msg_send![class!(NSDictionary), dictionary];
        let data: id = msg_send![rep, representationUsingType: PNG properties: props];
        if data == nil {
            return None;
        }
        let len: usize = msg_send![data, length];
        let bytes: *const u8 = msg_send![data, bytes];
        Some(std::slice::from_raw_parts(bytes, len).to_vec())
    }
}

/// Whether some installed app handles this URL's scheme.
pub fn can_open(url: &str) -> bool {
    let Some(url) = ns_url(url) else {
        return false;
    };
    let app: id = unsafe { msg_send![workspace(), URLForApplicationToOpenURL: url] };
    app != nil
}

/// Opens the URL with whatever app handles its scheme.
pub fn open(url: &str) -> Result<()> {
    let obj = ns_url(url).ok_or_else(|| anyhow!("`{url}` is not a URL macOS accepts"))?;
    if !can_open(url) {
        let scheme = url.split(':').next().unwrap_or(url);
        return Err(anyhow!("no installed app opens {scheme}: links"));
    }
    let ok: BOOL = unsafe { msg_send![workspace(), openURL: obj] };
    if ok == NO {
        return Err(anyhow!("macOS could not open `{url}`"));
    }
    Ok(())
}

/// Opens the URL in a specific app.
pub fn open_with(url: &str, bundle_id: &str) -> Result<()> {
    let obj = ns_url(url).ok_or_else(|| anyhow!("`{url}` is not a URL macOS accepts"))?;
    let app = app_url(bundle_id).ok_or_else(|| anyhow!("{bundle_id} is not installed"))?;
    unsafe {
        let urls: id = msg_send![class!(NSArray), arrayWithObject: obj];
        let config: id = msg_send![class!(NSWorkspaceOpenConfiguration), configuration];
        let _: () = msg_send![config, setActivates: YES];
        let _: () = msg_send![workspace(),
            openURLs: urls
            withApplicationAtURL: app
            configuration: config
            completionHandler: nil];
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

/// The modifier keys held right now.
pub fn modifiers() -> Modifiers {
    let flags: usize = unsafe { msg_send![class!(NSEvent), modifierFlags] };
    Modifiers {
        shift: flags & (1 << 17) != 0,
        control: flags & (1 << 18) != 0,
        option: flags & (1 << 19) != 0,
        command: flags & (1 << 20) != 0,
    }
}

/// Shows the file or folder in Finder.
pub fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
}
