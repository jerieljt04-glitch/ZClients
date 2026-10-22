//! JavaScript the app injects. All of it runs in WebKit's isolated *client*
//! content world: it shares the page's DOM but none of its JavaScript, so a
//! page can neither see these functions nor post fake messages to the app.

pub const CONTEXT_HANDLER: &str = "zcContext";
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
/// `__zcFill` for the app to sign the profile back in with saved details.
pub const LOGIN_HELPER: &str = r#"
(function () {
  if (window.__zcLogin) { return; }
  if (!/^accounts\.zoho(cloud)?\.[a-z.]+$/i.test(location.hostname)) { return; }
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
    remember();
    var p = passwordField();
    if (!p || !p.value || !lastEmail) { return; }
    var key = lastEmail + '\u0001' + p.value;
    if (key === lastPosted) { return; }
    lastPosted = key;
    try { window.webkit.messageHandlers.zcLogin.postMessage({ email: lastEmail, password: p.value }); } catch (x) {}
  }
  document.addEventListener('input', remember, true);
  document.addEventListener('click', function (ev) {
    var b = nextButton();
    if (b && ev.target && (ev.target === b || (b.contains && b.contains(ev.target)))) { maybePost(); }
  }, true);
  document.addEventListener('keydown', function (ev) { if (ev.key === 'Enter') { maybePost(); } }, true);
  document.addEventListener('submit', maybePost, true);

  // ---- sign back in with saved details ----------------------------------
  function setValue(el, v) {
    var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    el.focus();
    setter.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }
  window.__zcFill = function (email, password) {
    if (window.__zcFilling) { return 'busy'; }
    window.__zcFilling = true;
    var stage = 0, ticks = 0;
    var timer = setInterval(function () {
      ticks += 1;
      if (ticks > 80 || otpVisible()) { clearInterval(timer); window.__zcFilling = false; return; }
      var e = emailField(), p = passwordField(), b = nextButton();
      if (stage === 0 && visible(e)) {
        if (!e.value) { setValue(e, email); }
        lastEmail = e.value;
        if (visible(p)) { stage = 1; return; }
        if (b) { b.click(); }
        stage = 1;
        return;
      }
      if (stage <= 1 && visible(p)) {
        if (!p.value) { setValue(p, password); }
        lastPosted = (lastEmail || email) + '\u0001' + p.value;
        if (b) { b.click(); }
        stage = 2;
        clearInterval(timer);
        window.__zcFilling = false;
      }
    }, 250);
    return 'started';
  };
})();
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

pub fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}
