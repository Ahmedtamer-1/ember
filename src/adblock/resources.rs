use adblock::resources::{MimeType, PermissionMask, Resource, ResourceType};

/// Permission mask granted to trusted built-in / uBlock Origin filter lists
/// so `trusted-*` scriptlets (e.g. `trusted-replace-xhr-response`, `trusted-replace-fetch-response`)
/// are allowed to execute.
pub const TRUSTED_SCRIPTLET_PERMISSION: PermissionMask = PermissionMask::from_bits(0b0000_0001);

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn js_scriptlet(
    name: &str,
    aliases: &[&str],
    permission: PermissionMask,
    js_fn_source: &str,
) -> Resource {
    Resource {
        name: name.to_string(),
        aliases: aliases.iter().map(|s| (*s).to_string()).collect(),
        kind: ResourceType::Mime(MimeType::ApplicationJavascript),
        content: encode_base64(js_fn_source.as_bytes()),
        dependencies: Vec::new(),
        permission,
    }
}

fn redirect_resource(name: &str, aliases: &[&str], mime: MimeType, bytes: &[u8]) -> Resource {
    Resource {
        name: name.to_string(),
        aliases: aliases.iter().map(|s| (*s).to_string()).collect(),
        kind: ResourceType::Mime(mime),
        content: encode_base64(bytes),
        dependencies: Vec::new(),
        permission: PermissionMask::default(),
    }
}

/// Returns the built-in uBlock Origin-compatible scriptlet and redirect resources
/// registered with `Engine::use_resources(...)`.
pub fn builtin_resources() -> Vec<Resource> {
    vec![
        // --- Core YouTube & general adblocking scriptlets ---
        js_scriptlet(
            "json-prune.js",
            &["json-prune", "json-prune.fn"],
            PermissionMask::default(),
            JS_JSON_PRUNE,
        ),
        js_scriptlet(
            "json-prune-fetch-response.js",
            &["json-prune-fetch-response"],
            PermissionMask::default(),
            JS_JSON_PRUNE_FETCH_RESPONSE,
        ),
        js_scriptlet(
            "json-prune-xhr-response.js",
            &["json-prune-xhr-response"],
            PermissionMask::default(),
            JS_JSON_PRUNE_XHR_RESPONSE,
        ),
        js_scriptlet(
            "set-constant.js",
            &["set-constant", "set.js", "set"],
            PermissionMask::default(),
            JS_SET_CONSTANT,
        ),
        js_scriptlet(
            "trusted-replace-xhr-response.js",
            &["trusted-replace-xhr-response"],
            TRUSTED_SCRIPTLET_PERMISSION,
            JS_TRUSTED_REPLACE_XHR_RESPONSE,
        ),
        js_scriptlet(
            "trusted-replace-fetch-response.js",
            &["trusted-replace-fetch-response"],
            TRUSTED_SCRIPTLET_PERMISSION,
            JS_TRUSTED_REPLACE_FETCH_RESPONSE,
        ),
        js_scriptlet(
            "trusted-prune-inbound-object.js",
            &["trusted-prune-inbound-object"],
            TRUSTED_SCRIPTLET_PERMISSION,
            JS_JSON_PRUNE,
        ),
        js_scriptlet(
            "trusted-prune-outbound-object.js",
            &["trusted-prune-outbound-object"],
            TRUSTED_SCRIPTLET_PERMISSION,
            JS_TRUSTED_PRUNE_OUTBOUND_OBJECT,
        ),
        js_scriptlet(
            "abort-on-property-read.js",
            &["abort-on-property-read", "aopr.js", "aopr"],
            PermissionMask::default(),
            JS_ABORT_ON_PROPERTY_READ,
        ),
        js_scriptlet(
            "abort-on-property-write.js",
            &["abort-on-property-write", "aopw.js", "aopw"],
            PermissionMask::default(),
            JS_ABORT_ON_PROPERTY_WRITE,
        ),
        js_scriptlet(
            "abort-current-inline-script.js",
            &["abort-current-inline-script", "acis.js", "acis"],
            PermissionMask::default(),
            JS_ABORT_CURRENT_INLINE_SCRIPT,
        ),
        js_scriptlet(
            "no-setTimeout-if.js",
            &["no-setTimeout-if", "nostif.js", "nostif", "setTimeout-defuser.js"],
            PermissionMask::default(),
            JS_NO_SETTIMEOUT_IF,
        ),
        js_scriptlet(
            "no-setInterval-if.js",
            &["no-setInterval-if", "nosiif.js", "nosiif", "setInterval-defuser.js"],
            PermissionMask::default(),
            JS_NO_SETINTERVAL_IF,
        ),
        js_scriptlet(
            "no-fetch-if.js",
            &["no-fetch-if", "prevent-fetch.js", "prevent-fetch"],
            PermissionMask::default(),
            JS_NO_FETCH_IF,
        ),
        js_scriptlet(
            "no-xhr-if.js",
            &["no-xhr-if", "prevent-xhr.js", "prevent-xhr"],
            PermissionMask::default(),
            JS_NO_XHR_IF,
        ),
        js_scriptlet(
            "noeval-if.js",
            &["noeval-if", "noeval.js", "noeval"],
            PermissionMask::default(),
            JS_NOEVAL_IF,
        ),
        js_scriptlet(
            "remove-attr.js",
            &["remove-attr", "ra.js", "ra"],
            PermissionMask::default(),
            JS_REMOVE_ATTR,
        ),
        js_scriptlet(
            "remove-class.js",
            &["remove-class", "rc.js", "rc"],
            PermissionMask::default(),
            JS_REMOVE_CLASS,
        ),
        js_scriptlet(
            "set-attr.js",
            &["set-attr"],
            PermissionMask::default(),
            JS_SET_ATTR,
        ),
        // --- Redirect resources ---
        redirect_resource(
            "noop.js",
            &[
                "noopjs",
                "googlesyndication_adsbygoogle.js",
                "googletagmanager_gtm.js",
                "google-analytics_analytics.js",
                "google-analytics_ga.js",
                "doubleclick_instream_ad_status.js",
                "amazon_apstag.js",
                "scorecardresearch_beacon.js",
            ],
            MimeType::ApplicationJavascript,
            b"(() => {})();",
        ),
        redirect_resource(
            "noop.txt",
            &["nooptext", "empty"],
            MimeType::TextPlain,
            b"",
        ),
        redirect_resource(
            "noop.css",
            &["noopcss"],
            MimeType::TextCss,
            b"",
        ),
        redirect_resource(
            "noop.html",
            &["noopframe"],
            MimeType::TextHtml,
            b"<!DOCTYPE html><html><head></head><body></body></html>",
        ),
        // 1x1 transparent GIF
        redirect_resource(
            "1x1.gif",
            &["1x1-transparent.gif", "2x2.png", "32x32.png"],
            MimeType::ImageGif,
            b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;",
        ),
    ]
}

const JS_JSON_PRUNE: &str = r#"function emberJsonPrune(rawPrunePaths, rawNeedlePaths) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("json-prune", String(rawPrunePaths || ""));
  }
  var prunePaths = (rawPrunePaths || "").split(/ +/).filter(Boolean);
  var needlePaths = (rawNeedlePaths || "").split(/ +/).filter(Boolean);
  if (prunePaths.length === 0) return;

  function matchesPath(root, path) {
    var parts = path.split(".");
    var cur = root;
    for (var i = 0; i < parts.length; i++) {
      if (cur == null || typeof cur !== "object") return false;
      var k = parts[i];
      if (k === "*" || k === "[]") {
        var keys = Array.isArray(cur) ? cur.keys() : Object.keys(cur);
        for (var sub of keys) {
          if (matchesPath(cur[sub], parts.slice(i + 1).join("."))) return true;
        }
        return false;
      }
      if (!(k in cur)) return false;
      cur = cur[k];
    }
    return true;
  }

  function pruneAt(obj, parts, idx) {
    if (obj == null || typeof obj !== "object" || idx >= parts.length) return;
    var key = parts[idx];
    var isLast = idx === parts.length - 1;
    if (key === "*" || key === "[]") {
      if (Array.isArray(obj)) {
        if (isLast) { obj.length = 0; }
        else { for (var i = 0; i < obj.length; i++) pruneAt(obj[i], parts, idx + 1); }
      } else {
        var ks = Object.keys(obj);
        for (var j = 0; j < ks.length; j++) {
          if (isLast) delete obj[ks[j]];
          else pruneAt(obj[ks[j]], parts, idx + 1);
        }
      }
      return;
    }
    if (key.startsWith("[-]") && Array.isArray(obj)) {
      return;
    }
    if (!(key in obj)) return;
    if (isLast) {
      delete obj[key];
    } else {
      pruneAt(obj[key], parts, idx + 1);
    }
  }

  function applyPrune(root) {
    if (root == null || typeof root !== "object") return root;
    if (needlePaths.length > 0) {
      for (var i = 0; i < needlePaths.length; i++) {
        if (!matchesPath(root, needlePaths[i])) return root;
      }
    }
    for (var j = 0; j < prunePaths.length; j++) {
      pruneAt(root, prunePaths[j].split("."), 0);
    }
    return root;
  }

  if (!window.__emberJsonParseHooked) {
    window.__emberJsonParseHooked = true;
    window.__emberJsonPruners = [applyPrune];
    var origParse = JSON.parse;
    JSON.parse = function () {
      var res = origParse.apply(this, arguments);
      for (var i = 0; i < window.__emberJsonPruners.length; i++) {
        try { res = window.__emberJsonPruners[i](res); } catch (_) {}
      }
      return res;
    };
    var origResJson = Response.prototype.json;
    Response.prototype.json = function () {
      return origResJson.apply(this, arguments).then(function (res) {
        for (var i = 0; i < window.__emberJsonPruners.length; i++) {
          try { res = window.__emberJsonPruners[i](res); } catch (_) {}
        }
        return res;
      });
    };
  } else {
    window.__emberJsonPruners.push(applyPrune);
  }
}"#;

const JS_JSON_PRUNE_FETCH_RESPONSE: &str = r#"function emberJsonPruneFetchResponse(rawPrunePaths, rawNeedlePaths, rawPropsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("json-prune-fetch-response", String(rawPrunePaths || ""));
  }
  var prunePaths = (rawPrunePaths || "").split(/ +/).filter(Boolean);
  var needlePaths = (rawNeedlePaths || "").split(/ +/).filter(Boolean);
  if (prunePaths.length === 0) return;

  function pruneAt(obj, parts, idx) {
    if (obj == null || typeof obj !== "object" || idx >= parts.length) return;
    var key = parts[idx];
    var isLast = idx === parts.length - 1;
    if (key === "*" || key === "[]") {
      var ks = Array.isArray(obj) ? obj : Object.keys(obj);
      if (Array.isArray(obj)) {
        if (isLast) obj.length = 0;
        else for (var i = 0; i < obj.length; i++) pruneAt(obj[i], parts, idx + 1);
      } else {
        for (var j = 0; j < ks.length; j++) {
          if (isLast) delete obj[ks[j]];
          else pruneAt(obj[ks[j]], parts, idx + 1);
        }
      }
      return;
    }
    if (!(key in obj)) return;
    if (isLast) delete obj[key];
    else pruneAt(obj[key], parts, idx + 1);
  }

  var origFetch = window.fetch;
  window.fetch = function (input, init) {
    var url = typeof input === "string" ? input : (input && input.url ? input.url : "");
    return origFetch.apply(this, arguments).then(function (response) {
      if (rawPropsToMatch && url && url.indexOf(rawPropsToMatch.replace(/^url:/, "")) === -1) {
        return response;
      }
      var ct = response.headers && response.headers.get ? (response.headers.get("content-type") || "") : "";
      if (ct.indexOf("json") === -1) return response;
      return response.clone().text().then(function (text) {
        try {
          var obj = JSON.parse(text);
          for (var j = 0; j < prunePaths.length; j++) {
            pruneAt(obj, prunePaths[j].split("."), 0);
          }
          return new Response(JSON.stringify(obj), {
            status: response.status,
            statusText: response.statusText,
            headers: response.headers
          });
        } catch (_) {
          return response;
        }
      }).catch(function () { return response; });
    });
  };
}"#;

const JS_JSON_PRUNE_XHR_RESPONSE: &str = r#"function emberJsonPruneXhrResponse(rawPrunePaths, rawNeedlePaths, rawPropsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("json-prune-xhr-response", String(rawPrunePaths || ""));
  }
  var prunePaths = (rawPrunePaths || "").split(/ +/).filter(Boolean);
  if (prunePaths.length === 0) return;

  function pruneAt(obj, parts, idx) {
    if (obj == null || typeof obj !== "object" || idx >= parts.length) return;
    var key = parts[idx];
    var isLast = idx === parts.length - 1;
    if (!(key in obj)) return;
    if (isLast) delete obj[key];
    else pruneAt(obj[key], parts, idx + 1);
  }

  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__emberXhrUrl = String(url || "");
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    var xhr = this;
    xhr.addEventListener("readystatechange", function () {
      if (xhr.readyState !== 4) return;
      if (rawPropsToMatch && xhr.__emberXhrUrl && xhr.__emberXhrUrl.indexOf(rawPropsToMatch.replace(/^url:/, "")) === -1) return;
      try {
        var obj = JSON.parse(xhr.responseText);
        for (var j = 0; j < prunePaths.length; j++) {
          pruneAt(obj, prunePaths[j].split("."), 0);
        }
        var out = JSON.stringify(obj);
        Object.defineProperty(xhr, "responseText", { value: out, configurable: true });
        Object.defineProperty(xhr, "response", { value: out, configurable: true });
      } catch (_) {}
    });
    return origSend.apply(this, arguments);
  };
}"#;

const JS_SET_CONSTANT: &str = r#"function emberSetConstant(chain, rawValue) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("set-constant", String(chain || "") + "=" + String(rawValue || ""));
  }
  if (!chain) return;
  var val;
  if (rawValue === "undefined") val = undefined;
  else if (rawValue === "false") val = false;
  else if (rawValue === "true") val = true;
  else if (rawValue === "null") val = null;
  else if (rawValue === "NaN") val = NaN;
  else if (rawValue === "emptyArr") val = [];
  else if (rawValue === "emptyObj") val = {};
  else if (rawValue === "noopFunc") val = function () {};
  else if (rawValue === "trueFunc") val = function () { return true; };
  else if (rawValue === "falseFunc") val = function () { return false; };
  else if (rawValue === "''" || rawValue === "\"\"") val = "";
  else if (/^-?\d+(\.\d+)?$/.test(String(rawValue))) val = Number(rawValue);
  else val = rawValue;

  var parts = String(chain).split(".");
  function trap(obj, idx) {
    if (!obj || typeof obj !== "object") return;
    var prop = parts[idx];
    if (idx === parts.length - 1) {
      try {
        Object.defineProperty(obj, prop, {
          get: function () { return val; },
          set: function () {},
          configurable: true
        });
      } catch (_) {}
      return;
    }
    var cur = obj[prop];
    if (cur && typeof cur === "object") {
      trap(cur, idx + 1);
      return;
    }
    try {
      Object.defineProperty(obj, prop, {
        get: function () { return cur; },
        set: function (next) {
          cur = next;
          if (next && typeof next === "object") trap(next, idx + 1);
        },
        configurable: true
      });
    } catch (_) {}
  }
  trap(window, 0);
}"#;

const JS_TRUSTED_REPLACE_XHR_RESPONSE: &str = r#"function emberTrustedReplaceXhrResponse(pattern, replacement, propsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("trusted-replace-xhr-response", String(propsToMatch || pattern || ""));
  }
  if (!pattern) return;
  var re;
  try {
    if (pattern.startsWith("/") && pattern.lastIndexOf("/") > 0) {
      var last = pattern.lastIndexOf("/");
      re = new RegExp(pattern.slice(1, last), pattern.slice(last + 1));
    } else {
      re = new RegExp(pattern, "g");
    }
  } catch (_) { return; }

  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__emberXhrUrl = String(url || "");
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    var xhr = this;
    xhr.addEventListener("readystatechange", function () {
      if (xhr.readyState !== 4) return;
      if (propsToMatch && xhr.__emberXhrUrl && xhr.__emberXhrUrl.indexOf(propsToMatch.replace(/^url:/, "")) === -1) return;
      try {
        var text = xhr.responseText;
        if (typeof text === "string" && re.test(text)) {
          var next = text.replace(re, replacement || "");
          Object.defineProperty(xhr, "responseText", { value: next, configurable: true });
          Object.defineProperty(xhr, "response", { value: next, configurable: true });
        }
      } catch (_) {}
    });
    return origSend.apply(this, arguments);
  };
}"#;

const JS_TRUSTED_REPLACE_FETCH_RESPONSE: &str = r#"function emberTrustedReplaceFetchResponse(pattern, replacement, propsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("trusted-replace-fetch-response", String(propsToMatch || pattern || ""));
  }
  if (!pattern) return;
  var re;
  try {
    if (pattern.startsWith("/") && pattern.lastIndexOf("/") > 0) {
      var last = pattern.lastIndexOf("/");
      re = new RegExp(pattern.slice(1, last), pattern.slice(last + 1));
    } else {
      re = new RegExp(pattern, "g");
    }
  } catch (_) { return; }

  var origFetch = window.fetch;
  window.fetch = function (input, init) {
    var url = typeof input === "string" ? input : (input && input.url ? input.url : "");
    return origFetch.apply(this, arguments).then(function (response) {
      if (propsToMatch && url && url.indexOf(propsToMatch.replace(/^url:/, "")) === -1) {
        return response;
      }
      return response.clone().text().then(function (text) {
        if (!re.test(text)) return response;
        var next = text.replace(re, replacement || "");
        return new Response(next, {
          status: response.status,
          statusText: response.statusText,
          headers: response.headers
        });
      }).catch(function () { return response; });
    });
  };
}"#;

const JS_TRUSTED_PRUNE_OUTBOUND_OBJECT: &str = r#"function emberTrustedPruneOutboundObject(rawPrunePaths) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("trusted-prune-outbound-object", String(rawPrunePaths || ""));
  }
  var paths = (rawPrunePaths || "").split(/ +/).filter(Boolean);
  if (paths.length === 0) return;
  var origStringify = JSON.stringify;
  JSON.stringify = function (val) {
    if (val && typeof val === "object") {
      try {
        var copy = structuredClone ? structuredClone(val) : val;
        for (var i = 0; i < paths.length; i++) {
          var parts = paths[i].split(".");
          var cur = copy;
          for (var j = 0; j < parts.length - 1 && cur && typeof cur === "object"; j++) {
            cur = cur[parts[j]];
          }
          if (cur && typeof cur === "object") delete cur[parts[parts.length - 1]];
        }
        return origStringify.call(this, copy, arguments[1], arguments[2]);
      } catch (_) {}
    }
    return origStringify.apply(this, arguments);
  };
}"#;

const JS_ABORT_ON_PROPERTY_READ: &str = r#"function emberAbortOnPropertyRead(chain) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("abort-on-property-read", String(chain || ""));
  }
  if (!chain) return;
  var parts = String(chain).split(".");
  var obj = window;
  for (var i = 0; i < parts.length - 1; i++) {
    if (!obj || typeof obj !== "object") return;
    obj = obj[parts[i]];
  }
  if (obj && typeof obj === "object") {
    try {
      Object.defineProperty(obj, parts[parts.length - 1], {
        get: function () { throw new ReferenceError("Ember aborted property read: " + chain); },
        set: function () {},
        configurable: true
      });
    } catch (_) {}
  }
}"#;

const JS_ABORT_ON_PROPERTY_WRITE: &str = r#"function emberAbortOnPropertyWrite(chain) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("abort-on-property-write", String(chain || ""));
  }
  if (!chain) return;
  var parts = String(chain).split(".");
  var obj = window;
  for (var i = 0; i < parts.length - 1; i++) {
    if (!obj || typeof obj !== "object") return;
    obj = obj[parts[i]];
  }
  if (obj && typeof obj === "object") {
    try {
      Object.defineProperty(obj, parts[parts.length - 1], {
        get: function () { return undefined; },
        set: function () { throw new ReferenceError("Ember aborted property write: " + chain); },
        configurable: true
      });
    } catch (_) {}
  }
}"#;

const JS_ABORT_CURRENT_INLINE_SCRIPT: &str = r#"function emberAbortCurrentInlineScript(api, search) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("abort-current-inline-script", String(api || ""));
  }
  if (!api) return;
  var parts = String(api).split(".");
  var obj = window;
  for (var i = 0; i < parts.length - 1; i++) {
    if (!obj) return;
    obj = obj[parts[i]];
  }
  var prop = parts[parts.length - 1];
  if (!obj) return;
  var origDesc = Object.getOwnPropertyDescriptor(obj, prop);
  var cur = obj[prop];
  try {
    Object.defineProperty(obj, prop, {
      get: function () {
        var cs = document.currentScript;
        if (cs && cs.src === "" && (!search || (cs.textContent || "").indexOf(search) !== -1)) {
          throw new ReferenceError("Ember aborted inline script");
        }
        return origDesc && origDesc.get ? origDesc.get.call(this) : cur;
      },
      set: function (v) { cur = v; },
      configurable: true
    });
  } catch (_) {}
}"#;

const JS_NO_SETTIMEOUT_IF: &str = r#"function emberNoSetTimeoutIf(needle, delay) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("no-setTimeout-if", String(needle || ""));
  }
  var orig = window.setTimeout;
  window.setTimeout = function (fn, ms) {
    var src = typeof fn === "function" ? fn.toString() : String(fn || "");
    if ((!needle || src.indexOf(needle) !== -1) && (!delay || String(ms) === String(delay))) {
      return 0;
    }
    return orig.apply(this, arguments);
  };
}"#;

const JS_NO_SETINTERVAL_IF: &str = r#"function emberNoSetIntervalIf(needle, delay) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("no-setInterval-if", String(needle || ""));
  }
  var orig = window.setInterval;
  window.setInterval = function (fn, ms) {
    var src = typeof fn === "function" ? fn.toString() : String(fn || "");
    if ((!needle || src.indexOf(needle) !== -1) && (!delay || String(ms) === String(delay))) {
      return 0;
    }
    return orig.apply(this, arguments);
  };
}"#;

const JS_NO_FETCH_IF: &str = r#"function emberNoFetchIf(propsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("no-fetch-if", String(propsToMatch || ""));
  }
  var origFetch = window.fetch;
  window.fetch = function (input) {
    var url = typeof input === "string" ? input : (input && input.url ? input.url : "");
    if (!propsToMatch || url.indexOf(propsToMatch.replace(/^url:/, "")) !== -1) {
      return Promise.resolve(new Response("", { status: 200, statusText: "OK" }));
    }
    return origFetch.apply(this, arguments);
  };
}"#;

const JS_NO_XHR_IF: &str = r#"function emberNoXhrIf(propsToMatch) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("no-xhr-if", String(propsToMatch || ""));
  }
  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (m, url) {
    this.__emberXhrUrl = String(url || "");
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    if (!propsToMatch || (this.__emberXhrUrl && this.__emberXhrUrl.indexOf(propsToMatch.replace(/^url:/, "")) !== -1)) {
      Object.defineProperty(this, "readyState", { value: 4, configurable: true });
      Object.defineProperty(this, "status", { value: 200, configurable: true });
      Object.defineProperty(this, "responseText", { value: "", configurable: true });
      Object.defineProperty(this, "response", { value: "", configurable: true });
      this.dispatchEvent(new Event("readystatechange"));
      this.dispatchEvent(new Event("load"));
      return;
    }
    return origSend.apply(this, arguments);
  };
}"#;

const JS_NOEVAL_IF: &str = r#"function emberNoevalIf(needle) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("noeval-if", String(needle || ""));
  }
  var origEval = window.eval;
  window.eval = function (code) {
    if (!needle || String(code || "").indexOf(needle) !== -1) return undefined;
    return origEval.apply(this, arguments);
  };
}"#;

const JS_REMOVE_ATTR: &str = r#"function emberRemoveAttr(attrs, selector) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("remove-attr", String(attrs || ""));
  }
  var list = String(attrs || "").split("|").filter(Boolean);
  var sel = selector || ("[" + list.join("],[") + "]");
  var run = function () {
    try {
      document.querySelectorAll(sel).forEach(function (el) {
        list.forEach(function (a) { el.removeAttribute(a); });
      });
    } catch (_) {}
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", run);
  else run();
}"#;

const JS_REMOVE_CLASS: &str = r#"function emberRemoveClass(classes, selector) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("remove-class", String(classes || ""));
  }
  var list = String(classes || "").split("|").filter(Boolean);
  var sel = selector || ("." + list.join(",."));
  var run = function () {
    try {
      document.querySelectorAll(sel).forEach(function (el) {
        list.forEach(function (c) { el.classList.remove(c); });
      });
    } catch (_) {}
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", run);
  else run();
}"#;

const JS_SET_ATTR: &str = r#"function emberSetAttr(selector, attr, val) {
  if (typeof window.__emberLogScriptlet === "function") {
    window.__emberLogScriptlet("set-attr", String(selector || ""));
  }
  if (!selector || !attr) return;
  var run = function () {
    try {
      document.querySelectorAll(selector).forEach(function (el) {
        el.setAttribute(attr, val == null ? "" : String(val));
      });
    } catch (_) {}
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", run);
  else run();
}"#;
