/// Returns the page initialization script that installs Ember's custom right-click context menu
/// (**Open link in new tab**, **Copy link**, and **Inspect** in debug builds).
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
  var currentHref = '';

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

  function post(msg) {{
    if (window.ipc && typeof window.ipc.postMessage === 'function') {{
      window.ipc.postMessage(msg);
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
    btn.addEventListener('mousedown', function (e) {{
      e.stopPropagation();
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
    var tag = e.target && e.target.tagName ? e.target.tagName.toUpperCase() : '';
    if (tag === 'INPUT' || tag === 'TEXTAREA' || (e.target && e.target.isContentEditable)) {{
      return;
    }}
    e.preventDefault();
    removeMenu();

    currentHref = findEnclosingLink(e.target);
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

    var hasItems = false;
    if (currentHref) {{
      var hrefCopy = currentHref;
      menu.appendChild(
        createMenuItem('Open link in new tab', '', function () {{
          post('ctx_open_new_tab:' + hrefCopy);
        }})
      );
      menu.appendChild(
        createMenuItem('Copy link', '', function () {{
          if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {{
            navigator.clipboard.writeText(hrefCopy).catch(function () {{}});
          }}
          post('ctx_copy_link:' + hrefCopy);
        }})
      );
      hasItems = true;
    }}

    if (hasItems) {{
      menu.appendChild(createDivider());
    }}

    menu.appendChild(
      createMenuItem('Reload', 'Ctrl+R', function () {{
        post('shortcut:reload');
      }})
    );

    if (SHOW_INSPECT) {{
      menu.appendChild(createDivider());
      menu.appendChild(
        createMenuItem('Inspect', 'Ctrl+Shift+I', function () {{
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

  window.addEventListener('mousedown', function () {{
    removeMenu();
  }}, true);
  window.addEventListener('scroll', function () {{
    removeMenu();
  }}, true);
  window.addEventListener('keydown', function (e) {{
    if (e.key === 'Escape') removeMenu();
  }}, true);
}})();
"#
    )
}

/// Copies UTF-8 text to the OS clipboard.
pub fn copy_text_to_clipboard(text: &str) {
    platform::copy_text_to_clipboard(text);
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ptr;

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h_wnd_new_owner: *mut core::ffi::c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(u_format: u32, h_mem: *mut core::ffi::c_void)
            -> *mut core::ffi::c_void;
        fn CloseClipboard() -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> *mut core::ffi::c_void;
        fn GlobalLock(h_mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(h_mem: *mut core::ffi::c_void) -> i32;
    }

    pub fn copy_text_to_clipboard(text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * std::mem::size_of::<u16>();

        // SAFETY: Standard Win32 clipboard sequence with `GMEM_MOVEABLE` UTF-16 buffer.
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return;
            }
            let _ = EmptyClipboard();
            let h_mem = GlobalAlloc(GMEM_MOVEABLE, byte_len);
            if !h_mem.is_null() {
                let locked = GlobalLock(h_mem) as *mut u16;
                if !locked.is_null() {
                    ptr::copy_nonoverlapping(wide.as_ptr(), locked, wide.len());
                    let _ = GlobalUnlock(h_mem);
                    let _ = SetClipboardData(CF_UNICODETEXT, h_mem);
                }
            }
            let _ = CloseClipboard();
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn copy_text_to_clipboard(_text: &str) {}
}
