// Loaded before the app bundle, in plain ES5: reports why the interface did not start
// (bundle that does not parse on an old WebView, missing file, early exception) to the
// startup journal. Messages only, never page content.
(function () {
  var sent = 0;
  function send(level, msg) {
    try {
      var tauri = window.__TAURI_INTERNALS__;
      if (!tauri || !tauri.invoke || sent++ > 30) return;
      tauri.invoke("boot_log_append", { level: level, module: "webview", msg: String(msg).slice(0, 500) });
    } catch (e) { /* nothing else to report to */ }
  }
  send("info", "WebView: " + navigator.userAgent);
  window.addEventListener("error", function (e) {
    var el = e.target;
    if (el && el !== window && (el.src || el.href)) {
      send("error", "Failed to load " + String(el.src || el.href).split("/").pop());
    } else {
      send("error", (e.message || "Script error") + " @ " + String(e.filename || "").split("/").pop() + ":" + (e.lineno || 0));
    }
  }, true);
  window.addEventListener("unhandledrejection", function (e) {
    var r = e.reason;
    send("error", "Unhandled rejection: " + (r && r.message ? r.message : r));
  });
})();
