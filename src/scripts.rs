//! JavaScript the app injects. All of it runs in WebKit's isolated *client*
//! content world: it shares the page's DOM but none of its JavaScript, so a
//! page can neither see these functions nor post fake messages to the app.

pub const CONTEXT_HANDLER: &str = "zcContext";
pub const EDITOR_HANDLER: &str = "zcEditor";

/// Tells the app whether the keyboard is in a code editor (CodeMirror, Ace,
/// Monaco, or any editor that types into a hidden text field and draws the
/// text itself — Zoho's Deluge editor among them). Those editors implement
/// Select All, Undo, Find, Save, Comment and Indent in their own key
/// handlers, so the app sends them those shortcuts as keystrokes instead of
/// running its own menu command (see `web::editor_owned_shortcut`). When the
/// editor leaves ⌘F / ⌘G / ⇧⌘G unhandled, the app's find bar opens after
/// all. Reports a yes/no and the unhandled find key only; never text.
pub const EDITOR_HOOK: &str = r#"
(function () {
  if (window.__zcEditorHook) { return; }
  window.__zcEditorHook = true;
  function isCodeEditor(el) {
    if (!el || !el.closest) { return false; }
    if (el.closest('.CodeMirror, .cm-editor, .ace_editor, .monaco-editor')) { return true; }
    if (el.tagName === 'TEXTAREA') {
      var r = el.getBoundingClientRect(), s = getComputedStyle(el);
      return r.width < 4 || r.height < 4 || s.opacity === '0' || s.color === 'transparent';
    }
    return false;
  }
  var last = null;
  function report(editor) {
    if (editor === last) { return; }
    last = editor;
    try { window.webkit.messageHandlers.zcEditor.postMessage({ editor: editor ? 'yes' : 'no' }); } catch (x) {}
  }
  document.addEventListener('focusin', function (e) {
    // Focus moving into a frame is reported by the frame itself.
    if (e.target && (e.target.tagName === 'IFRAME' || e.target.tagName === 'FRAME')) { return; }
    report(isCodeEditor(e.target));
  }, true);
  document.addEventListener('focusout', function (e) { if (!e.relatedTarget) { report(false); } }, true);
  // Bubble phase on the window: after the editor's own handlers.
  window.addEventListener('keydown', function (e) {
    if (!e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented || !isCodeEditor(document.activeElement)) { return; }
    var k = (e.key || '').toLowerCase();
    if ((k === 'f' && !e.shiftKey) || k === 'g') {
      try { window.webkit.messageHandlers.zcEditor.postMessage({ unhandled: e.shiftKey ? 'shift+' + k : k }); } catch (x) {}
    }
  }, false);
})();
"#;
pub const LOGIN_HANDLER: &str = "zcLogin";

/// Reports what the user right-clicked, so the context menu can offer a
/// working "download this" for links, images and media. Capture phase, so a
/// page that swallows the event cannot hide the target.
pub const CONTEXT_HOOK: &str = r#"
(function () {
  if (window.__zcContextHook) { return; }
  window.__zcContextHook = true;
  function mediaSource(el) {
    if (!el) { return null; }
    if (el.currentSrc) { return el.currentSrc; }
    if (el.src) { return el.src; }
    var s = el.querySelector ? el.querySelector('source[src]') : null;
    return s ? s.src : null;
  }
  function describe(t) {
    var link = null, image = null, media = null, name = null;
    try {
      if (t && t.closest) {
        link = t.closest('a[href]');
        media = t.closest('video, audio');
        var picture = t.closest('picture');
        if (t.tagName === 'IMG') { image = t; } else if (picture) { image = picture.querySelector('img'); }
      }
      if (link) {
        var explicit = link.getAttribute('download');
        name = (explicit && explicit.length ? explicit : (link.textContent || '')).trim().slice(0, 120) || null;
      }
      if (image && !name) { name = (image.getAttribute('alt') || '').trim().slice(0, 120) || null; }
    } catch (e) {}
    return { link: link ? link.href : null, image: image ? (image.currentSrc || image.src || null) : null,
             media: mediaSource(media), name: name };
  }
  function post(e) { try { window.webkit.messageHandlers.zcContext.postMessage(describe(e.target)); } catch (x) {} }
  document.addEventListener('contextmenu', post, true);
  document.addEventListener('mousedown', function (e) { if (e.button === 2) { post(e); } }, true);
})();
"#;

/// On Zoho's sign-in page only: notices the email and password the user
/// submits (so the app can offer to save them to this profile), and provides
/// `__zcFill` for the app to fill in saved details. `__ZC_HOSTS__` is
/// replaced with the exact sign-in hosts (`policy::sign_in_hosts`).
const LOGIN_HELPER_TEMPLATE: &str = r#"
(function () {
  if (window.__zcLogin) { return; }
  // Exactly Zoho's sign-in origins, default port, top frame only — the
  // same rule as policy::Capability::Autofill.
  var hosts = __ZC_HOSTS__;
  function onSignInPage() {
    return location.protocol === 'https:' && location.port === '' && window.top === window &&
           hosts.indexOf(location.hostname.toLowerCase()) >= 0;
  }
  if (!onSignInPage()) { return; }
  window.__zcLogin = true;

  function visible(el) { return !!el && !el.disabled && el.offsetParent !== null && el.getClientRects().length > 0; }
  function emailField() {
    return document.querySelector('#login_id') || document.querySelector('input[name="LOGIN_ID"]') ||
           document.querySelector('input[type="email"]');
  }
  function passwordField() {
    return document.querySelector('#password') || document.querySelector('input[name="PASSWORD"]') ||
           document.querySelector('input[type="password"]');
  }
  function nextButton() {
    return document.querySelector('#nextbtn') || document.querySelector('button[type="submit"]') ||
           document.querySelector('input[type="submit"]');
  }
  function otpVisible() {
    return Array.prototype.some.call(document.querySelectorAll('input'), function (i) {
      var n = ((i.id || '') + ' ' + (i.name || '') + ' ' + (i.autocomplete || '')).toLowerCase();
      return visible(i) && /otp|mfa|totp|one-time-code|verify/.test(n);
    });
  }

  // ---- notice what the user submits -------------------------------------
  var lastEmail = '';
  var lastPosted = '';
  function remember() {
    var e = emailField();
    if (e && e.value && e.value.indexOf('@') > 0) { lastEmail = e.value.trim(); }
  }
  function maybePost() {
    if (!onSignInPage()) { return; }
    remember();
    var p = passwordField();
    if (!p || !p.value || !lastEmail) { return; }
    var key = lastEmail + '\u0001' + p.value.length;
    if (key === lastPosted) { return; }
    lastPosted = key;
    try { window.webkit.messageHandlers.zcLogin.postMessage({ email: lastEmail, password: p.value }); } catch (x) {}
  }
  document.addEventListener('input', remember, true);
  document.addEventListener('click', function (ev) {
    var b = nextButton();
    if (b && ev.isTrusted && ev.target && (ev.target === b || (b.contains && b.contains(ev.target)))) { maybePost(); }
  }, true);
  document.addEventListener('keydown', function (ev) { if (ev.isTrusted && ev.key === 'Enter') { maybePost(); } }, true);
  document.addEventListener('submit', function (ev) { if (ev.isTrusted) { maybePost(); } }, true);

  // ---- fill in saved details --------------------------------------------
  // The email step is advanced (Next); the password is filled but never
  // submitted: the user presses Sign In. The origin is re-checked before
  // every step, and the password is dropped as soon as it is in the field.
  function setValue(el, v) {
    var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    el.focus();
    setter.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }
  window.__zcFill = function (email, password, origin) {
    if (window.__zcFilling) { return 'busy'; }
    if (location.origin !== origin || !onSignInPage()) { return 'origin-mismatch'; }
    window.__zcFilling = true;
    var stage = 0, ticks = 0;
    function stop() { clearInterval(timer); password = null; email = null; window.__zcFilling = false; }
    var timer = setInterval(function () {
      ticks += 1;
      if (ticks > 80 || otpVisible() || location.origin !== origin || !onSignInPage()) { stop(); return; }
      var e = emailField(), p = passwordField(), b = nextButton();
      if (stage === 0 && visible(e)) {
        if (!e.value) { setValue(e, email); }
        lastEmail = e.value;
        if (visible(p)) { stage = 1; return; }
        if (b && location.origin === origin) { b.click(); }
        stage = 1;
        return;
      }
      if (stage <= 1 && visible(p)) {
        if (!p.value && location.origin === origin) { setValue(p, password); }
        lastPosted = (lastEmail || email) + '\u0001' + p.value.length;
        if (b) { try { b.focus(); } catch (x) {} }
        stop();
      }
    }, 250);
    return 'started';
  };
})();
"#;

pub fn login_helper() -> String {
    let hosts = serde_json::to_string(&crate::policy::sign_in_hosts()).unwrap_or_else(|_| "[]".into());
    LOGIN_HELPER_TEMPLATE.replace("__ZC_HOSTS__", &hosts)
}

/// `callAsyncJavaScript` body that hands saved details to `__zcFill`, as
/// arguments (`email`, `password`, `origin`), never as script text.
pub const FILL_CALL: &str = r#"
if (window.top !== window || location.origin !== origin) { return 'origin-mismatch'; }
if (typeof window.__zcFill !== 'function') { return 'no-helper'; }
return window.__zcFill(email, password, origin);
"#;

/// Reads a blob:/data: URL's bytes back through the page that owns it. Run
/// with `callAsyncJavaScript`, which (unlike `evaluateJavaScript`) waits for
/// the promise.
pub const READ_INLINE_BODY: &str = r#"
const response = await fetch(u);
const blob = await response.blob();
if (blob.size > limit) { return { error: 'too-large' }; }
const bytes = new Uint8Array(await blob.arrayBuffer());
let binary = '';
for (let i = 0; i < bytes.length; i += 0x8000) {
  binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
}
return { data: btoa(binary), type: blob.type || '' };
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_helper_lists_exact_hosts_only() {
        let js = login_helper();
        assert!(js.contains("\"accounts.zoho.com\""));
        assert!(js.contains("\"accounts.zohocloud.ca\""));
        assert!(!js.contains("__ZC_HOSTS__"));
        assert!(!js.contains("slice(0, 9)"), "no prefix matching");
        assert!(!FILL_CALL.contains("b.click()"), "the password step never submits");
    }
}
