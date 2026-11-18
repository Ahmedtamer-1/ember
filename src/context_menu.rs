/// Returns the page initialization script that installs Ember's custom right-click context menu.
///
/// The custom menu is only shown when right-clicking a link (**Open link in new tab**,
/// **Copy link**, **Copy** when text is selected, **Reload**, and **Inspect** in debug builds).
/// Everywhere else (plain text, selections outside links, images, media, canvas, editable
/// fields) the event is left alone so the native WebView context menu appears with its full
/// set of options (Copy, Save image, etc.). Holding Shift always shows the native menu.
pub fn context_menu_init_script() -> String {
    let show_inspect = if cfg!(debug_assertions) {
        "true"
    } else {
        "false"
    };

    format!(
        r#"
(function () {{
  var SHOW_INSPECT = {show_inspect};
  var menuEl = null;

  function removeMenu() {{
    if (menuEl && menuEl.parentNode) {{
      menuEl.parentNode.removeChild(menuEl);
    }}
    menuEl = null;
  }}

  function findEnclosingLink(target) {{
    var el = target;
    while (el && el !== document.documentElement) {{
      if (el.tagName && el.tagName.toUpperCase() === 'A' && el.href) {{
        return String(el.href);
      }}
      el = el.parentElement;
    }}
    return '';
  }}

  function isNativeMenuTarget(target) {{
    if (!target) return true;
    if (target.isContentEditable) return true;
    var tag = target.tagName ? target.tagName.toUpperCase() : '';
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' ||
      tag === 'IMG' || tag === 'VIDEO' || tag === 'AUDIO' || tag === 'CANVAS' ||
      tag === 'PICTURE' || tag === 'SVG';
  }}

  function selectedText() {{
    try {{
      var sel = window.getSelection ? window.getSelection() : null;
      return sel ? String(sel.toString()) : '';
    }} catch (_) {{
      return '';
    }}
  }}

  function post(msg) {{
    if (window.ipc && typeof window.ipc.postMessage === 'function') {{
      window.ipc.postMessage(msg);
    }}
  }}

  function copyText(text) {{
    var copied = false;
    try {{
      copied = document.execCommand && document.execCommand('copy');
    }} catch (_) {{
      copied = false;
    }}
    if (!copied && navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {{
      navigator.clipboard.writeText(text).catch(function () {{}});
    }}
  }}

  function createMenuItem(label, shortcut, onClick) {{
    var btn = document.createElement('div');
    btn.style.cssText =
      'display:flex;align-items:center;justify-content:space-between;gap:24px;' +
      'padding:7px 12px;border-radius:6px;cursor:pointer;font-size:13px;user-select:none;';
    var left = document.createElement('span');
    left.textContent = label;
    btn.appendChild(left);
    if (shortcut) {{
      var right = document.createElement('span');
      right.textContent = shortcut;
      right.style.cssText = 'font-size:11px;opacity:0.6;';
      btn.appendChild(right);
    }}
    btn.addEventListener('mouseenter', function () {{
      btn.style.background = 'rgba(128, 128, 128, 0.18)';
    }});
    btn.addEventListener('mouseleave', function () {{
      btn.style.background = 'transparent';
    }});
    btn.addEventListener('click', function (e) {{
      e.preventDefault();
      e.stopPropagation();
      removeMenu();
      onClick();
    }});
    return btn;
  }}

  function createDivider() {{
    var div = document.createElement('div');
    div.style.cssText = 'height:1px;margin:4px 0;background:rgba(128,128,128,0.22);';
    return div;
  }}

  window.addEventListener('contextmenu', function (e) {{
    removeMenu();
    if (e.shiftKey || isNativeMenuTarget(e.target)) {{
      return;
    }}
    var href = findEnclosingLink(e.target);
    if (!href) {{
      // Not a link: let the native WebView menu (Copy, Select all, etc.) appear.
      return;
    }}
    e.preventDefault();

    var selText = selectedText();
    var isDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;

    var menu = document.createElement('div');
    menu.id = '__ember-ctx-menu';
    menu.style.cssText =
      'position:fixed;z-index:2147483647;min-width:210px;padding:5px;border-radius:9px;' +
      'font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;' +
      'box-shadow:0 8px 28px rgba(0,0,0,0.28);' +
      (isDark
        ? 'background:#27272a;color:#f4f4f5;border:1px solid #3f3f46;'
        : 'background:#ffffff;color:#0f172a;border:1px solid #e2e8f0;');
    // Keep the page selection intact while interacting with the menu so "Copy" works.
    menu.addEventListener('mousedown', function (ev) {{
      ev.preventDefault();
      ev.stopPropagation();
    }});

    menu.appendChild(
      createMenuItem('Open link in new tab', '', function () {{
        post('ctx_open_new_tab:' + href);
      }})
    );
    menu.appendChild(
      createMenuItem('Copy link', '', function () {{
        if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {{
          navigator.clipboard.writeText(href).catch(function () {{}});
        }}
        post('ctx_copy_link:' + href);
      }})
    );
    if (selText) {{
      menu.appendChild(
        createMenuItem('Copy', 'Ctrl+C', function () {{
          copyText(selText);
        }})
      );
    }}

    menu.appendChild(createDivider());
    menu.appendChild(
      createMenuItem('Reload', 'Ctrl+R', function () {{
        post('shortcut:reload');
      }})
    );

    if (SHOW_INSPECT) {{
      menu.appendChild(createDivider());
      menu.appendChild(
        createMenuItem('Inspect', '', function () {{
          post('ctx_inspect:');
        }})
      );
    }}

    (document.body || document.documentElement).appendChild(menu);
    menuEl = menu;

    var rect = menu.getBoundingClientRect();
    var x = e.clientX;
    var y = e.clientY;
    if (x + rect.width > window.innerWidth - 8) {{
      x = Math.max(8, window.innerWidth - rect.width - 8);
    }}
    if (y + rect.height > window.innerHeight - 8) {{
      y = Math.max(8, window.innerHeight - rect.height - 8);
    }}
    menu.style.left = x + 'px';
    menu.style.top = y + 'px';
  }}, true);

  window.addEventListener('mousedown', function (e) {{
    // Clicks inside the menu must not tear it down before the item's click handler runs.
    if (menuEl && e.target && menuEl.contains(e.target)) return;
    removeMenu();
  }}, true);
  window.addEventListener('scroll', function () {{
    removeMenu();
  }}, true);
  window.addEventListener('blur', function () {{
    removeMenu();
  }});
  window.addEventListener('keydown', function (e) {{
    if (e.key === 'Escape') removeMenu();
  }}, true);
}})();
"#
    )
}

/// Copies UTF-8 text to the OS clipboard, using a window owned by this process (the active
/// or foreground window) as the clipboard owner when one is available.
///
/// Returns `true` on success.
pub fn copy_text_to_clipboard(text: &str) -> bool {
    platform::copy_text_to_clipboard_with_owner(platform::default_owner_window(), text)
}

/// Copies UTF-8 text to the OS clipboard with an explicit owner window handle (`HWND` on
/// Windows; `0` lets the platform pick a fallback owner). Returns `true` on success.
#[allow(dead_code)]
pub fn copy_text_to_clipboard_with_owner(owner_hwnd: isize, text: &str) -> bool {
    let owner = if owner_hwnd == 0 {
        platform::default_owner_window()
    } else {
        owner_hwnd
    };
    platform::copy_text_to_clipboard_with_owner(owner, text)
}

#[cfg(target_os = "windows")]
mod platform {
    use core::ffi::c_void;
    use std::ptr;

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    const OPEN_ATTEMPTS: u32 = 5;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h_wnd_new_owner: isize) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(u_format: u32, h_mem: *mut c_void) -> *mut c_void;
        fn CloseClipboard() -> i32;
        fn GetActiveWindow() -> isize;
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(h_wnd: isize, lpdw_process_id: *mut u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> *mut c_void;
        fn GlobalLock(h_mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h_mem: *mut c_void) -> i32;
        fn GlobalFree(h_mem: *mut c_void) -> *mut c_void;
        fn GetCurrentProcessId() -> u32;
    }

    /// Finds a top-level window belonging to this process to own the clipboard.
    pub fn default_owner_window() -> isize {
        // SAFETY: These Win32 calls take no pointers we own (other than the out-param, which
        // points to a valid local) and are safe to call from any thread.
        unsafe {
            let active = GetActiveWindow();
            if active != 0 {
                return active;
            }
            let fg = GetForegroundWindow();
            if fg != 0 {
                let mut pid: u32 = 0;
                let _ = GetWindowThreadProcessId(fg, &mut pid);
                if pid == GetCurrentProcessId() {
                    return fg;
                }
            }
        }
        0
    }

    pub fn copy_text_to_clipboard_with_owner(owner_hwnd: isize, text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * std::mem::size_of::<u16>();

        // SAFETY: Standard Win32 clipboard sequence with a `GMEM_MOVEABLE` UTF-16 buffer. The
        // memory handle is only freed by us if the system did not take ownership of it.
        unsafe {
            let mut opened = false;
            for attempt in 0..OPEN_ATTEMPTS {
                if OpenClipboard(owner_hwnd) != 0 {
                    opened = true;
                    break;
                }
                if attempt + 1 < OPEN_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            if !opened {
                return false;
            }

            let h_mem = GlobalAlloc(GMEM_MOVEABLE, byte_len);
            if h_mem.is_null() {
                let _ = CloseClipboard();
                return false;
            }
            let locked = GlobalLock(h_mem) as *mut u16;
            if locked.is_null() {
                let _ = GlobalFree(h_mem);
                let _ = CloseClipboard();
                return false;
            }
            ptr::copy_nonoverlapping(wide.as_ptr(), locked, wide.len());
            let _ = GlobalUnlock(h_mem);

            if EmptyClipboard() == 0 {
                let _ = GlobalFree(h_mem);
                let _ = CloseClipboard();
                return false;
            }
            let ok = !SetClipboardData(CF_UNICODETEXT, h_mem).is_null();
            if !ok {
                // Ownership was not transferred to the system; release the buffer.
                let _ = GlobalFree(h_mem);
            }
            let _ = CloseClipboard();
            ok
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn default_owner_window() -> isize {
        0
    }

    pub fn copy_text_to_clipboard_with_owner(_owner_hwnd: isize, _text: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_menu_script_only_overrides_links() {
        let script = context_menu_init_script();
        assert!(!script.contains("Ctrl+Shift+I"));
        // preventDefault must only happen after the link check, never unconditionally.
        let handler_start = script
            .find("window.addEventListener('contextmenu'")
            .expect("contextmenu handler present");
        let handler = &script[handler_start..];
        let link_check = handler.find("if (!href)").expect("link check present");
        let prevent = handler
            .find("e.preventDefault()")
            .expect("preventDefault present for link menu");
        assert!(link_check < prevent);
        assert!(script.contains("'IMG'"));
        assert!(script.contains("'VIDEO'"));
        assert!(script.contains("'CANVAS'"));
        assert!(script.contains("e.shiftKey"));
        assert!(script.contains("Copy link"));
        assert!(script.contains("Open link in new tab"));
        assert!(script.contains("getSelection"));
        assert!(script.contains("execCommand('copy')"));
        // Clicks within the menu must not dismiss it before the item handler runs.
        assert!(script.contains("menuEl.contains(e.target)"));
        // Rendered braces must be single (format! escapes collapsed).
        assert!(!script.contains("{{"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_clipboard_stub_reports_failure() {
        assert!(!copy_text_to_clipboard("x"));
        assert!(!copy_text_to_clipboard_with_owner(0, "x"));
    }
}
