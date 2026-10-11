pub mod picker;
pub mod prompt;
pub mod settings;

use std::ops::Range;

use ely_gpui_component::forms::Highlight;
use ely_gpui_component::theme::ActiveTheme;
use gpui::{
    App, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, px, size,
};

/// Options for a small window centered on the active screen.
pub fn window_options(title: &str, width: f32, height: f32, kind: WindowKind, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(title.to_string())),
            appears_transparent: true,
            ..Default::default()
        }),
        kind,
        focus: true,
        show: true,
        ..Default::default()
    }
}

/// Colors the moving parts of a match pattern or rewrite template:
/// `(NAMES)` and calls, `#Tables`, `"strings"` and globs.
pub fn highlight_rule(text: &str, cx: &App) -> Vec<(Range<usize>, Highlight)> {
    let colors = &cx.theme().colors;
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        match c {
            b'\\' => {
                i += 2;
                continue;
            }
            b'(' | b')' => {
                if c == b'(' {
                    depth += 1;
                } else {
                    depth = depth.saturating_sub(1);
                }
                out.push((start..i + 1, Highlight::new(colors.accent)));
            }
            b'*' if depth == 0 => {
                while i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    i += 1;
                }
                out.push((start..i + 1, Highlight::new(colors.warning)));
            }
            b'#' if depth > 0 => {
                while i + 1 < bytes.len() && (bytes[i + 1].is_ascii_alphanumeric() || bytes[i + 1] == b'_') {
                    i += 1;
                }
                out.push((start..i + 1, Highlight::new(colors.info)));
            }
            b'"' if depth > 0 => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                let end = (i + 1).min(bytes.len());
                out.push((start..end, Highlight::new(colors.success)));
            }
            _ if depth > 0 && (c.is_ascii_alphanumeric() || c == b'_' || c == b'.') => {
                while i + 1 < bytes.len()
                    && (bytes[i + 1].is_ascii_alphanumeric() || b"_.".contains(&bytes[i + 1]))
                {
                    i += 1;
                }
                out.push((start..i + 1, Highlight::new(colors.accent)));
            }
            _ => {}
        }
        i += 1;
    }
    out
}
