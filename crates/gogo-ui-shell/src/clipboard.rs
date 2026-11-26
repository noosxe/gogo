//! Reading the system clipboard's text, when the application asks (design 24 §3):
//! smithay-clipboard on Wayland (`wl_data_device`, which every compositor has), arboard
//! on X11 and macOS. Nothing is ever written to it.

use winit::raw_window_handle::{HasDisplayHandle as _, RawDisplayHandle};
use winit::window::Window;
use zeroize::Zeroizing;

/// The longest text handed to the application.
pub const CLIPBOARD_MAX: usize = 4096;

pub(crate) enum Clipboard {
    #[cfg(target_os = "linux")]
    Wayland(smithay_clipboard::Clipboard),
    Other(arboard::Clipboard),
}

impl Clipboard {
    /// The clipboard of `window`'s display. It must be dropped before the window.
    pub(crate) fn open(window: &Window) -> Option<Clipboard> {
        let display = window.display_handle().ok()?.as_raw();
        #[cfg(target_os = "linux")]
        if let RawDisplayHandle::Wayland(handle) = display {
            return Some(Clipboard::Wayland(wayland(handle.display.as_ptr())));
        }
        let _: RawDisplayHandle = display;
        arboard::Clipboard::new().ok().map(Clipboard::Other)
    }

    /// The text on the clipboard; `None` when there is none, or more than
    /// [`CLIPBOARD_MAX`] bytes of it.
    pub(crate) fn text(&mut self) -> Option<Zeroizing<String>> {
        let text = match self {
            #[cfg(target_os = "linux")]
            Clipboard::Wayland(clipboard) => clipboard.load().ok()?,
            Clipboard::Other(clipboard) => clipboard.get_text().ok()?,
        };
        bounded(Zeroizing::new(text))
    }
}

fn bounded(text: Zeroizing<String>) -> Option<Zeroizing<String>> {
    (!text.is_empty() && text.len() <= CLIPBOARD_MAX).then_some(text)
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn wayland(display: *mut std::ffi::c_void) -> smithay_clipboard::Clipboard {
    // SAFETY: `display` is the `wl_display` of the window's live connection, taken from
    // its display handle just now. The returned clipboard is kept only in `run::Open`,
    // in a field declared before the window, so it is dropped while the window and the
    // event loop's connection are still alive.
    unsafe { smithay_clipboard::Clipboard::new(display) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_text_is_handed_on() {
        let text = |s: &str| bounded(Zeroizing::new(s.to_string()));
        assert!(text("a").is_some());
        assert!(text("").is_none());
        assert!(text(&"a".repeat(CLIPBOARD_MAX + 1)).is_none());
    }
}
