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

/// The user's answer to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChoice {
    /// Grant access and remember it for this origin for the rest of the session.
    AllowForSession,
    /// Deny this one request only; the site will be asked again next time.
    BlockOnce,
}

/// Manages per-origin permission decisions for Camera, Microphone, and Location.
///
/// Only "Allow" answers from the prompt are remembered (for the session). Denials are not cached,
/// so a mistaken "No" never sticks: the next request simply prompts again.
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

    /// Records an explicit Allow/Deny decision for `(origin, kind)`.
    pub fn set_decision(&self, origin: &str, kind: PermissionKind, allow: bool) {
        let key = (normalize_origin(origin), kind);
        if let Ok(mut map) = self.decisions.write() {
            map.insert(key, allow);
        }
    }

    /// Forgets every remembered decision (e.g. from a future "Reset site permissions" action).
    pub fn clear_decisions(&self) {
        if let Ok(mut map) = self.decisions.write() {
            map.clear();
        }
    }

    /// Resolves a permission request: returns a remembered decision if there is one, otherwise
    /// calls `prompt` and caches the answer only when the user allowed access.
    pub fn resolve<F>(&self, origin: &str, kind: PermissionKind, prompt: F) -> bool
    where
        F: FnOnce() -> PromptChoice,
    {
        if let Some(decision) = self.get_decision(origin, kind) {
            return decision;
        }
        match prompt() {
            PromptChoice::AllowForSession => {
                self.set_decision(origin, kind, true);
                true
            }
            PromptChoice::BlockOnce => false,
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

/// Builds the text shown in the permission prompt.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn prompt_message(origin: &str, kind: PermissionKind) -> String {
    format!(
        "{} wants to access your {}.\n\n\
         Yes: Allow (remembered until Ember is closed)\n\
         No: Block this time (you will be asked again)",
        origin,
        kind.label()
    )
}

/// Attaches the native WebView2 `PermissionRequested` handler for Camera, Microphone, and Location.
pub fn attach_webview_permissions(webview: &WebView, manager: Arc<PermissionManager>) {
    platform::attach_webview_permissions(webview, manager);
}

#[cfg(target_os = "windows")]
mod platform {
    use std::sync::Arc;

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

    use super::{
        normalize_origin, prompt_message, PermissionKind, PermissionManager, PromptChoice,
    };

    const MB_YESNO: u32 = 0x0000_0004;
    const MB_ICONQUESTION: u32 = 0x0000_0020;
    const MB_DEFBUTTON2: u32 = 0x0000_0100;
    const MB_TASKMODAL: u32 = 0x0000_2000;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    const IDYES: i32 = 6;
    const GA_ROOT: u32 = 2;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            h_wnd: *mut core::ffi::c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
        fn GetAncestor(hwnd: *mut core::ffi::c_void, ga_flags: u32) -> *mut core::ffi::c_void;
    }

    pub fn attach_webview_permissions(webview: &WebView, manager: Arc<PermissionManager>) {
        let controller = webview.controller();
        // SAFETY: Retrieves the active `ICoreWebView2` COM interface from the controller.
        let Ok(core_webview) = (unsafe { controller.CoreWebView2() }) else {
            return;
        };

        // Resolve Ember's top-level window once so the prompt is owned by it (stays in front of
        // the browser and disables it while open). Stored as `isize` so the closure stays `Send`.
        let mut parent = Default::default();
        // SAFETY: Reads the controller's parent HWND into a local out-parameter.
        let owner_hwnd: isize = if unsafe { controller.ParentWindow(&mut parent) }.is_ok() {
            let raw = parent.0;
            // SAFETY: `GetAncestor` accepts any HWND (returns null on failure).
            let root = unsafe { GetAncestor(raw, GA_ROOT) };
            if root.is_null() {
                raw as isize
            } else {
                root as isize
            }
        } else {
            0
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

            let allowed = manager.resolve(&origin, perm_kind, || {
                prompt_permission_dialog(owner_hwnd, &origin, perm_kind)
            });

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

    /// Shows the permission prompt.
    ///
    /// This runs synchronously inside the WebView2 `PermissionRequested` callback on the UI
    /// thread. `MessageBoxW` pumps a nested modal message loop while it is open; that is safe
    /// here because the box is owned by Ember's top-level window (`owner_hwnd`) and
    /// `MB_TASKMODAL` disables the thread's other top-level windows, so the user cannot interact
    /// with tabs or close the browser underneath it. A fully asynchronous prompt would need
    /// `args.GetDeferral()` plus an in-app UI surface, which is left for future work.
    fn prompt_permission_dialog(
        owner_hwnd: isize,
        origin: &str,
        kind: PermissionKind,
    ) -> PromptChoice {
        let message = prompt_message(origin, kind);
        let caption = "Ember — Permission Request";
        let msg_w: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        let cap_w: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: `owner_hwnd` is Ember's window handle (or null); text buffers are
        // null-terminated UTF-16 and outlive the call.
        let result = unsafe {
            MessageBoxW(
                owner_hwnd as *mut core::ffi::c_void,
                msg_w.as_ptr(),
                cap_w.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TASKMODAL | MB_SETFOREGROUND,
            )
        };
        if result == IDYES {
            PromptChoice::AllowForSession
        } else {
            PromptChoice::BlockOnce
        }
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
    use std::cell::Cell;

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

    #[test]
    fn test_resolve_caches_allow_but_not_block() {
        let mgr = PermissionManager::new();
        let prompts = Cell::new(0);
        let origin = "https://meet.example.com/room";

        // "No" blocks this request only and is not remembered.
        let allowed = mgr.resolve(origin, PermissionKind::Microphone, || {
            prompts.set(prompts.get() + 1);
            PromptChoice::BlockOnce
        });
        assert!(!allowed);
        assert_eq!(mgr.get_decision(origin, PermissionKind::Microphone), None);

        // Asked again next time; "Yes" is remembered for the session.
        let allowed = mgr.resolve(origin, PermissionKind::Microphone, || {
            prompts.set(prompts.get() + 1);
            PromptChoice::AllowForSession
        });
        assert!(allowed);
        assert_eq!(prompts.get(), 2);

        // Remembered allow: no further prompt.
        let allowed = mgr.resolve(
            "https://meet.example.com/another",
            PermissionKind::Microphone,
            || {
                prompts.set(prompts.get() + 1);
                PromptChoice::BlockOnce
            },
        );
        assert!(allowed);
        assert_eq!(prompts.get(), 2);
    }

    #[test]
    fn test_clear_decisions_forgets_everything() {
        let mgr = PermissionManager::new();
        mgr.set_decision("https://a.example.com", PermissionKind::Camera, true);
        mgr.set_decision("https://b.example.com", PermissionKind::Location, false);
        mgr.clear_decisions();
        assert_eq!(
            mgr.get_decision("https://a.example.com", PermissionKind::Camera),
            None
        );
        assert_eq!(
            mgr.get_decision("https://b.example.com", PermissionKind::Location),
            None
        );

        // Shared clones observe the reset too.
        let clone = mgr.clone();
        mgr.set_decision("https://a.example.com", PermissionKind::Camera, true);
        clone.clear_decisions();
        assert_eq!(
            mgr.get_decision("https://a.example.com", PermissionKind::Camera),
            None
        );
    }

    #[test]
    fn test_prompt_message_explains_choices() {
        let msg = prompt_message("https://meet.example.com", PermissionKind::Camera);
        assert!(msg.contains("https://meet.example.com wants to access your Camera"));
        assert!(msg.contains("Yes: Allow"));
        assert!(msg.contains("asked again"));
    }
}
