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

  function tagOf(el) {
    return el && el.tagName ? String(el.tagName).toUpperCase() : '';
  }

  function isClosedDetails(el) {
    return tagOf(el) === 'DETAILS' && !el.open;
  }

  // Returns true if `el` itself prevents its subtree from rendering (display:none, a closed
  // <details> parent when `el` is not its summary, or Ember's own context menu).
  function isSelfHidden(el) {
    if (el.id === '__ember-ctx-menu') return true;
    var parent = el.parentElement;
    if (parent && isClosedDetails(parent) && tagOf(el) !== 'SUMMARY') return true;
    var style = window.getComputedStyle(el);
    return !style || style.display === 'none';
  }

  // Walks `el` and its ancestors, caching the result for every element on the path so a
  // single search touches each element at most once.
  function isAncestryHidden(el, cache) {
    var chain = [];
    var hidden = false;
    var cur = el;
    while (cur && cur.nodeType === 1) {
      if (cache.has(cur)) {
        hidden = cache.get(cur);
        break;
      }
      chain.push(cur);
      if (isSelfHidden(cur)) {
        hidden = true;
        break;
      }
      cur = cur.parentElement;
    }
    for (var i = 0; i < chain.length; i++) {
      cache.set(chain[i], hidden);
    }
    return hidden;
  }

  // `parent` is the element that directly contains a text node.
  function isTextParentVisible(parent, cache) {
    if (!parent || !parent.tagName) return false;
    var tag = tagOf(parent);
    if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'NOSCRIPT' || tag === 'TEMPLATE' ||
        tag === 'TEXTAREA' || tag === 'TITLE') {
      return false;
    }
    // Text sitting directly inside a closed <details> (outside its summary) is not rendered.
    if (isClosedDetails(parent)) return false;
    if (isAncestryHidden(parent, cache)) return false;
    // `visibility` is inherited (and can be re-enabled by descendants), so the computed value
    // on the direct parent is authoritative.
    var style = window.getComputedStyle(parent);
    if (!style || style.visibility === 'hidden' || style.visibility === 'collapse') return false;
    if (typeof parent.checkVisibility === 'function') {
      try {
        if (!parent.checkVisibility({ visibilityProperty: true })) return false;
      } catch (_) {}
    }
    return true;
  }

  function collectMatchRanges(needle) {
    var ranges = [];
    if (!needle || !document.body) return ranges;
    var lowerNeedle = needle.toLowerCase();
    var nLen = lowerNeedle.length;
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    var hiddenCache = new Map();
    var parentVisibleCache = new Map();
    var node;
    while ((node = walker.nextNode())) {
      var parent = node.parentElement;
      var visible = parentVisibleCache.get(parent);
      if (visible === undefined) {
        visible = isTextParentVisible(parent, hiddenCache);
        parentVisibleCache.set(parent, visible);
      }
      if (!visible) continue;
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
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '<' => out.push_str("\\x3C"),
            '>' => out.push_str("\\x3E"),
            '\0' => out.push_str("\\x00"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_js_string_special_chars() {
        assert_eq!(escape_js_string("a\\b'c"), "a\\\\b\\'c");
        assert_eq!(escape_js_string("x\ny\rz"), "x\\ny\\rz");
        assert_eq!(escape_js_string("p\u{2028}q\u{2029}r"), "p\\u2028q\\u2029r");
        assert_eq!(
            escape_js_string("</script><b>"),
            "\\x3C/script\\x3E\\x3Cb\\x3E"
        );
        let script = build_find_script("</script>\u{2028}", true, true);
        assert!(!script.contains("</script>"));
        assert!(!script.contains('\u{2028}'));
    }

    #[test]
    fn test_find_script_checks_ancestor_visibility() {
        assert!(FIND_INIT_SCRIPT.contains("isAncestryHidden"));
        assert!(FIND_INIT_SCRIPT.contains("parentElement"));
        assert!(FIND_INIT_SCRIPT.contains("checkVisibility"));
        assert!(FIND_INIT_SCRIPT.contains("DETAILS"));
    }

    #[test]
    fn test_parse_find_result_and_script() {
        assert_eq!(parse_find_result("3:14"), Some((3, 14)));
        assert_eq!(parse_find_result("0:0"), Some((0, 0)));
        assert_eq!(parse_find_result("invalid"), None);

        let script = build_find_script("rust's borrow", true, false);
        assert!(script.contains("rust\\'s borrow"));
    }
}
