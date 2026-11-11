//! System clipboard integration using [`arboard`].

/// Copy text to the system clipboard.
///
/// Returns `true` on success. On platforms or environments without a clipboard
/// provider (e.g. headless Linux, CI, SSH without X11 forwarding), returns `false`
/// without crashing or emitting fatal errors.
pub fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text).is_ok(),
        Err(_) => false,
    }
}
