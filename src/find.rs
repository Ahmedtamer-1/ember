/// State of the Find-in-Page bar (`Ctrl/Cmd+F`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindState {
    pub is_open: bool,
    pub query: String,
    pub active_match: usize,
    pub total_matches: usize,
}

/// Initialization script installed on every page WebView to support Find-in-Page with match counts
/// using the non-destructive CSS Custom Highlight API (`CSS.highlights`) and Range selection.
pub const FIND_INIT_SCRIPT: &str = r#"
(function () {
  var state = {
    query: '',
    ranges: [],
    activeIndex: 0
  };

  function ensureHighlightStyles() {
    if (document.getElementById('__ember-find-style')) return;
    var style = document.createElement('style');
    style.id = '__ember-find-style';
    style.textContent =
      '::highlight(ember-find-all) { background-color: rgba(250, 204, 21, 0.45); color: inherit; }\n' +
      '::highlight(ember-find-active) { background-color: rgba(249, 115, 22, 0.85); color: #ffffff; }';
    (document.head || document.documentElement).appendChild(style);
  }

  function isNodeVisible(el) {
    if (!el || !el.tagName) return false;
    var tag = el.tagName.toUpperCase();
    if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'NOSCRIPT' || tag === 'TEMPLATE' || tag === 'SVG') {
      return false;
    }
    if (el.id === '__ember-ctx-menu') return false;
    var style = window.getComputedStyle(el);
    return style && style.display !== 'none' && style.visibility !== 'hidden';
  }

  function collectMatchRanges(needle) {
    var ranges = [];
    if (!needle || !document.body) return ranges;
    var lowerNeedle = needle.toLowerCase();
    var nLen = lowerNeedle.length;
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    var node;
    while ((node = walker.nextNode())) {
      var parent = node.parentElement;
      if (!isNodeVisible(parent)) continue;
      var text = node.nodeValue;
      if (!text) continue;
      var lowerText = text.toLowerCase();
      var start = 0;
      var idx;
      while ((idx = lowerText.indexOf(lowerNeedle, start)) !== -1) {
        var r = document.createRange();
        r.setStart(node, idx);
        r.setEnd(node, idx + nLen);
        ranges.push(r);
        start = idx + nLen;
        if (ranges.length >= 2000) return ranges;
      }
    }
    return ranges;
  }

  function applyHighlights() {
    ensureHighlightStyles();
    if (window.CSS && CSS.highlights) {
      CSS.highlights.delete('ember-find-all');
      CSS.highlights.delete('ember-find-active');
      if (state.ranges.length > 0 && typeof Highlight !== 'undefined') {
        var allHl = new Highlight();
        for (var i = 0; i < state.ranges.length; i++) {
          allHl.add(state.ranges[i]);
        }
        CSS.highlights.set('ember-find-all', allHl);
        if (state.activeIndex >= 1 && state.activeIndex <= state.ranges.length) {
          var activeRange = state.ranges[state.activeIndex - 1];
          var activeHl = new Highlight(activeRange);
          CSS.highlights.set('ember-find-active', activeHl);
        }
      }
    }
    if (state.activeIndex >= 1 && state.activeIndex <= state.ranges.length) {
      var targetRange = state.ranges[state.activeIndex - 1];
      var rect = targetRange.getBoundingClientRect();
      if (rect && (rect.top < 0 || rect.bottom > window.innerHeight)) {
        var el = targetRange.startContainer.parentElement;
        if (el && typeof el.scrollIntoView === 'function') {
          el.scrollIntoView({ behavior: 'smooth', block: 'center' });
        }
      }
      try {
        var sel = window.getSelection();
        if (sel) {
          sel.removeAllRanges();
          sel.addRange(targetRange);
        }
      } catch (_) {}
    }
  }

  function reportCounts() {
    if (window.ipc && typeof window.ipc.postMessage === 'function') {
      window.ipc.postMessage('find_result:' + state.activeIndex + ':' + state.ranges.length);
    }
  }

  window.__emberFind = function (query, forward, reset) {
    var clean = String(query || '');
    if (!clean) {
      window.__emberClearFind();
      return;
    }
    if (reset || clean !== state.query) {
      state.query = clean;
      state.ranges = collectMatchRanges(clean);
      state.activeIndex = state.ranges.length > 0 ? 1 : 0;
    } else if (state.ranges.length > 0) {
      if (forward !== false) {
        state.activeIndex = (state.activeIndex % state.ranges.length) + 1;
      } else {
        state.activeIndex = state.activeIndex <= 1 ? state.ranges.length : state.activeIndex - 1;
      }
    }
    applyHighlights();
    reportCounts();
  };

  window.__emberClearFind = function () {
    state.query = '';
    state.ranges = [];
    state.activeIndex = 0;
    if (window.CSS && CSS.highlights) {
      CSS.highlights.delete('ember-find-all');
      CSS.highlights.delete('ember-find-active');
    }
    try {
      var sel = window.getSelection();
      if (sel) sel.removeAllRanges();
    } catch (_) {}
    reportCounts();
  };
})();
"#;

/// Builds a JavaScript snippet to execute a Find-in-Page query on the active page WebView.
pub fn build_find_script(query: &str, forward: bool, reset: bool) -> String {
    let escaped = escape_js_string(query);
    format!("if (window.__emberFind) {{ window.__emberFind('{escaped}', {forward}, {reset}); }}")
}

/// Builds a JavaScript snippet to clear Find-in-Page highlights on the active page WebView.
pub fn build_clear_find_script() -> &'static str {
    "if (window.__emberClearFind) { window.__emberClearFind(); }"
}

/// Parses a `find_result:<active>:<total>` payload from page IPC.
pub fn parse_find_result(payload: &str) -> Option<(usize, usize)> {
    let mut parts = payload.splitn(2, ':');
    let active = parts.next()?.trim().parse::<usize>().ok()?;
    let total = parts.next()?.trim().parse::<usize>().ok()?;
    Some((active, total))
}

fn escape_js_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_find_result_and_script() {
        assert_eq!(parse_find_result("3:14"), Some((3, 14)));
        assert_eq!(parse_find_result("0:0"), Some((0, 0)));
        assert_eq!(parse_find_result("invalid"), None);

        let script = build_find_script("rust's borrow", true, false);
        assert!(script.contains("rust\\'s borrow"));
    }
}
