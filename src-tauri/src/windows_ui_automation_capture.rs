//! Windows UI Automation focused-element capture boundary.
//!
//! UI Automation metadata is preferred over screenshots because it can expose
//! semantic control information without retaining visual assets. The provider
//! must treat missing patterns and inaccessible/elevated applications as
//! normal outcomes, not capture failures.

use crate::local_sqlite_event_database::RawEvent;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FocusedElementSnapshot {
    pub automation_id: Option<String>,
    pub control_type: Option<String>,
    pub element_name: Option<String>,
    pub element_value: Option<String>,
    pub class_name: Option<String>,
    pub framework_id: Option<String>,
    pub bounds_json: Option<String>,
    pub selected_text: Option<String>,
}

impl FocusedElementSnapshot {
    pub fn sanitize(mut self) -> Self {
        self.element_name = self
            .element_name
            .map(|value| value.chars().take(512).collect());
        self.element_value = self
            .element_value
            .map(|value| value.chars().take(4096).collect());
        self.selected_text = self
            .selected_text
            .map(|value| value.chars().take(4096).collect());
        if self
            .control_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("password"))
        {
            self.element_value = None;
            self.selected_text = None;
        }
        self
    }
}

pub trait UiAutomationProvider: Send {
    fn is_available(&self) -> bool;
    fn focused_element(&self) -> Result<Option<FocusedElementSnapshot>, String>;
}

pub struct WindowsUiAutomationProvider;
impl UiAutomationProvider for WindowsUiAutomationProvider {
    fn is_available(&self) -> bool {
        cfg!(windows)
    }
    fn focused_element(&self) -> Result<Option<FocusedElementSnapshot>, String> {
        #[cfg(windows)]
        {
            use windows::core::PCWSTR;
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED,
            };
            use windows::Win32::UI::Accessibility::CUIAutomation;
            use windows::Win32::UI::Shell::SHCoCreateInstance;
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            if !initialized {
                return Ok(None);
            }
            struct ComGuard;
            impl Drop for ComGuard {
                fn drop(&mut self) {
                    unsafe {
                        CoUninitialize();
                    }
                }
            }
            let _com_guard = ComGuard;
            let automation: windows::Win32::UI::Accessibility::IUIAutomation =
                unsafe { SHCoCreateInstance(PCWSTR::null(), Some(&CUIAutomation), None) }
                    .map_err(|error| error.to_string())?;
            let element =
                unsafe { automation.GetFocusedElement() }.map_err(|error| error.to_string())?;
            let name = unsafe { element.CurrentName() }
                .ok()
                .map(|value| value.to_string());
            let automation_id = unsafe { element.CurrentAutomationId() }
                .ok()
                .map(|value| value.to_string());
            let class_name = unsafe { element.CurrentClassName() }
                .ok()
                .map(|value| value.to_string());
            let framework_id = unsafe { element.CurrentFrameworkId() }
                .ok()
                .map(|value| value.to_string());
            let control_type = unsafe { element.CurrentLocalizedControlType() }
                .ok()
                .map(|value| value.to_string());
            let bounds = unsafe { element.CurrentBoundingRectangle() }.ok().map(|value| serde_json::json!({ "left": value.left, "top": value.top, "right": value.right, "bottom": value.bottom }).to_string());
            let selected_text = unsafe { read_selected_text(&element) };
            return Ok(Some(
                FocusedElementSnapshot {
                    automation_id,
                    control_type,
                    element_name: name,
                    element_value: None,
                    class_name,
                    framework_id,
                    bounds_json: bounds,
                    selected_text,
                }
                .sanitize(),
            ));
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }
}

#[cfg(windows)]
unsafe fn read_selected_text(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::System::Ole::{
        SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
    };
    use windows::Win32::UI::Accessibility::{ITextProvider, ITextRangeProvider, UIA_TextPatternId};
    let provider: ITextProvider = element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
    let array = provider.GetSelection().ok()?;
    if array.is_null() {
        return None;
    }
    let lower = SafeArrayGetLBound(array, 1).ok()?;
    let upper = SafeArrayGetUBound(array, 1).ok()?;
    let mut selected = None;
    for index in lower..=upper {
        let mut raw: *mut core::ffi::c_void = core::ptr::null_mut();
        if SafeArrayGetElement(array, &index, &mut raw as *mut _ as *mut core::ffi::c_void).is_ok()
            && !raw.is_null()
        {
            let range = ITextRangeProvider::from_raw(raw);
            if let Ok(text) = range.GetText(4096) {
                let value = text.to_string();
                if !value.trim().is_empty() {
                    selected = Some(value);
                    break;
                }
            }
        }
    }
    let _ = SafeArrayDestroy(array);
    selected
}

pub fn normalize_focused_element(snapshot: FocusedElementSnapshot) -> RawEvent {
    let snapshot = snapshot.sanitize();
    let protected = snapshot
        .control_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("password"));
    RawEvent { id: Uuid::new_v4().to_string(), timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or_default(), event_type: "element_focused".into(), source: "windows_ui_automation".into(), app_name: None, executable_path: None, process_id: None, window_handle: None, window_title: None, element_name: snapshot.element_name, text: snapshot.selected_text, file_path: None, metadata_json: serde_json::json!({ "automation_id": snapshot.automation_id, "control_type": snapshot.control_type, "element_value": snapshot.element_value, "class_name": snapshot.class_name, "framework_id": snapshot.framework_id, "bounds": snapshot.bounds_json }).to_string(), privacy_class: if protected { "protected_field".into() } else { "ui_automation_metadata".into() }, confidence: 1.0, created_at: Utc::now().to_rfc3339() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_element_metadata_without_screenshot() {
        let event = normalize_focused_element(FocusedElementSnapshot {
            element_name: Some("Save".into()),
            control_type: Some("Button".into()),
            ..Default::default()
        });
        assert_eq!(event.event_type, "element_focused");
        assert_eq!(event.source, "windows_ui_automation");
        assert!(event.metadata_json.contains("Button"));
    }

    #[test]
    fn bounds_selected_text_and_control_values() {
        let snapshot = FocusedElementSnapshot {
            selected_text: Some("x".repeat(5000)),
            element_value: Some("y".repeat(5000)),
            ..Default::default()
        }
        .sanitize();
        assert_eq!(snapshot.selected_text.unwrap().len(), 4096);
        assert_eq!(snapshot.element_value.unwrap().len(), 4096);
    }

    #[test]
    fn password_controls_never_retain_values() {
        let event = normalize_focused_element(FocusedElementSnapshot {
            control_type: Some("PasswordBox".into()),
            element_value: Some("secret".into()),
            selected_text: Some("secret".into()),
            ..Default::default()
        });
        assert_eq!(event.privacy_class, "protected_field");
        assert!(event.text.is_none());
        assert!(!event.metadata_json.contains("secret"));
    }

    #[test]
    fn unavailable_or_inaccessible_focus_returns_empty_result() {
        let provider = WindowsUiAutomationProvider;
        assert!(provider.focused_element().is_ok());
    }
}
