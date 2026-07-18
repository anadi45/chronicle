//! Windows low-level mouse hook boundary.
//!
//! The OS callback is intentionally tiny: it copies the event data into a
//! channel and immediately returns. Database writes happen on the hook worker
//! thread so the Windows input callback never blocks the desktop or other
//! applications.

#[cfg(windows)]
use super::normalize_mouse_event;
#[cfg(windows)]
use crate::local_sqlite_event_database::Database;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{mpsc, Arc, Mutex, OnceLock};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
#[derive(Clone, Copy)]
struct MouseMessage {
    event_type: &'static str,
    x: i32,
    y: i32,
    button: Option<&'static str>,
}

#[cfg(windows)]
static MOUSE_SENDER: OnceLock<Mutex<Option<mpsc::Sender<MouseMessage>>>> = OnceLock::new();

#[cfg(windows)]
pub fn start_mouse_hook(
    database: Arc<Mutex<Database>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (sender, receiver) = mpsc::channel();
        let _ = MOUSE_SENDER.set(Mutex::new(Some(sender)));
        let hook = unsafe { install_hook() };
        while !stop.load(Ordering::Relaxed) {
            if let Ok(message) = receiver.recv_timeout(Duration::from_millis(100)) {
                let event = normalize_mouse_event(
                    message.event_type,
                    message.x,
                    message.y,
                    message.button,
                    None,
                );
                if let Ok(database) = database.lock() {
                    let _ = database.insert_event(&event);
                }
            }
        }
        if !hook.is_invalid() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(hook);
            }
        }
        if let Some(slot) = MOUSE_SENDER.get() {
            if let Ok(mut sender) = slot.lock() {
                *sender = None;
            }
        }
    })
}

#[cfg(windows)]
unsafe fn install_hook() -> windows::Win32::UI::WindowsAndMessaging::HHOOK {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_MOUSE_LL};
    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_callback), None, 0).unwrap_or_default()
}

#[cfg(windows)]
unsafe extern "system" fn mouse_callback(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::CallNextHookEx;
    use windows::Win32::UI::WindowsAndMessaging::{
        MSLLHOOKSTRUCT, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    };
    if code >= 0 && lparam.0 != 0 {
        let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let (event_type, button) = match wparam.0 as u32 {
            WM_LBUTTONDOWN => ("mouse_click", Some("left")),
            WM_RBUTTONDOWN => ("mouse_right_click", Some("right")),
            WM_MBUTTONDOWN => ("mouse_click", Some("middle")),
            WM_MOUSEWHEEL => ("mouse_scroll", None),
            _ => ("mouse_input", None),
        };
        if let Some(slot) = MOUSE_SENDER.get() {
            if let Ok(sender) = slot.lock() {
                if let Some(sender) = sender.as_ref() {
                    let _ = sender.send(MouseMessage {
                        event_type,
                        x: data.pt.x,
                        y: data.pt.y,
                        button,
                    });
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
