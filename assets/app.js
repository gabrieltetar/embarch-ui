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

  document.addEventListener("DOMContentLoaded", () => {
    initNav();
    initEnrollTab();
    initDebugTab();
    initEvents();
    const toggle = document.querySelector(".theme-toggle");
    if (toggle) toggle.addEventListener("click", toggleTheme);
  });
})();
