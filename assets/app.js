// embarch-ui client-side glue: tab switching, theme toggle, SSE plumbing.
// Zero-build (embarch-ui/design.md §3 decision 2) — plain vanilla JS, no
// bundler, no framework.

(function () {
  "use strict";

  const THEME_KEY = "embarch-ui.theme";

  // The initial theme is applied by a tiny inline script at the top of
  // <head> (index.html), before first paint, so there's no flash of the
  // wrong theme — this file only handles the toggle click from here on.
  function toggleTheme() {
    const current = document.documentElement.getAttribute("data-theme") || "dark";
    const next = current === "light" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch (_) {
      /* best effort */
    }
  }

  function showTab(name) {
    document.querySelectorAll(".nav-item").forEach((el) => {
      el.classList.toggle("active", el.dataset.tab === name);
    });
    document.querySelectorAll(".tab-panel").forEach((el) => {
      el.classList.toggle("active", el.dataset.tab === name);
    });
    const title = document.querySelector(`.nav-item[data-tab="${name}"] .nav-label`);
    const topbarTitle = document.querySelector(".topbar-title");
    if (title && topbarTitle) {
      topbarTitle.textContent = title.textContent;
    }
    try {
      localStorage.setItem("embarch-ui.tab", name);
    } catch (_) {
      /* best effort */
    }
  }

  function initNav() {
    document.querySelectorAll(".nav-item").forEach((el) => {
      el.addEventListener("click", () => showTab(el.dataset.tab));
    });
    let initial = "dashboard";
    try {
      const stored = localStorage.getItem("embarch-ui.tab");
      if (stored) initial = stored;
    } catch (_) {
      /* best effort */
    }
    showTab(initial);
  }

  function escapeHtml(value) {
    return String(value).replace(/[&<>"']/g, (ch) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[ch]));
  }

  function formatTimestamp(utcMs) {
    if (!utcMs) return "—";
    try {
      return new Date(utcMs).toLocaleString();
    } catch (_) {
      return String(utcMs);
    }
  }

  // The suite's current hardware-topology scope (embarch-topology/design.md
  // §3 decision 10): one DUT + one dev-bench per machine — a fixed pair of
  // roles, not a dynamically-discovered list.
  const ROLES = [
    { role: "dev-bench", label: "Dev bench" },
    { role: "dut", label: "DUT" },
  ];

  function findEnrolled(snapshot, role) {
    return (snapshot.enrolled || []).find((b) => b.role === role) || null;
  }

  // A board is "attached" when one of its declared serials (the JTAG
  // probe's own serial, or — for dev-bench — its separate runtime-link
  // serial, embarch-topology/design.md §3 decision 17) matches a
  // currently-enumerated probe's serial number.
  function isAttached(snapshot, board) {
    if (!board) return false;
    const probes = snapshot.probes || [];
    return probes.some((p) => p.serial_number === board.probe_serial || p.serial_number === board.link_port_serial);
  }

  function boardStatusBadge(snapshot, board) {
    if (!board) return '<span class="badge badge-neutral">not enrolled</span>';
    if (isAttached(snapshot, board)) return '<span class="badge badge-success">● attached</span>';
    return '<span class="badge badge-warning">● enrolled, not attached</span>';
  }

  function renderStatusChip(snapshot) {
    const dot = document.querySelector(".status-chip .dot");
    const text = document.querySelector(".status-chip .status-text");
    if (!dot || !text) return;
    if (snapshot.core_reachable) {
      dot.style.background = "var(--success)";
      text.textContent = "Core: connected";
    } else {
      dot.style.background = "var(--danger)";
      text.textContent = "Core: unreachable";
    }
  }

  function renderErrorBanner(elId, snapshot) {
    const el = document.getElementById(elId);
    if (!el) return;
    if (snapshot.core_reachable || !snapshot.error) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.innerHTML =
      '<div class="card-title" style="color:var(--danger);">embarch-core unreachable</div>' +
      '<p class="placeholder-note">' + escapeHtml(snapshot.error) + "</p>";
  }

  // Role/chip/probe/confirmed-timestamp rows — Dashboard's and the Enroll
  // tab's own "Enrolled boards" table. Distinct from `boardsTableRows`
  // below (Topology's Role/Chip/Probe/Status-badge shape, fixed to the
  // two canonical roles) — this one lists every actually-enrolled entry,
  // whatever its role happens to be named.
  function enrolledTableRows(snapshot) {
    const enrolled = snapshot.enrolled || [];
    if (enrolled.length === 0) {
      return '<tr><td colspan="4" class="placeholder-note">none enrolled yet</td></tr>';
    }
    return enrolled.map((b) => (
      "<tr><td>" + escapeHtml(b.role) + '</td><td class="mono">' + escapeHtml(b.chip) +
      '</td><td class="mono">' + escapeHtml(b.probe_serial) + "</td><td>" + formatTimestamp(b.confirmed_at_utc_ms) + "</td></tr>"
    )).join("");
  }

  function boardsTableRows(snapshot) {
    return ROLES.map(({ role, label }) => {
      const board = findEnrolled(snapshot, role);
      const chip = board ? escapeHtml(board.chip) : "—";
      const probe = board ? escapeHtml(board.probe_serial) : "—";
      return "<tr><td>" + escapeHtml(label) + '</td><td class="mono">' + chip +
        '</td><td class="mono">' + probe + "</td><td>" + boardStatusBadge(snapshot, board) + "</td></tr>";
    }).join("");
  }

  function alertsListHtml(alerts) {
    if (!alerts || alerts.length === 0) {
      return (
        '<div style="display:flex; flex-direction:column; align-items:center; justify-content:center; gap:10px; padding:34px 10px; text-align:center;">' +
        '<svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="var(--success)" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M8 12.5l2.5 2.5L16 9.5"/></svg>' +
        '<div style="font-size:13px; font-weight:600; color:var(--text-primary);">No mismatches</div>' +
        '<div style="font-size:12px; color:var(--text-tertiary);">Topology matches expected state</div></div>'
      );
    }
    return alerts.slice(0, 10).map((a) => (
      '<div style="display:flex; gap:12px; align-items:flex-start; padding:8px 0; border-bottom:1px solid var(--border);">' +
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="var(--warning)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="flex-shrink:0; margin-top:2px;"><path d="M12 4 3 20h18Z"/><path d="M12 10v4M12 17h.01"/></svg>' +
      '<div><div style="font-size:13px; color:var(--text-primary);">' + escapeHtml(a.reason) + "</div>" +
      '<div class="mono" style="font-size:11px; color:var(--text-tertiary); margin-top:2px;">' +
      escapeHtml(a.role) + " · " + formatTimestamp(a.occurred_at_utc_ms) + "</div></div></div>"
    )).join("");
  }

  function renderDashboard(snapshot) {
    renderErrorBanner("dashboard-error", snapshot);

    const enrolledCount = (snapshot.enrolled || []).length;
    document.getElementById("stat-enrolled-count").textContent = String(enrolledCount);
    document.getElementById("stat-enrolled-sub").textContent =
      enrolledCount > 0 ? (snapshot.enrolled.map((b) => b.role).join(" · ")) : "none enrolled yet";

    const probeCount = (snapshot.probes || []).length;
    document.getElementById("stat-probes-count").textContent = String(probeCount);
    document.getElementById("stat-probes-sub").textContent =
      probeCount > 0 ? snapshot.probes.map((p) => p.identifier).join(" · ") : "none currently attached";

    const alertCount = (snapshot.alerts || []).length;
    const alertStat = document.getElementById("stat-alerts-count");
    alertStat.textContent = String(alertCount);
    alertStat.style.color = alertCount > 0 ? "var(--warning)" : "var(--success)";
    document.getElementById("stat-alerts-sub").textContent =
      alertCount > 0 ? alertCount + " mismatch(es) recorded" : "topology matches expected";

    document.getElementById("enrolled-table-body").innerHTML = enrolledTableRows(snapshot);
    document.getElementById("dashboard-alerts-list").innerHTML = alertsListHtml(snapshot.alerts);
  }

  function renderTopologyDiagram(snapshot) {
    const svg = document.getElementById("topology-diagram");
    if (!svg) return;

    const devBench = findEnrolled(snapshot, "dev-bench");
    const dut = findEnrolled(snapshot, "dut");
    const devBenchAttached = isAttached(snapshot, devBench);
    const dutAttached = isAttached(snapshot, dut);
    const linkColor = devBenchAttached ? "var(--accent)" : "var(--border-strong)";
    const bleColor = devBenchAttached && dutAttached ? "var(--text-secondary)" : "var(--border-strong)";

    function boxLabel(board, fallback) {
      return board ? board.role : fallback;
    }
    function boxSub(board, attached) {
      if (!board) return "not enrolled";
      return (attached ? "attached · " : "not attached · ") + board.chip;
    }

    svg.innerHTML =
      '<rect x="30" y="80" width="220" height="80" rx="12" fill="var(--bg-surface-2)" stroke="var(--border-strong)" stroke-width="1.4"/>' +
      '<text x="140" y="112" text-anchor="middle" fill="var(--text-primary)" font-size="14" font-weight="600" font-family="IBM Plex Sans, sans-serif">this machine</text>' +
      '<text x="140" y="132" text-anchor="middle" fill="var(--text-tertiary)" font-size="11.5" font-family="IBM Plex Mono, monospace">' +
      (snapshot.core_reachable ? "embarch-core" : "embarch-core (unreachable)") + "</text>" +

      '<rect x="390" y="80" width="200" height="80" rx="12" fill="var(--bg-surface-2)" stroke="var(--border-strong)" stroke-width="1.4"/>' +
      '<text x="490" y="112" text-anchor="middle" fill="var(--text-primary)" font-size="14" font-weight="600" font-family="IBM Plex Sans, sans-serif">' + escapeHtml(boxLabel(devBench, "dev-bench")) + "</text>" +
      '<text x="490" y="132" text-anchor="middle" fill="var(--text-tertiary)" font-size="11.5" font-family="IBM Plex Mono, monospace">' + escapeHtml(boxSub(devBench, devBenchAttached)) + "</text>" +

      '<rect x="740" y="80" width="200" height="80" rx="12" fill="var(--bg-surface-2)" stroke="var(--border-strong)" stroke-width="1.4"/>' +
      '<text x="840" y="112" text-anchor="middle" fill="var(--text-primary)" font-size="14" font-weight="600" font-family="IBM Plex Sans, sans-serif">' + escapeHtml(boxLabel(dut, "dut")) + "</text>" +
      '<text x="840" y="132" text-anchor="middle" fill="var(--text-tertiary)" font-size="11.5" font-family="IBM Plex Mono, monospace">' + escapeHtml(boxSub(dut, dutAttached)) + "</text>" +

      '<line x1="250" y1="120" x2="390" y2="120" stroke="' + linkColor + '" stroke-width="2"/>' +
      '<text x="320" y="108" text-anchor="middle" fill="var(--text-secondary)" font-size="11" font-weight="600" font-family="IBM Plex Mono, monospace">serial</text>' +

      '<line x1="590" y1="120" x2="740" y2="120" stroke="' + bleColor + '" stroke-width="2" stroke-dasharray="6 5"/>' +
      '<text x="665" y="108" text-anchor="middle" fill="var(--text-secondary)" font-size="11" font-weight="600" font-family="IBM Plex Mono, monospace">BLE</text>';
  }

  function renderTopology(snapshot) {
    renderTopologyDiagram(snapshot);
    document.getElementById("topology-table-body").innerHTML = boardsTableRows(snapshot);
    document.getElementById("topology-alerts-list").innerHTML = alertsListHtml(snapshot.alerts);
  }

  // --- Enroll tab (milestone-1.md §4.5) ---------------------------------
  // Drag-and-drop, matching embarch-core's own retired `/enroll` page's
  // interaction model (embarch-core/src/enroll_page.rs) — but submitting
  // through embarch-ui's own `/api/enroll`, which already holds a live
  // `CoreClient` server-side, rather than asking a human to paste in
  // Core's bearer token by hand the way that static page had to.
  let selectedSerial = null;
  let lastProbesKey = null;
  let latestSnapshot = null;

  function probeLabel(serial) {
    const probes = (latestSnapshot && latestSnapshot.probes) || [];
    const p = probes.find((p) => (p.serial_number || "") === serial);
    return p ? p.identifier + " (" + serial + ")" : serial;
  }

  function renderProbePool(snapshot) {
    const pool = document.getElementById("probes-pool");
    if (!pool) return;
    const probes = snapshot.probes || [];
    // Skip rebuilding the DOM when nothing actually changed, so a
    // mid-drag/selection isn't dropped by a routine 5s re-poll — same
    // reasoning embarch-core's own retired /enroll page used.
    const key = JSON.stringify(probes.map((p) => [p.identifier, p.serial_number]));
    if (key === lastProbesKey) return;
    lastProbesKey = key;

    if (probes.length === 0) {
      pool.innerHTML = '<span class="placeholder-note">no debug probes detected</span>';
      return;
    }
    pool.innerHTML = "";
    probes.forEach((p) => {
      const card = document.createElement("div");
      card.className = "probe-card" + (p.serial_number === selectedSerial ? " selected" : "");
      card.draggable = true;
      card.dataset.serial = p.serial_number || "";
      card.textContent = p.identifier + " (" + (p.serial_number || "no serial") + ")";
      card.addEventListener("dragstart", (ev) => {
        ev.dataTransfer.setData("text/plain", card.dataset.serial);
      });
      card.addEventListener("click", () => {
        document.querySelectorAll(".probe-card").forEach((c) => c.classList.remove("selected"));
        if (selectedSerial === card.dataset.serial) {
          selectedSerial = null;
        } else {
          selectedSerial = card.dataset.serial;
          card.classList.add("selected");
        }
      });
      pool.appendChild(card);
    });
  }

  function openAssignDialog(serial, role) {
    document.getElementById("assign-probe-label").textContent = probeLabel(serial);
    document.getElementById("assign-role-label").textContent = role;
    document.getElementById("assign-chip").value = "";
    document.getElementById("assign-result").textContent = "";
    const dialog = document.getElementById("assign-dialog");
    dialog.dataset.serial = serial;
    dialog.dataset.role = role;
    dialog.style.display = "block";
    document.getElementById("assign-dialog-backdrop").style.display = "block";
  }

  function closeAssignDialog() {
    document.getElementById("assign-dialog").style.display = "none";
    document.getElementById("assign-dialog-backdrop").style.display = "none";
  }

  async function confirmAssign() {
    const dialog = document.getElementById("assign-dialog");
    const serial = dialog.dataset.serial;
    const role = dialog.dataset.role;
    const chip = document.getElementById("assign-chip").value.trim();
    const result = document.getElementById("assign-result");
    if (!chip) {
      result.innerHTML = '<span style="color:var(--danger);">chip is required</span>';
      return;
    }
    result.textContent = "enrolling…";
    try {
      const resp = await fetch("/api/enroll", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ role, chip, probe_serial: serial }),
      });
      const text = await resp.text();
      if (!resp.ok) {
        result.innerHTML = '<span style="color:var(--danger);">' + resp.status + " " + escapeHtml(text) + "</span>";
        return;
      }
      const board = JSON.parse(text);
      result.innerHTML = '<span style="color:var(--success);">enrolled &#39;' + escapeHtml(board.role) + "&#39; — chip " + escapeHtml(board.chip) + "</span>";
      selectedSerial = null;
      setTimeout(closeAssignDialog, 700);
    } catch (e) {
      result.innerHTML = '<span style="color:var(--danger);">' + escapeHtml(String(e)) + "</span>";
    }
  }

  function initEnrollTab() {
    document.querySelectorAll(".drop-zone").forEach((zone) => {
      zone.addEventListener("dragover", (ev) => {
        ev.preventDefault();
        zone.classList.add("dragover");
      });
      zone.addEventListener("dragleave", () => zone.classList.remove("dragover"));
      zone.addEventListener("drop", (ev) => {
        ev.preventDefault();
        zone.classList.remove("dragover");
        const serial = ev.dataTransfer.getData("text/plain");
        if (serial) openAssignDialog(serial, zone.dataset.role);
      });
      // Click-to-assign fallback for anyone who'd rather select-then-click
      // than drag — same flow, different trigger; also what makes this
      // usable on touch devices, where native drag-and-drop is patchy.
      zone.addEventListener("click", () => {
        if (selectedSerial) openAssignDialog(selectedSerial, zone.dataset.role);
      });
    });
    const cancelBtn = document.getElementById("assign-cancel");
    const confirmBtn = document.getElementById("assign-confirm");
    const backdrop = document.getElementById("assign-dialog-backdrop");
    if (cancelBtn) cancelBtn.addEventListener("click", closeAssignDialog);
    if (confirmBtn) confirmBtn.addEventListener("click", confirmAssign);
    if (backdrop) backdrop.addEventListener("click", closeAssignDialog);

    // `?role=<role>` pre-fill, matching embarch-core's own retired
    // `/enroll` page: a per-alert "re-enroll this board" link can land
    // here with a role named, highlighting and scrolling to that zone.
    const highlightRole = new URLSearchParams(location.search).get("role");
    if (highlightRole) {
      const zone = document.querySelector('.drop-zone[data-role="' + highlightRole.replace(/"/g, "") + '"]');
      if (zone) {
        zone.classList.add("highlight");
        zone.scrollIntoView({ behavior: "smooth", block: "center" });
      }
    }
  }

  function renderEnroll(snapshot) {
    renderErrorBanner("enroll-error", snapshot);
    renderProbePool(snapshot);
    document.getElementById("enroll-table-body").innerHTML = enrolledTableRows(snapshot);
  }

  function renderSnapshot(snapshot) {
    latestSnapshot = snapshot;
    renderStatusChip(snapshot);
    renderDashboard(snapshot);
    renderTopology(snapshot);
    renderEnroll(snapshot);
  }

  // Suite-wide SSE convergence (embarch-ui/design.md §3 decision 6): one
  // `/events` stream, not per-tab polling — a single "snapshot" event
  // carries everything Dashboard and Topology need, pushed by the server's
  // own background poll of embarch-core (main.rs), never fetched on a
  // client-side timer.
  function initEvents() {
    const statusText = document.querySelector(".status-chip .status-text");
    try {
      const source = new EventSource("/events");
      window.embarchUiEvents = source;
      source.addEventListener("snapshot", (evt) => {
        try {
          renderSnapshot(JSON.parse(evt.data));
        } catch (_) {
          /* malformed payload — wait for the next one rather than crash */
        }
      });
      source.addEventListener("error", () => {
        if (statusText) statusText.textContent = "reconnecting…";
      });
    } catch (_) {
      // EventSource unsupported or blocked — the shell still works, tabs
      // just won't get live pushes until this is revisited.
    }
  }

  // --- Debug tab (milestone-1.md §4.7) ----------------------------------
  // Backlog via one `/api/logs/recent` fetch on load, then live lines over
  // their own `/api/logs/events` SSE stream (never re-fetching `/recent`
  // on a timer — design.md §3 decision 6). `embarch-ui` never reads Core's
  // logfile directly; every line arrives already mediated through Core's
  // own HTTP+Bearer surface.
  const MAX_LOG_LINES = 2000;
  let logFilterLevel = "all";
  let logSearchText = "";
  let logPaused = false;

  // tracing_subscriber's default formatter writes the level as an
  // upper-case word (`INFO`/`WARN`/`ERROR`/`DEBUG`/`TRACE`) — matched as a
  // whole word rather than assuming a fixed column position, since ANSI
  // escape sequences may or may not surround it depending on the writer.
  function detectLevel(line) {
    if (/\bERROR\b/.test(line)) return "error";
    if (/\bWARN\b/.test(line)) return "warn";
    if (/\bINFO\b/.test(line)) return "info";
    return "other";
  }

  function logLineElement(line) {
    const level = detectLevel(line);
    const el = document.createElement("div");
    el.className = "log-line";
    el.dataset.level = level;
    const lvl = document.createElement("span");
    lvl.className = "lvl lvl-" + level;
    lvl.textContent = level === "other" ? "—" : level.toUpperCase();
    const msg = document.createElement("span");
    msg.className = "msg";
    msg.textContent = line;
    el.appendChild(lvl);
    el.appendChild(msg);
    return el;
  }

  function applyLogVisibility(el) {
    const matchesLevel = logFilterLevel === "all" || el.dataset.level === logFilterLevel;
    const matchesSearch = !logSearchText || el.querySelector(".msg").textContent.toLowerCase().includes(logSearchText);
    el.style.display = matchesLevel && matchesSearch ? "flex" : "none";
  }

  function appendLogLines(lines) {
    const console_ = document.getElementById("log-console");
    if (!console_ || lines.length === 0) return;
    const wasEmpty = console_.querySelector(".placeholder-note");
    if (wasEmpty) console_.innerHTML = "";
    const atBottom = console_.scrollHeight - console_.scrollTop - console_.clientHeight < 40;
    for (const line of lines) {
      const el = logLineElement(line);
      applyLogVisibility(el);
      console_.appendChild(el);
    }
    while (console_.children.length > MAX_LOG_LINES) {
      console_.removeChild(console_.firstChild);
    }
    if (atBottom) console_.scrollTop = console_.scrollHeight;
  }

  function renderLogsError(message) {
    const el = document.getElementById("debug-error");
    if (!el) return;
    if (!message) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.innerHTML = '<div class="card-title" style="color:var(--danger);">embarch-core unreachable</div><p class="placeholder-note"></p>';
    el.querySelector("p").textContent = message;
  }

  async function loadLogBacklog() {
    try {
      const resp = await fetch("/api/logs/recent?tail=200");
      const text = await resp.text();
      if (!resp.ok) {
        renderLogsError(text);
        return;
      }
      renderLogsError(null);
      const data = JSON.parse(text);
      appendLogLines(data.lines || []);
    } catch (e) {
      renderLogsError(String(e));
    }
  }

  function initDebugTab() {
    loadLogBacklog();

    try {
      const source = new EventSource("/api/logs/events");
      source.addEventListener("lines", (evt) => {
        if (logPaused) return;
        try {
          appendLogLines(JSON.parse(evt.data));
        } catch (_) {
          /* malformed payload — wait for the next one */
        }
      });
    } catch (_) {
      // EventSource unsupported or blocked — backlog still loaded once.
    }

    document.querySelectorAll(".chip[data-level]").forEach((chip) => {
      chip.addEventListener("click", () => {
        document.querySelectorAll(".chip[data-level]").forEach((c) => c.classList.remove("active-filter"));
        chip.classList.add("active-filter");
        logFilterLevel = chip.dataset.level;
        document.querySelectorAll("#log-console .log-line").forEach(applyLogVisibility);
      });
    });

    const search = document.getElementById("log-search");
    if (search) {
      search.addEventListener("input", () => {
        logSearchText = search.value.toLowerCase();
        document.querySelectorAll("#log-console .log-line").forEach(applyLogVisibility);
      });
    }

    const pauseBtn = document.getElementById("log-pause");
    if (pauseBtn) {
      pauseBtn.addEventListener("click", () => {
        logPaused = !logPaused;
        pauseBtn.classList.toggle("btn-primary", logPaused);
        pauseBtn.lastChild.textContent = logPaused ? " Resume" : " Pause";
      });
    }
  }


  // --- Study Designer tab (milestone-1.md §4.6) --------------------------
  //
  // Authoring happens server-side in `embarch-study-designer` (the merged
  // action list, the registry, table-rows -> `Study`); this file's job is
  // only to collect what the engineer typed and hand it over unaltered.
  // The one thing it does interpret is byte input — and only mechanically,
  // text -> UTF-8 or hex tokens -> bytes, never a number encoded into a
  // width/endianness nobody here is in a position to know
  // (embarch-study-designer/design.md §3 decision 35).

  var sdRows = [];
  var sdActions = [];        // MergedAction[] from GET /api/study-designer/actions
  var sdRegistry = [];       // RegisteredAction[] — the subset with fields to pick
  var sdNextRowId = 1;
  var sdRunSource = null;
  var sdLastStudyId = null;

  var SD_BUILT_INS = [
    { value: "ble_connect", label: "BleConnect — connect to the DUT" },
    { value: "gatt_discover", label: "GattDiscover — walk the GATT table" },
    { value: "gatt_monitor_all", label: "GattMonitorAll — subscribe + capture for this step" },
    { value: "gatt_monitor_start", label: "GattMonitorStart — open a capture window" },
    { value: "gatt_monitor_stop", label: "GattMonitorStop — close the capture window" },
  ];

  function sdEl(id) {
    return document.getElementById(id);
  }

  // 16 raw big-endian bytes (how `Uuid` serializes) -> the hyphenated form a
  // firmware engineer actually recognizes.
  function uuidStr(bytes) {
    if (!bytes || bytes.length !== 16) return "";
    var hex = bytes.map(function (b) {
      return ("0" + (b & 0xff).toString(16)).slice(-2);
    });
    return (
      hex.slice(0, 4).join("") + "-" + hex.slice(4, 6).join("") + "-" +
      hex.slice(6, 8).join("") + "-" + hex.slice(8, 10).join("") + "-" +
      hex.slice(10, 16).join("")
    );
  }

  // Raw ATT characteristic-properties byte -> the short names that decide
  // whether a characteristic is even usable for a given operation. Bit
  // meanings are the Bluetooth Core Spec's, not this file's invention.
  function propsLabel(properties) {
    var names = [];
    if (properties & 0x02) names.push("read");
    if (properties & 0x04) names.push("write-nr");
    if (properties & 0x08) names.push("write");
    if (properties & 0x10) names.push("notify");
    if (properties & 0x20) names.push("indicate");
    return names.length ? names.join(" ") : "none";
  }

  /* Parses byte input in whichever of the two modes the engineer picked.
   *
   * "text": UTF-8, with the backslash escapes a shell/NUS command actually
   * needs — \n, \r, \t, \0, \xNN, and \\ for a literal backslash. A shell
   * command's terminator is the single most likely thing to be wrong here,
   * so it has to be typeable exactly rather than appended by this tool on a
   * guess about what the DUT expects.
   *
   * "hex": whitespace/comma-separated tokens, each 0x-prefixed hex, bare
   * hex pairs, or plain decimal — the same shapes `study-actions.toml`
   * accepts, so a value typed here and a value registered there mean the
   * same thing.
   *
   * Throws with a message naming the offending token; callers surface it
   * rather than substituting a default.
   */
  function parseBytes(text, mode) {
    if (mode === "text") {
      var out = [];
      var enc = new TextEncoder();
      for (var i = 0; i < text.length; i++) {
        var ch = text[i];
        if (ch !== "\\") {
          enc.encode(ch).forEach(function (b) { out.push(b); });
          continue;
        }
        i++;
        var esc = text[i];
        if (esc === undefined) throw new Error("payload ends with a lone backslash");
        if (esc === "n") out.push(0x0a);
        else if (esc === "r") out.push(0x0d);
        else if (esc === "t") out.push(0x09);
        else if (esc === "0") out.push(0x00);
        else if (esc === "\\") out.push(0x5c);
        else if (esc === "x") {
          var hex = text.substr(i + 1, 2);
          if (!/^[0-9a-fA-F]{2}$/.test(hex)) throw new Error("\\x must be followed by two hex digits");
          out.push(parseInt(hex, 16));
          i += 2;
        } else {
          throw new Error("unknown escape \\" + esc + " (supported: \\n \\r \\t \\0 \\xNN \\\\)");
        }
      }
      return out;
    }

    var tokens = text.split(/[\s,]+/).filter(function (t) { return t.length > 0; });
    return tokens.map(function (tok) {
      var value;
      if (/^0[xX][0-9a-fA-F]{1,2}$/.test(tok)) value = parseInt(tok.slice(2), 16);
      else if (/^[0-9a-fA-F]{2}$/.test(tok)) value = parseInt(tok, 16);
      else if (/^\d{1,3}$/.test(tok)) value = parseInt(tok, 10);
      else throw new Error("'" + tok + "' isn't a byte (expected 0xNN, NN hex, or 0-255)");
      if (value < 0 || value > 255) throw new Error("'" + tok + "' is out of the 0-255 byte range");
      return value;
    });
  }

  function sdRegisteredActions() {
    return sdActions
      .filter(function (a) { return a.Registered; })
      .map(function (a) { return a.Registered; });
  }

  function sdUnregistered() {
    return sdActions
      .filter(function (a) { return a.Unregistered; })
      .map(function (a) { return a.Unregistered; });
  }

  // Vendor-defined services (embarch-study-designer/design.md §3 decision
  // 39) — Nordic's UART Service and anything else the crate's `vendor` table
  // ships. Always present in the merged list whether or not discovery saw
  // them, since the table is a compile-time fact, not an observation.
  function sdVendorActions() {
    return sdActions
      .filter(function (a) { return a.Vendor; })
      .map(function (a) { return a.Vendor; });
  }

  function sdVendorEntry(serviceId, charId) {
    return sdVendorActions().find(function (v) {
      return v.service_id === serviceId && v.characteristic_id === charId;
    });
  }

  // Which operations a properties byte actually declares. Offering an
  // operation the characteristic doesn't support just moves the failure to
  // the middle of a study run, as an opaque ATT error.
  function opsForProperties(properties) {
    var ops = [];
    if (properties & 0x08) ops.push("write");
    if (properties & 0x02) ops.push("read");
    if (properties & 0x10) ops.push("subscribe", "notify");
    if (properties & 0x20) ops.push("indicate");
    return ops;
  }

  function sdNewRow(overrides) {
    var row = {
      id: sdNextRowId++,
      name: "step-" + (sdRows.length + 1),
      kind: "built_in",
      which: "ble_connect",
      role: "central",
      registeredName: "",
      fieldChoices: {},
      // Only meaningful for a `ble_connect` row: the advertised local name
      // to connect to (embarch-study-designer/design.md §3 decision 41).
      // Blank means "whichever peripheral advertises first", which on a
      // bench with any other BLE device in range is a coin toss.
      targetName: "",
      rawService: "",
      rawChar: "",
      // Vendor-defined selection (decision 39): ids, never UUIDs — the
      // whole point is that nobody transcribes 6e400002-… by hand. The
      // UUIDs come from the server's merged list.
      vendorService: "",
      vendorChar: "",
      // Operation + payload state is shared by the `raw` and `vendor` row
      // kinds: they differ only in where the UUID pair comes from, and
      // sharing means switching a row between them keeps what was typed.
      rawOp: "write",
      rawMode: "text",
      rawPayload: "",
      timeout_ms: 15000,
      continue_on_fail: false,
      // The "when" (decision 40): how long dev-bench waits before starting
      // this step's action. Not deducted from timeout_ms.
      delay_before_ms: 0,
    };
    Object.keys(overrides || {}).forEach(function (k) { row[k] = overrides[k]; });
    return row;
  }

  // The shape a stimulate-and-capture study needs, prefilled: the capture
  // window has to be opened *before* the write and closed after it, because
  // steps run strictly in sequence and GattMonitorAll tears its own
  // subscriptions down when its step ends
  // (embarch-study-designer/design.md §3 decision 36). Getting that order
  // wrong produces an empty capture and no error, so it's offered as one
  // click rather than left to be rediscovered.
  function sdCaptureTemplate() {
    return [
      // Left blank deliberately rather than prefilled with some DUT's name:
      // which device is under test is the engineer's to say, and the row
      // flags itself as "any device!" until they do (decision 41).
      sdNewRow({ name: "connect", kind: "built_in", which: "ble_connect", timeout_ms: 20000 }),
      sdNewRow({ name: "open-capture", kind: "built_in", which: "gatt_monitor_start", timeout_ms: 20000 }),
      // Prefilled against the Nordic UART Service rather than as a raw row:
      // its UUIDs are Nordic's, not the engineer's, so there is nothing to
      // type here but the payload (decision 39). The payload is left empty
      // on purpose — what a given DUT expects on NUS, terminator included,
      // is knowledge this tool doesn't have and won't invent.
      sdNewRow({
        name: "stimulate",
        kind: "vendor",
        vendorService: "nordic-uart",
        vendorChar: "rx",
        rawOp: "write",
        rawMode: "text",
        timeout_ms: 5000,
        // A moment inside the open window before the write, so the
        // transcript separates whatever the DUT was already saying from its
        // response to the stimulus (decision 40).
        delay_before_ms: 1000,
      }),
      // The old template put a `gatt_monitor_all` step here to hold the run
      // open while the response arrived. A delay does that without a second
      // action re-subscribing inside an already-open window, which is what
      // decision 40 made possible.
      sdNewRow({
        name: "close-capture",
        kind: "built_in",
        which: "gatt_monitor_stop",
        timeout_ms: 5000,
        delay_before_ms: 8000,
      }),
    ];
  }

  function sdActionOptionsHtml(row) {
    var html = '<optgroup label="Built-in">';
    SD_BUILT_INS.forEach(function (b) {
      var sel = row.kind === "built_in" && row.which === b.value ? " selected" : "";
      html += '<option value="builtin:' + b.value + '"' + sel + ">" + escapeHtml(b.label) + "</option>";
    });
    html += "</optgroup>";

    var registered = sdRegisteredActions();
    if (registered.length) {
      html += '<optgroup label="Registered">';
      registered.forEach(function (r) {
        var sel = row.kind === "registered" && row.registeredName === r.name ? " selected" : "";
        html += '<option value="registered:' + escapeHtml(r.name) + '"' + sel + ">" + escapeHtml(r.name) + "</option>";
      });
      html += "</optgroup>";
    }

    var vendor = sdVendorActions();
    if (vendor.length) {
      html += '<optgroup label="Vendor-defined">';
      vendor.forEach(function (v) {
        var sel =
          row.kind === "vendor" &&
          row.vendorService === v.service_id &&
          row.vendorChar === v.characteristic_id
            ? " selected"
            : "";
        var value = "vendor:" + v.service_id + ":" + v.characteristic_id;
        html +=
          '<option value="' + escapeHtml(value) + '"' + sel + ">" +
          escapeHtml(v.service_name + " — " + v.characteristic_id.toUpperCase()) +
          "</option>";
      });
      html += "</optgroup>";
    }

    html += '<optgroup label="One-off">';
    html += '<option value="raw:"' + (row.kind === "raw" ? " selected" : "") + ">Raw GATT — type UUIDs + payload</option>";
    html += "</optgroup>";
    return html;
  }

  function sdParamsHtml(row) {
    if (row.kind === "built_in") {
      if (row.which !== "ble_connect") {
        return '<span class="placeholder-note">no parameters</span>';
      }
      return (
        '<div class="sd-params"><label class="sd-param" style="flex:0 1 130px;"><span>Role</span>' +
        '<select data-field="role">' +
        '<option value="central"' + (row.role === "central" ? " selected" : "") + ">Central</option>" +
        '<option value="peripheral"' + (row.role === "peripheral" ? " selected" : "") + ">Peripheral</option>" +
        "</select></label>" +
        '<label class="sd-param" style="flex:1 1 220px;"><span>Device name' +
        (row.targetName.trim() ? "" : " — any device!") + "</span>" +
        '<input type="text" data-field="targetName" spellcheck="false" ' +
        'placeholder="e.g. the client S11" value="' + escapeHtml(row.targetName) + '" ' +
        'title="advertised local name to connect to; leave blank to take whichever peripheral advertises first" />' +
        "</label></div>"
      );
    }

    if (row.kind === "registered") {
      var action = sdRegisteredActions().find(function (r) { return r.name === row.registeredName; });
      if (!action) return '<span class="sd-error">this registered action no longer exists</span>';
      if (!action.fields || !action.fields.length) {
        return '<span class="placeholder-note mono">' + escapeHtml(action.operation) + " · no fields</span>";
      }
      var html = '<div class="sd-params">';
      action.fields.forEach(function (f) {
        html += '<label class="sd-param"><span>' + escapeHtml(f.name) + "</span>";
        html += '<select data-field="choice" data-choice-field="' + escapeHtml(f.name) + '">';
        html += '<option value="">choose…</option>';
        f.values.forEach(function (v) {
          var sel = row.fieldChoices[f.name] === v.label ? " selected" : "";
          html += '<option value="' + escapeHtml(v.label) + '"' + sel + ">" + escapeHtml(v.label) + "</option>";
        });
        html += "</select></label>";
      });
      html += "</div>";
      return html;
    }

    if (row.kind === "vendor") {
      var v = sdVendorEntry(row.vendorService, row.vendorChar);
      if (!v) {
        return '<span class="sd-error">this vendor-defined characteristic isn\'t in this build\'s table</span>';
      }
      var ops = opsForProperties(v.properties);
      // A saved row can name an operation this characteristic doesn't
      // declare (the table changed, or the row was hand-edited). Surface it
      // rather than silently snapping to something else — `build_study`
      // refuses it server-side too.
      var opInvalid = ops.indexOf(row.rawOp) < 0;
      var confirmed = [];
      if (v.sources.live) confirmed.push("live");
      if (v.sources.static_extraction) confirmed.push("source");

      var html = '<div class="sd-params">';
      html +=
        '<div class="sd-param" style="flex:1 1 260px;"><span>' +
        escapeHtml(v.characteristic_name) + "</span>" +
        '<span class="mono" style="font-size:11px;">' + escapeHtml(uuidStr(v.uuid)) + "</span>" +
        '<span style="font-size:11px; color:var(--text-tertiary);">' +
        escapeHtml(propsLabel(v.properties)) +
        (confirmed.length
          ? " · found on this DUT (" + confirmed.join("+") + ")"
          : " · not seen by discovery yet") +
        "</span></div>";
      html +=
        '<label class="sd-param" style="flex:0 1 110px;"><span>Operation</span>' +
        '<select data-field="rawOp">' +
        (opInvalid
          ? '<option value="' + escapeHtml(row.rawOp) + '" selected>' +
            escapeHtml(row.rawOp) + " (not declared)</option>"
          : "") +
        ops.map(function (op) {
          return '<option value="' + op + '"' + (row.rawOp === op ? " selected" : "") + ">" + op + "</option>";
        }).join("") +
        "</select></label>";
      html += sdPayloadInputsHtml(row);
      html += "</div>";
      return html;
    }

    // raw
    return (
      '<div class="sd-params">' +
      '<label class="sd-param" style="flex:1 1 190px;"><span>Service UUID</span>' +
      '<input type="text" data-field="rawService" spellcheck="false" placeholder="6e400001-… or 180f" value="' + escapeHtml(row.rawService) + '" /></label>' +
      '<label class="sd-param" style="flex:1 1 190px;"><span>Characteristic UUID</span>' +
      '<input type="text" data-field="rawChar" spellcheck="false" placeholder="6e400002-… or 2a19" value="' + escapeHtml(row.rawChar) + '" /></label>' +
      '<label class="sd-param" style="flex:0 1 110px;"><span>Operation</span>' +
      '<select data-field="rawOp">' +
      ["write", "read", "subscribe", "notify", "indicate"].map(function (op) {
        return '<option value="' + op + '"' + (row.rawOp === op ? " selected" : "") + ">" + op + "</option>";
      }).join("") +
      "</select></label>" +
      sdPayloadInputsHtml(row) +
      "</div>"
    );
  }

  /* The "what": mode + payload inputs, shared by the `raw` and `vendor` row
   * kinds so the one place that decides how bytes are typed stays one place.
   *
   * The placeholder shows a trailing `\n` on purpose. Nothing in this suite
   * appends a terminator — a shell command's line ending is the single most
   * likely thing to be wrong, and it's DUT-specific knowledge this tool
   * doesn't have — so it has to be visibly typeable rather than implied. */
  function sdPayloadInputsHtml(row) {
    var isWrite = row.rawOp === "write";
    return (
      '<label class="sd-param" style="flex:0 1 90px;"><span>Payload as</span>' +
      '<select data-field="rawMode">' +
      '<option value="text"' + (row.rawMode === "text" ? " selected" : "") + ">text</option>" +
      '<option value="hex"' + (row.rawMode === "hex" ? " selected" : "") + ">bytes</option>" +
      "</select></label>" +
      '<label class="sd-param" style="flex:2 1 200px;"><span>Payload' +
      (isWrite ? "" : " (write only)") + "</span>" +
      '<input type="text" data-field="rawPayload" spellcheck="false" ' +
      (isWrite ? "" : "disabled ") +
      'placeholder="' + (row.rawMode === "text" ? "kernel version\\r\\n" : "0x6b 0x76 0x0d 0x0a") + '" ' +
      'value="' + escapeHtml(row.rawPayload) + '" /></label>'
    );
  }

  function renderSdRows() {
    var tbody = sdEl("sd-rows");
    if (!tbody) return;
    if (!sdRows.length) {
      tbody.innerHTML = '<tr><td colspan="8"><span class="placeholder-note">no steps yet — add one, or start from the capture-window template</span></td></tr>';
      return;
    }
    tbody.innerHTML = "";
    // Running total of the authored delays, shown as a hint per row. Only
    // the delays are summable — a step's real duration depends on how long
    // its action takes, which is bounded by timeout_ms but not equal to it —
    // so this is labelled as the earliest each step can start, not as a
    // schedule.
    var delaySum = 0;
    sdRows.forEach(function (row, index) {
      delaySum += row.delay_before_ms || 0;
      var tr = document.createElement("tr");
      tr.dataset.rowId = String(row.id);
      tr.innerHTML =
        "<td>" + (index + 1) + "</td>" +
        '<td><input type="text" data-field="name" spellcheck="false" value="' + escapeHtml(row.name) + '" /></td>' +
        '<td><select data-field="action">' + sdActionOptionsHtml(row) + "</select></td>" +
        "<td>" + sdParamsHtml(row) + "</td>" +
        '<td><input type="number" data-field="delay_before_ms" min="0" step="250" value="' + (row.delay_before_ms || 0) + '" ' +
        'title="wait this long before starting this step\'s action; not taken out of its timeout" />' +
        '<div class="sd-delay-hint" style="font-size:11px; color:var(--text-tertiary);">' +
        (delaySum > 0 ? "+" + delaySum + "ms in" : "") +
        "</div></td>" +
        '<td><input type="number" data-field="timeout_ms" min="1" step="500" value="' + row.timeout_ms + '" /></td>' +
        '<td style="text-align:center;"><input type="checkbox" data-field="continue_on_fail"' + (row.continue_on_fail ? " checked" : "") + ' title="continue the study even if this step fails" /></td>' +
        '<td><div style="display:flex; gap:4px;">' +
        '<button class="sd-icon-btn" data-act="up" title="move up">&#9650;</button>' +
        '<button class="sd-icon-btn" data-act="down" title="move down">&#9660;</button>' +
        '<button class="sd-icon-btn" data-act="remove" title="remove">&#10005;</button>' +
        "</div></td>";
      tbody.appendChild(tr);
    });
  }

  /* Rewrites just the cumulative "+Nms in" hints, without touching any
   * input — see the `delay_before_ms` branch of `onSdRowInput`. */
  function updateSdDelayHints() {
    var tbody = sdEl("sd-rows");
    if (!tbody) return;
    var sum = 0;
    sdRows.forEach(function (row) {
      sum += row.delay_before_ms || 0;
      var tr = tbody.querySelector('tr[data-row-id="' + row.id + '"]');
      if (!tr) return;
      var hint = tr.querySelector(".sd-delay-hint");
      if (hint) hint.textContent = sum > 0 ? "+" + sum + "ms in" : "";
    });
  }

  function sdRowById(id) {
    return sdRows.find(function (r) { return r.id === Number(id); });
  }

  function onSdRowInput(ev) {
    var tr = ev.target.closest("tr[data-row-id]");
    if (!tr) return;
    var row = sdRowById(tr.dataset.rowId);
    if (!row) return;
    var field = ev.target.dataset.field;

    if (field === "action") {
      var value = ev.target.value;
      if (value.indexOf("builtin:") === 0) {
        row.kind = "built_in";
        row.which = value.slice("builtin:".length);
      } else if (value.indexOf("registered:") === 0) {
        row.kind = "registered";
        row.registeredName = value.slice("registered:".length);
        row.fieldChoices = {};
      } else if (value.indexOf("vendor:") === 0) {
        var parts = value.slice("vendor:".length).split(":");
        row.kind = "vendor";
        row.vendorService = parts[0];
        row.vendorChar = parts[1];
        // Snap the operation to one this characteristic declares, so
        // picking NUS TX after NUS RX doesn't leave a `write` selected
        // against a notify-only characteristic.
        var entry = sdVendorEntry(row.vendorService, row.vendorChar);
        var allowed = entry ? opsForProperties(entry.properties) : [];
        if (allowed.length && allowed.indexOf(row.rawOp) < 0) row.rawOp = allowed[0];
      } else {
        row.kind = "raw";
      }
      renderSdRows();
      return;
    }

    if (field === "choice") {
      row.fieldChoices[ev.target.dataset.choiceField] = ev.target.value;
      return;
    }
    if (field === "name") { row.name = ev.target.value; return; }
    if (field === "timeout_ms") { row.timeout_ms = Number(ev.target.value) || 0; return; }
    if (field === "delay_before_ms") {
      row.delay_before_ms = Math.max(0, Number(ev.target.value) || 0);
      // Every downstream row's cumulative hint shifts, but re-rendering the
      // table would blow away focus and the caret mid-typing — so the hints
      // are patched in place instead.
      updateSdDelayHints();
      return;
    }
    if (field === "continue_on_fail") { row.continue_on_fail = ev.target.checked; return; }
    if (field === "role") { row.role = ev.target.value; return; }
    if (field === "targetName") {
      var wasBlank = !row.targetName.trim();
      row.targetName = ev.target.value;
      // Only re-render when the "— any device!" warning appears or clears,
      // so typing doesn't lose focus on every keystroke.
      if (wasBlank !== !row.targetName.trim()) renderSdRows();
      return;
    }
    if (field === "rawService") { row.rawService = ev.target.value; return; }
    if (field === "rawChar") { row.rawChar = ev.target.value; return; }
    if (field === "rawPayload") { row.rawPayload = ev.target.value; return; }
    if (field === "rawOp") {
      row.rawOp = ev.target.value;
      renderSdRows();  // the payload input enables/disables with the operation
      return;
    }
    if (field === "rawMode") {
      row.rawMode = ev.target.value;
      renderSdRows();
      return;
    }
  }

  function onSdRowClick(ev) {
    var btn = ev.target.closest("button[data-act]");
    if (!btn) return;
    var tr = btn.closest("tr[data-row-id]");
    var index = sdRows.findIndex(function (r) { return r.id === Number(tr.dataset.rowId); });
    if (index < 0) return;
    var act = btn.dataset.act;
    if (act === "remove") sdRows.splice(index, 1);
    else if (act === "up" && index > 0) sdRows.splice(index - 1, 0, sdRows.splice(index, 1)[0]);
    else if (act === "down" && index < sdRows.length - 1) sdRows.splice(index + 1, 0, sdRows.splice(index, 1)[0]);
    renderSdRows();
  }

  /* Turns the table into the `TableRow[]` the server's `build_study`
   * expects. Byte parsing is the only transformation; everything else is a
   * direct copy. Throws a message naming the row, so an error points at the
   * step that caused it rather than at the study as a whole. */
  function sdCollectRows() {
    return sdRows.map(function (row, index) {
      var label = "step " + (index + 1) + " ('" + row.name + "')";
      var action;
      if (row.kind === "built_in") {
        action = {
          kind: "built_in",
          which: row.which,
          role: row.role,
          target_name: row.targetName.trim() || null,
        };
      } else if (row.kind === "registered") {
        action = { kind: "registered", name: row.registeredName, field_choices: row.fieldChoices };
      } else if (row.kind === "vendor") {
        if (!row.vendorService || !row.vendorChar) {
          throw new Error(label + ": no vendor-defined characteristic picked");
        }
        action = {
          kind: "vendor",
          // Ids, not UUIDs: the server resolves them against the crate's
          // own vendor table, so the browser never carries a UUID it could
          // get wrong.
          service: row.vendorService,
          characteristic: row.vendorChar,
          operation: row.rawOp,
          payload: sdRowPayload(row, label),
        };
      } else {
        if (!row.rawService.trim() || !row.rawChar.trim()) {
          throw new Error(label + ": a raw GATT step needs both a service and a characteristic UUID");
        }
        action = {
          kind: "raw",
          service_uuid: row.rawService.trim(),
          characteristic_uuid: row.rawChar.trim(),
          operation: row.rawOp,
          payload: sdRowPayload(row, label),
        };
      }
      return {
        name: row.name,
        action: action,
        timeout_ms: row.timeout_ms,
        continue_on_fail: row.continue_on_fail,
        delay_before_ms: row.delay_before_ms || 0,
      };
    });
  }

  /* Parses a row's payload if its operation is a Write, else []. Shared by
   * the `raw` and `vendor` branches of `sdCollectRows`, which must agree:
   * a payload only means something for a Write, and the server refuses one
   * given against anything else rather than dropping it. */
  function sdRowPayload(row, label) {
    if (row.rawOp !== "write") return [];
    try {
      return parseBytes(row.rawPayload, row.rawMode);
    } catch (e) {
      throw new Error(label + ": " + e.message);
    }
  }

  function sdShowBuildError(message) {
    var el = sdEl("sd-build-error");
    if (!el) return;
    if (!message) {
      el.style.display = "none";
      el.textContent = "";
      return;
    }
    el.style.display = "block";
    el.textContent = message;
  }

  function renderSdUnregistered() {
    var pool = sdEl("sd-unregistered");
    if (!pool) return;
    var items = sdUnregistered();
    if (!items.length) {
      pool.innerHTML = '<span class="placeholder-note">nothing detected yet — run <span class="mono">Discover GATT</span> with the dev-bench and DUT connected, or set <span class="mono">[study_designer].static_extractor</span> to read them from the firmware source</span>';
      return;
    }
    pool.innerHTML = "";
    items.forEach(function (item) {
      var chip = document.createElement("div");
      chip.className = "probe-card";
      var sources = [];
      if (item.sources.live) sources.push("live");
      if (item.sources.static_extraction) sources.push("source");
      chip.innerHTML =
        '<div class="mono" style="font-size:12px;">' + escapeHtml(uuidStr(item.uuid)) + "</div>" +
        '<div style="font-size:11px; color:var(--text-tertiary);">' +
        escapeHtml(propsLabel(item.properties)) + " · " + sources.join("+") + "</div>";
      chip.title = "service " + uuidStr(item.service_uuid);
      chip.style.cursor = "pointer";
      chip.addEventListener("click", function () {
        openRegisterDialog(uuidStr(item.service_uuid), uuidStr(item.uuid), item.properties);
      });
      pool.appendChild(chip);
    });
  }

  async function loadSdActions() {
    var resp = await fetch("/api/study-designer/actions");
    if (!resp.ok) throw new Error(await resp.text());
    var data = await resp.json();
    sdActions = data.actions || [];
    sdRegistry = sdRegisteredActions();
    renderSdUnregistered();
    renderSdRows();
    return data;
  }

  async function loadSdStudies() {
    var select = sdEl("sd-load-select");
    if (!select) return;
    var resp = await fetch("/api/study-designer/studies");
    if (!resp.ok) return;
    var studies = await resp.json();
    var current = select.value;
    select.innerHTML = '<option value="">Load saved study…</option>';
    studies.forEach(function (s) {
      var opt = document.createElement("option");
      opt.value = s.slug;
      opt.textContent = s.name + " (" + s.steps + " step" + (s.steps === 1 ? "" : "s") + ")" + (s.editable ? "" : " — run-only");
      select.appendChild(opt);
    });
    select.value = current;
  }

  async function sdSaveStudy() {
    sdShowBuildError("");
    var name = sdEl("sd-name").value.trim();
    if (!name) return sdShowBuildError("give the study a name before saving");
    if (!sdRows.length) return sdShowBuildError("a study needs at least one step");
    var rows;
    try {
      rows = sdCollectRows();
    } catch (e) {
      return sdShowBuildError(e.message);
    }
    var resp = await fetch("/api/study-designer/studies", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: name, rows: rows }),
    });
    var text = await resp.text();
    if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
    var saved = JSON.parse(text);
    sdEl("sd-toolbar-note").innerHTML =
      'Saved to <span class="mono">' + escapeHtml(saved.path) + "</span> — re-run it any time with " +
      '<span class="mono">embarch-api run-study --study-file ' + escapeHtml(saved.path) + "</span>";
    await loadSdStudies();
    sdEl("sd-load-select").value = saved.slug;
  }

  async function sdLoadStudy(slug) {
    if (!slug) return;
    sdShowBuildError("");
    var resp = await fetch("/api/study-designer/studies/" + encodeURIComponent(slug));
    var text = await resp.text();
    if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
    var loaded = JSON.parse(text);
    sdEl("sd-name").value = loaded.name;
    sdRows = loaded.rows.map(function (r) {
      var base = sdNewRow({
        name: r.name,
        timeout_ms: r.timeout_ms,
        continue_on_fail: !!r.continue_on_fail,
        delay_before_ms: r.delay_before_ms || 0,
      });
      var a = r.action || {};
      if (a.kind === "built_in") {
        base.kind = "built_in";
        base.which = a.which;
        base.role = a.role || "central";
        base.targetName = a.target_name || "";
      } else if (a.kind === "registered") {
        base.kind = "registered";
        base.registeredName = a.name;
        base.fieldChoices = a.field_choices || {};
      } else if (a.kind === "vendor") {
        base.kind = "vendor";
        base.vendorService = a.service || "";
        base.vendorChar = a.characteristic || "";
        base.rawOp = a.operation || "write";
        base.rawMode = "hex";
        base.rawPayload = (a.payload || []).map(function (b) {
          return "0x" + ("0" + b.toString(16)).slice(-2);
        }).join(" ");
      } else if (a.kind === "raw") {
        base.kind = "raw";
        base.rawService = a.service_uuid || "";
        base.rawChar = a.characteristic_uuid || "";
        base.rawOp = a.operation || "write";
        // Round-tripped as hex, not as the text it may have been typed as:
        // the saved form is bytes, and re-rendering them as text would be a
        // guess about an encoding the bytes no longer carry.
        base.rawMode = "hex";
        base.rawPayload = (a.payload || []).map(function (b) {
          return "0x" + ("0" + b.toString(16)).slice(-2);
        }).join(" ");
      }
      return base;
    });
    renderSdRows();
  }

  async function sdDeleteStudy() {
    var slug = sdEl("sd-load-select").value;
    if (!slug) return sdShowBuildError("pick a saved study to delete first");
    var resp = await fetch("/api/study-designer/studies/" + encodeURIComponent(slug), { method: "DELETE" });
    if (!resp.ok) return sdShowBuildError(resp.status + " " + (await resp.text()));
    sdShowBuildError("");
    await loadSdStudies();
  }

  async function sdDiscover() {
    var btn = sdEl("sd-discover");
    var original = btn.textContent;
    btn.disabled = true;
    btn.textContent = "Discovering…";
    sdShowBuildError("");
    try {
      var resp = await fetch("/api/study-designer/discover", { method: "POST" });
      if (!resp.ok) {
        sdShowBuildError("discover failed: " + resp.status + " " + (await resp.text()));
        return;
      }
      var data = await resp.json();
      sdActions = data.actions || [];
      sdRegistry = sdRegisteredActions();
      renderSdUnregistered();
      renderSdRows();
    } catch (e) {
      sdShowBuildError("discover failed: " + String(e));
    } finally {
      btn.disabled = false;
      btn.textContent = original;
    }
  }

  // --- registration dialog ---

  var sdRegFieldSeq = 0;

  function regFieldHtml(seq) {
    return (
      '<div class="sd-reg-field" data-field-seq="' + seq + '">' +
      '<div class="sd-form-grid" style="margin-top:0;">' +
      '<label class="sd-field"><span>Field name</span><input type="text" class="sd-input" data-reg="fname" spellcheck="false" placeholder="e.g. command" /></label>' +
      '<label class="sd-field"><span>Byte offset / length</span>' +
      '<span style="display:flex; gap:6px;">' +
      '<input type="number" class="sd-input" data-reg="foff" value="0" min="0" style="width:50%;" />' +
      '<input type="number" class="sd-input" data-reg="flen" value="1" min="1" style="width:50%;" />' +
      "</span></label>" +
      "</div>" +
      '<div data-reg="values"></div>' +
      '<button class="btn" data-reg="add-value" style="margin-top:8px;">+ Add value</button>' +
      '<button class="sd-icon-btn" data-reg="remove-field" style="margin-top:8px; margin-left:6px;">Remove field</button>' +
      "</div>"
    );
  }

  function regValueHtml() {
    return (
      '<div class="sd-reg-value">' +
      '<label class="sd-field" style="flex:1 1 140px;"><span>Label</span><input type="text" class="sd-input" data-reg="vlabel" spellcheck="false" placeholder="e.g. start" /></label>' +
      '<label class="sd-field" style="flex:0 1 90px;"><span>Bytes as</span><select class="sd-input" data-reg="vmode"><option value="text">text</option><option value="hex">bytes</option></select></label>' +
      '<label class="sd-field" style="flex:2 1 200px;"><span>Bytes</span><input type="text" class="sd-input mono" data-reg="vbytes" spellcheck="false" placeholder="ppg start\\n" /></label>' +
      '<button class="sd-icon-btn" data-reg="remove-value">&#10005;</button>' +
      "</div>"
    );
  }

  function openRegisterDialog(serviceUuid, charUuid, properties) {
    sdEl("sd-reg-name").value = "";
    sdEl("sd-reg-service").value = serviceUuid;
    sdEl("sd-reg-char").value = charUuid;
    // Default the operation to something the characteristic's own ATT
    // properties actually allow, rather than always "write" — a registered
    // read against a notify-only characteristic fails at run time with a
    // reason that points at the DUT instead of at this form.
    var op = "read";
    if (properties & 0x08 || properties & 0x04) op = "write";
    else if (properties & 0x10) op = "notify";
    else if (properties & 0x20) op = "indicate";
    sdEl("sd-reg-op").value = op;
    sdEl("sd-reg-fields").innerHTML = "";
    sdEl("sd-reg-result").style.display = "none";
    syncRegFieldsVisibility();
    if (op === "write") addRegField();
    sdEl("sd-register-dialog").style.display = "block";
    sdEl("sd-register-backdrop").style.display = "block";
  }

  function closeRegisterDialog() {
    sdEl("sd-register-dialog").style.display = "none";
    sdEl("sd-register-backdrop").style.display = "none";
  }

  // Fields only mean something for a Write — a read/subscribe/notify entry
  // has no payload to compose, and `build_study` rejects field choices
  // against one outright rather than ignoring them.
  function syncRegFieldsVisibility() {
    var isWrite = sdEl("sd-reg-op").value === "write";
    sdEl("sd-reg-fields-wrap").style.display = isWrite ? "block" : "none";
  }

  function addRegField() {
    var wrap = document.createElement("div");
    wrap.innerHTML = regFieldHtml(sdRegFieldSeq++);
    var node = wrap.firstChild;
    sdEl("sd-reg-fields").appendChild(node);
    node.querySelector('[data-reg="values"]').insertAdjacentHTML("beforeend", regValueHtml());
  }

  function onRegisterDialogClick(ev) {
    var btn = ev.target.closest("button[data-reg]");
    if (!btn) return;
    ev.preventDefault();
    var act = btn.dataset.reg;
    if (act === "add-value") {
      btn.closest(".sd-reg-field").querySelector('[data-reg="values"]').insertAdjacentHTML("beforeend", regValueHtml());
    } else if (act === "remove-value") {
      btn.closest(".sd-reg-value").remove();
    } else if (act === "remove-field") {
      btn.closest(".sd-reg-field").remove();
    }
  }

  function regResult(message, ok) {
    var el = sdEl("sd-reg-result");
    el.style.display = "block";
    el.style.color = ok ? "var(--success)" : "var(--danger)";
    el.textContent = message;
  }

  async function submitRegistration() {
    var name = sdEl("sd-reg-name").value.trim();
    if (!name) return regResult("the action needs a name", false);
    var operation = sdEl("sd-reg-op").value;

    var fields = [];
    if (operation === "write") {
      var nodes = sdEl("sd-reg-fields").querySelectorAll(".sd-reg-field");
      if (!nodes.length) return regResult("a Write needs at least one field", false);
      for (var i = 0; i < nodes.length; i++) {
        var node = nodes[i];
        var fname = node.querySelector('[data-reg="fname"]').value.trim();
        if (!fname) return regResult("every field needs a name", false);
        var byteOffset = Number(node.querySelector('[data-reg="foff"]').value) || 0;
        var byteLen = Number(node.querySelector('[data-reg="flen"]').value) || 0;
        var values = [];
        var valueNodes = node.querySelectorAll(".sd-reg-value");
        if (!valueNodes.length) return regResult("field '" + fname + "' needs at least one value", false);
        for (var j = 0; j < valueNodes.length; j++) {
          var vn = valueNodes[j];
          var label = vn.querySelector('[data-reg="vlabel"]').value.trim();
          if (!label) return regResult("every value in '" + fname + "' needs a label", false);
          var bytes;
          try {
            bytes = parseBytes(vn.querySelector('[data-reg="vbytes"]').value, vn.querySelector('[data-reg="vmode"]').value);
          } catch (e) {
            return regResult("'" + label + "': " + e.message, false);
          }
          // The registry requires bytes.len() == byte_len; caught here so
          // the message names the value rather than arriving as a generic
          // registry validation failure.
          if (bytes.length !== byteLen) {
            return regResult("'" + label + "' is " + bytes.length + " bytes but field '" + fname + "' declares " + byteLen, false);
          }
          values.push({ label: label, bytes: bytes });
        }
        fields.push({ name: fname, byte_offset: byteOffset, byte_len: byteLen, values: values });
      }
    }

    var serviceBytes = uuidToBytes(sdEl("sd-reg-service").value);
    var charBytes = uuidToBytes(sdEl("sd-reg-char").value);
    if (!serviceBytes || !charBytes) return regResult("both UUIDs must be full 128-bit UUIDs", false);

    var resp = await fetch("/api/study-designer/registry", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        name: name,
        service_uuid: serviceBytes,
        uuid: charBytes,
        operation: operation,
        fields: fields,
      }),
    });
    var text = await resp.text();
    if (!resp.ok) return regResult(resp.status + " " + text, false);
    regResult("registered — it's now pickable as a step action", true);
    await loadSdActions();
    setTimeout(closeRegisterDialog, 800);
  }

  // The registry's own `Uuid` is 16 raw bytes over the wire, so the
  // hyphenated form shown in the dialog has to go back the other way here.
  function uuidToBytes(text) {
    var hex = (text || "").trim().replace(/-/g, "").toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(hex)) return null;
    var out = [];
    for (var i = 0; i < 32; i += 2) out.push(parseInt(hex.substr(i, 2), 16));
    return out;
  }

  // --- run and watch ---

  function outcomeBadge(outcome) {
    if (outcome === "Pass") return '<span class="badge badge-success">Pass</span>';
    if (outcome === "TimedOut") return '<span class="badge badge-warning">TimedOut</span>';
    if (outcome && outcome.Fail) {
      return '<span class="badge badge-danger">Fail</span> <span class="mono" style="font-size:11.5px;">' + escapeHtml(outcome.Fail.reason) + "</span>";
    }
    return '<span class="badge badge-neutral">—</span>';
  }

  function stepDetail(step) {
    var parts = [];
    if (step.gatt_services && step.gatt_services.length) {
      var chars = step.gatt_services.reduce(function (n, s) { return n + s.characteristics.length; }, 0);
      parts.push(step.gatt_services.length + " services, " + chars + " characteristics");
    }
    if (step.gatt_activity && step.gatt_activity.length) {
      // Named as the capped summary it is, so a reader doesn't take this
      // count for the full capture — the transcript CSV is the full one.
      parts.push(step.gatt_activity.length + " notifications (capped summary)");
    }
    if (step.captured_data && step.captured_data.length) {
      parts.push(step.captured_data.length + " bytes captured");
    }
    return parts.length ? escapeHtml(parts.join(" · ")) : '<span class="placeholder-note">—</span>';
  }

  function renderRunState(state) {
    var card = sdEl("sd-run-card");
    var badge = sdEl("sd-run-status");
    var idEl = sdEl("sd-run-id");
    var reason = sdEl("sd-run-reason");
    var rows = sdEl("sd-run-rows");
    var download = sdEl("sd-gatt-download");
    if (!card) return;

    if (state.status === "idle") {
      card.style.display = "none";
      return;
    }
    card.style.display = "block";
    reason.style.display = "none";
    rows.innerHTML = "";
    download.style.display = "none";

    if (state.study_id) {
      sdLastStudyId = state.study_id;
      idEl.textContent = state.study_id;
    }

    if (state.status === "running") {
      badge.className = "badge badge-warning";
      var progress = state.current_step != null && state.total_steps != null
        ? " " + (state.current_step + 1) + "/" + state.total_steps
        : "";
      badge.textContent = "running" + progress;
      return;
    }

    if (state.status === "failed") {
      badge.className = "badge badge-danger";
      badge.textContent = "failed";
      reason.style.display = "block";
      reason.textContent = state.reason || "no reason given";
      // Still offered on a failure: `gatt.csv` is written incrementally as
      // entries arrive (design.md §5.1), so a study that failed part-way
      // usually still captured the traffic that led up to the failure —
      // which is exactly what you want to read when something went wrong.
      if (state.study_id) {
        download.href = "/api/study-designer/gatt/" + encodeURIComponent(state.study_id);
        download.style.display = "inline-flex";
      }
      return;
    }

    badge.className = "badge badge-success";
    badge.textContent = "completed";
    var result = state.result || {};
    (result.steps || []).forEach(function (step, i) {
      var tr = document.createElement("tr");
      tr.innerHTML =
        "<td>" + (i + 1) + "</td>" +
        '<td class="mono">' + escapeHtml(step.step_name) + "</td>" +
        "<td>" + outcomeBadge(step.outcome) + "</td>" +
        "<td>" + stepDetail(step) + "</td>";
      rows.appendChild(tr);
    });
    download.href = "/api/study-designer/gatt/" + encodeURIComponent(state.study_id);
    download.style.display = "inline-flex";
  }

  async function sdRunStudy() {
    sdShowBuildError("");
    if (!sdRows.length) return sdShowBuildError("a study needs at least one step");
    var rows;
    try {
      rows = sdCollectRows();
    } catch (e) {
      return sdShowBuildError(e.message);
    }
    var btn = sdEl("sd-run");
    btn.disabled = true;
    try {
      var resp = await fetch("/api/study-designer/run", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: sdEl("sd-name").value.trim() || "untitled-study", rows: rows }),
      });
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
    } catch (e) {
      return sdShowBuildError(String(e));
    } finally {
      btn.disabled = false;
    }
  }

  function initStudyDesignerTab() {
    var body = sdEl("sd-body");
    if (!body) return;

    // One probe decides whether this tab is usable at all: every route
    // answers 404 when `[study_designer]` isn't configured, so asking once
    // is more honest than rendering a table that can't submit.
    fetch("/api/study-designer/actions").then(async function (resp) {
      if (!resp.ok) {
        sdEl("sd-disabled").style.display = "block";
        return;
      }
      body.style.display = "block";
      var data = await resp.json();
      sdActions = data.actions || [];
      sdRegistry = sdRegisteredActions();
      sdRows = sdCaptureTemplate();
      renderSdRows();
      renderSdUnregistered();
      loadSdStudies();

      var tbody = sdEl("sd-rows");
      tbody.addEventListener("input", onSdRowInput);
      tbody.addEventListener("change", onSdRowInput);
      tbody.addEventListener("click", onSdRowClick);

      sdEl("sd-add-row").addEventListener("click", function () {
        sdRows.push(sdNewRow({ name: "step-" + (sdRows.length + 1) }));
        renderSdRows();
      });
      sdEl("sd-add-capture-template").addEventListener("click", function () {
        sdRows = sdRows.concat(sdCaptureTemplate());
        renderSdRows();
      });
      sdEl("sd-run").addEventListener("click", sdRunStudy);
      sdEl("sd-save").addEventListener("click", sdSaveStudy);
      sdEl("sd-delete").addEventListener("click", sdDeleteStudy);
      sdEl("sd-discover").addEventListener("click", sdDiscover);
      sdEl("sd-load-select").addEventListener("change", function (ev) {
        sdLoadStudy(ev.target.value);
      });

      sdEl("sd-reg-op").addEventListener("change", syncRegFieldsVisibility);
      sdEl("sd-reg-add-field").addEventListener("click", function (ev) {
        ev.preventDefault();
        addRegField();
      });
      sdEl("sd-register-dialog").addEventListener("click", onRegisterDialogClick);
      sdEl("sd-reg-cancel").addEventListener("click", closeRegisterDialog);
      sdEl("sd-reg-save").addEventListener("click", submitRegistration);
      sdEl("sd-register-backdrop").addEventListener("click", closeRegisterDialog);

      // Run progress arrives by push, never by client-side polling —
      // design.md §3 decision 6's suite-wide SSE convergence.
      sdRunSource = new EventSource("/api/study-designer/events");
      sdRunSource.addEventListener("run", function (ev) {
        try {
          renderRunState(JSON.parse(ev.data));
        } catch (e) {
          /* a malformed frame shouldn't kill the stream */
        }
      });
    }).catch(function () {
      sdEl("sd-disabled").style.display = "block";
    });
  }

  document.addEventListener("DOMContentLoaded", () => {
    initNav();
    initEnrollTab();
    initStudyDesignerTab();
    initDebugTab();
    initEvents();
    const toggle = document.querySelector(".theme-toggle");
    if (toggle) toggle.addEventListener("click", toggleTheme);
  });
})();
