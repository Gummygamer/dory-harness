//! Routes llama.cpp / ggml log output into `tracing`.

use std::ffi::{CStr, c_char, c_void};

use rillwright_sys::ggml_log_level as Level;

pub(crate) unsafe extern "C" fn callback(level: Level, text: *const c_char, _user: *mut c_void) {
    if text.is_null() {
        return;
    }
    let msg = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let msg = msg.trim_end();
    if msg.is_empty() {
        return;
    }
    // ggml levels: 1 DEBUG, 2 INFO, 3 WARN, 4 ERROR, 5 CONT (continuation of the previous line).
    match level {
        4 => tracing::error!(target: "llama", "{msg}"),
        3 => tracing::warn!(target: "llama", "{msg}"),
        2 => tracing::info!(target: "llama", "{msg}"),
        1 => tracing::debug!(target: "llama", "{msg}"),
        _ => tracing::trace!(target: "llama", "{msg}"),
    }
}
