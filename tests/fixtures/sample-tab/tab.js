// Proves a tab's script runs, can call its backend, and can hold an event stream.
(function () {
  document.getElementById("sample-script").textContent = "the script ran";
  fetch("/api/tab/sample/echo/hello?x=1", {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: "from the page",
  })
    .then(function (r) { return r.text(); })
    .then(function (text) { document.getElementById("sample-result").textContent = text; });
  var source = new EventSource("/api/tab-events/sample/echo/view");
  source.addEventListener("view", function (ev) {
    document.getElementById("sample-pushed").textContent = ev.data;
  });
})();
