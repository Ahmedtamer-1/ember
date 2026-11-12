use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use wry::WebView;

/// Supported browser permission types prompted by Ember.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionKind {
    Camera,
    Microphone,
    Location,
}

impl PermissionKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Microphone => "Microphone",
            Self::Location => "Location",
        }
    }
}

/// Manages per-origin permission decisions for Camera, Microphone, and Location.
#[derive(Debug, Clone, Default)]
pub struct PermissionManager {
    decisions: Arc<RwLock<HashMap<(String, PermissionKind), bool>>>,
}

impl PermissionManager {
    pub fn new() -> Self {
        Self {
            decisions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Looks up a previously remembered decision (`Some(true)` = Allow, `Some(false)` = Deny).
    pub fn get_decision(&self, origin: &str, kind: PermissionKind) -> Option<bool> {
        let key = (normalize_origin(origin), kind);
        self.decisions
            .read()
            .ok()
            .and_then(|map| map.get(&key).copied())
    }

    /// Records a user's Allow/Deny decision for `(origin, kind)`.
    pub fn set_decision(&self, origin: &str, kind: PermissionKind, allow: bool) {
        let key = (normalize_origin(origin), kind);
        if let Ok(mut map) = self.decisions.write() {
            map.insert(key, allow);
        }
    }
}

fn normalize_origin(uri: &str) -> String {
    let trimmed = uri.trim().to_ascii_lowercase();
    if let Some(scheme_idx) = trimmed.find("://") {
        let after = &trimmed[scheme_idx + 3..];
        let host_end = after.find(['/', '?', '#']).unwrap_or(after.len());
        return trimmed[..scheme_idx + 3 + host_end].to_string();
    }
    trimmed
}

/// Attaches the native WebView2 `PermissionRequested` handler for Camera, Microphone, and Location.
pub fn attach_webview_permissions(webview: &WebView, manager: Arc<PermissionManager>) {
    platform::attach_webview_permissions(webview, manager);
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{ptr, sync::Arc};

    use webview2_com::{
        take_pwstr,
        Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
            COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
            COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
        },
        PermissionRequestedEventHandler,
    };
    use windows_core::PWSTR;
    use wry::{WebView, WebViewExtWindows};

    use super::{normalize_origin, PermissionKind, PermissionManager};

    const MB_YESNO: u32 = 0x0000_0004;
    const MB_ICONQUESTION: u32 = 0x0000_0020;
    const IDYES: i32 = 6;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            h_wnd: *mut core::ffi::c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
    }

    pub fn attach_webview_permissions(webview: &WebView, manager: Arc<PermissionManager>) {
        let controller = webview.controller();
        // SAFETY: Retrieves the active `ICoreWebView2` COM interface from the controller.
        let Ok(core_webview) = (unsafe { controller.CoreWebView2() }) else {
            return;
        };

        let handler = PermissionRequestedEventHandler::create(Box::new(move |_webview, args| {
            let Some(args) = args else {
                return Ok(());
            };

            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            // SAFETY: Reads the requested permission kind from the event args.
            if unsafe { args.PermissionKind(&mut kind) }.is_err() {
                return Ok(());
            }

            let mapped_kind = if kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA {
                Some(PermissionKind::Camera)
            } else if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
                Some(PermissionKind::Microphone)
            } else if kind == COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION {
                Some(PermissionKind::Location)
            } else {
                None
            };

            let Some(perm_kind) = mapped_kind else {
                return Ok(());
            };

            let mut uri_pwstr = PWSTR::null();
            // SAFETY: Retrieves the requesting URI string from WebView2.
            let uri = if unsafe { args.Uri(&mut uri_pwstr) }.is_ok() {
                take_pwstr(uri_pwstr)
            } else {
                String::from("this site")
            };
            let origin = normalize_origin(&uri);

            let allowed = match manager.get_decision(&origin, perm_kind) {
                Some(decision) => decision,
                None => {
                    let decision = prompt_permission_dialog(&origin, perm_kind);
                    manager.set_decision(&origin, perm_kind, decision);
                    decision
                }
            };

            let state = if allowed {
                COREWEBVIEW2_PERMISSION_STATE_ALLOW
            } else {
                COREWEBVIEW2_PERMISSION_STATE_DENY
            };
            // SAFETY: Sets the permission state on the WebView2 event args.
            let _ = unsafe { args.SetState(state) };
            Ok(())
        }));

        let mut token: i64 = 0;
        // SAFETY: Registers the permission handler on the WebView2 instance.
        unsafe {
            let _ = core_webview.add_PermissionRequested(&handler, &mut token);
        }
    }

    fn prompt_permission_dialog(origin: &str, kind: PermissionKind) -> bool {
        let message = format!(
            "{} wants to access your {}.\n\nClick Yes to Allow, or No to Block.",
            origin,
            kind.label()
        );
        let caption = "Ember — Permission Request";
        let msg_w: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        let cap_w: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: Standard Win32 modal prompt with null-terminated UTF-16 buffers.
        let result = unsafe {
            MessageBoxW(
                ptr::null_mut(),
                msg_w.as_ptr(),
                cap_w.as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        result == IDYES
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::PermissionManager;
    use std::sync::Arc;
    use wry::WebView;

    pub fn attach_webview_permissions(_webview: &WebView, _manager: Arc<PermissionManager>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_manager_decisions() {
        let mgr = PermissionManager::new();
        assert_eq!(
            mgr.get_decision("https://meet.example.com/room", PermissionKind::Camera),
            None
        );

        mgr.set_decision(
            "https://meet.example.com/room",
            PermissionKind::Camera,
            true,
        );
        assert_eq!(
            mgr.get_decision("https://meet.example.com/other", PermissionKind::Camera),
            Some(true)
        );
        assert_eq!(
            mgr.get_decision("https://meet.example.com/other", PermissionKind::Microphone),
            None
        );
    }
}
