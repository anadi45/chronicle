//! Windows low-level mouse hook boundary.
//!
//! The OS callback is intentionally tiny: it copies the event data into a
//! channel and immediately returns. Database writes happen on the hook worker
//! thread so the Windows input callback never blocks the desktop or other
//! applications.

#[cfg(windows)]
use super::normalize_keyboard_event;
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
use std::time::{Duration, Instant};

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
static KEYBOARD_SENDER: OnceLock<Mutex<Option<mpsc::Sender<u32>>>> = OnceLock::new();

#[cfg(windows)]
pub fn start_keyboard_hook(
    database: Arc<Mutex<Database>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (sender, receiver) = mpsc::channel();
        let _ = KEYBOARD_SENDER.set(Mutex::new(Some(sender)));
        let hook = unsafe { install_keyboard_hook() };
        while !stop.load(Ordering::Relaxed) {
            pump_window_messages();
            if let Ok(key_code) = receiver.recv_timeout(Duration::from_millis(100)) {
                let event = normalize_keyboard_event("key_down", key_code, None, None);
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
        if let Some(slot) = KEYBOARD_SENDER.get() {
            if let Ok(mut sender) = slot.lock() {
                *sender = None;
            }
        }
    })
}

#[cfg(windows)]
unsafe fn install_keyboard_hook() -> windows::Win32::UI::WindowsAndMessaging::HHOOK {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_KEYBOARD_LL};
    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_callback), None, 0).unwrap_or_default()
}

#[cfg(windows)]
unsafe extern "system" fn keyboard_callback(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_SYSKEYDOWN,
    };
    if code >= 0 && lparam.0 != 0 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let data = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if let Some(slot) = KEYBOARD_SENDER.get() {
            if let Ok(sender) = slot.lock() {
                if let Some(sender) = sender.as_ref() {
                    let _ = sender.send(data.vkCode);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(windows)]
pub fn start_mouse_hook(
    database: Arc<Mutex<Database>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (sender, receiver) = mpsc::channel();
        let _ = MOUSE_SENDER.set(Mutex::new(Some(sender)));
        let hook = unsafe { install_hook() };
        let mut left_down: Option<(i32, i32)> = None;
        let mut last_left_click: Option<(Instant, i32, i32)> = None;
        while !stop.load(Ordering::Relaxed) {
            pump_window_messages();
            if let Ok(message) = receiver.recv_timeout(Duration::from_millis(100)) {
                let mut event_type = message.event_type;
                if message.event_type == "mouse_click" {
                    if let Some((time, old_x, old_y)) = last_left_click {
                        if time.elapsed() <= Duration::from_millis(500)
                            && (old_x - message.x).abs() <= 4
                            && (old_y - message.y).abs() <= 4
                        {
                            event_type = "mouse_double_click";
                        }
                    }
                    last_left_click = Some((Instant::now(), message.x, message.y));
                    left_down = Some((message.x, message.y));
                } else if message.event_type == "mouse_move" {
                    if let Some((down_x, down_y)) = left_down {
                        if (down_x - message.x).abs() > 4 || (down_y - message.y).abs() > 4 {
                            event_type = "mouse_drag_started";
                        }
                    }
                } else if message.event_type == "mouse_left_up" && left_down.take().is_some() {
                    event_type = "mouse_drag_ended";
                }
                let event =
                    normalize_mouse_event(event_type, message.x, message.y, message.button, None);
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
fn pump_window_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let mut message = MSG::default();
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
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
        MSLLHOOKSTRUCT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
        WM_RBUTTONDOWN, WM_RBUTTONUP,
    };
    if code >= 0 && lparam.0 != 0 {
        let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let (event_type, button) = match wparam.0 as u32 {
            WM_LBUTTONDOWN => ("mouse_click", Some("left")),
            WM_RBUTTONDOWN => ("mouse_right_click", Some("right")),
            WM_MBUTTONDOWN => ("mouse_click", Some("middle")),
            WM_LBUTTONUP => ("mouse_left_up", Some("left")),
            WM_RBUTTONUP => ("mouse_right_up", Some("right")),
            WM_MOUSEMOVE => ("mouse_move", None),
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
