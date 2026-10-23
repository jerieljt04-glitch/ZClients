# Private WebKit/AppKit API in ZClients

Every private selector is listed here with its purpose, its guard, and what
happens when WebKit stops calling or answering it. None can grant a
permission by failing: each fails closed or to a no-op.

| Selector | Where | Why | Guard / fallback |
|---|---|---|---|
| `_webView:requestDisplayCapturePermissionForOrigin:initiatedByFrame:withSystemAudio:decisionHandler:` (UI delegate, called *by* WebKit) | `web.rs` `Coordinator` | WebKit offers `getDisplayMedia` (Meet "Present now") only to apps that answer this; Safari does. | The decision comes from `policy::decide(ScreenCapture)`: exact call origins get the macOS picker, other shown pages get a confirmation first, everything else is denied. If WebKit stops calling it, screen sharing is refused (WebKit's default) — nothing is granted. |
| `_webView:saveDataToFile:suggestedFilename:mimeType:originatingURL:` (UI delegate, called *by* WebKit) | `web.rs` `Coordinator` | The PDF viewer's download button hands the document here; without it the button does nothing. | Data is saved through `downloads::save_from_tab` (safe file name, new file only, quarantined or removed). If WebKit stops calling it, the button does nothing; File › Download Current Page still works. |
| `_hasSleepDisabler` (WKWebView) | `web.rs` `has_sleep_disabler`, self-tests only | Diagnostics: is WebKit itself holding a sleep assertion. | `respondsToSelector:` first; false if missing. Not used for any decision. |
| `_setMockCaptureDevicesEnabled:` / `_setMockCaptureDevicesPromptEnabled:` (WKPreferences) | `web.rs` `configuration` | Self-tests use WebKit's fake camera/mic/screens. | Only when `util::is_test_build()` **and** `ZCLIENTS_MOCK_MEDIA` is set. Never in the real app. |
| `_performActionForControl:` (PDF HUD) | `selftest.rs` | Presses the PDF viewer's download button in a self-test. | Test builds only. |

Public replacements already used: camera/microphone permission
(`webView:requestMediaCapturePermissionForOrigin:…` → `WKPermissionDecision`),
stopping capture (`setCameraCaptureState:` / `setMicrophoneCaptureState:`),
Web Inspector (`isInspectable`), data-store removal and listing
(`removeDataStoreForIdentifier:` / `fetchAllDataStoreIdentifiers:`), find
(`findString:withConfiguration:`), user agent
(`WKWebViewConfiguration.applicationNameForUserAgent`).

Not available publicly, so not implemented: a find-in-page **match count**
(WebKit's public `WKFindResult` only says found / not found), and a public
"stop screen sharing" call (macOS's own screen-sharing indicator in the menu
bar stops it).
