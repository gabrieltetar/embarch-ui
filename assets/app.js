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

  // A `#<tab>` fragment names a tab directly, so a link from outside can
  // land on a specific one. embarch-topology's `fix_it_url` is the real
  // caller (embarch-topology/design.md §3 decision 19): a topology mismatch
  // relayed by embarch-api points a human at `#topology` rather than at
  // whichever tab that browser happened to have open last. Unknown or absent
  // fragment -> null, and the stored/default tab wins as before.
  // The fragment may carry parameters of its own — `#trace?study=…&tap=…`
  // (see `traceDeepLink`) — so the tab name is everything up to the first `?`.
  function tabFromHash() {
    const name = (location.hash || "").replace(/^#/, "").split("?")[0];
    return document.querySelector(`.nav-item[data-tab="${CSS.escape(name)}"]`) ? name : null;
  }

  /// Parameters carried in the fragment, if any.
  function hashParams() {
    const raw = (location.hash || "").split("?")[1] || "";
    return new URLSearchParams(raw);
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
    // The fragment outranks the remembered tab — it was typed or clicked
    // just now, the stored one is from some previous session.
    showTab(tabFromHash() || initial);
    // Following the same link twice in one already-open tab changes only the
    // fragment, which fires no navigation — without this, the second click
    // does nothing at all.
    window.addEventListener("hashchange", () => {
      const name = tabFromHash();
      if (name) showTab(name);
    });
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

  // Every declared signal gets its own lane **below** the three nodes, and the
  // lane is where decision 10's requirement lives: a `direct` signal's line
  // runs the full width, past and underneath the dev-bench box, and comes back
  // up into "this machine"; a `via dev-bench` signal's line stops at the bench
  // and goes no further. The bypass is therefore a shape a reader sees at a
  // glance rather than a label they have to read — "the picture matches the
  // wiring, including when the wiring is deliberately unusual."
  //
  // Below the nodes rather than between them, found the hard way: an earlier
  // version drew the `via dev-bench` edge at the nodes' own y, which put the
  // line straight through both boxes it was meant to connect.
  const SIGNAL_LANE_TOP = 184;
  const SIGNAL_LANE_STEP = 30;
  const NODE_BOTTOM = 160;
  const DUT_CENTRE = 840;
  const BENCH_CENTRE = 490;
  const HOST_CENTRE = 140;

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
      '<text x="665" y="108" text-anchor="middle" fill="var(--text-secondary)" font-size="11" font-weight="600" font-family="IBM Plex Mono, monospace">BLE</text>' +
      signalEdges(snapshot);

    // The picture grows with the wiring rather than clipping it: every declared
    // signal takes its own lane under the nodes.
    const laneCount = (snapshot.signals || []).length;
    const height = Math.max(230, SIGNAL_LANE_TOP + laneCount * SIGNAL_LANE_STEP + 10);
    svg.setAttribute("viewBox", "0 0 1000 " + height);
    svg.setAttribute("height", String(height));
  }

  function isDirect(sig) {
    return sig && sig.route && sig.route.kind === "direct";
  }

  // A signal drawn from the same data the rows below are drawn from — one
  // source, two renderings, which is what stops the picture and the table
  // from ever disagreeing.
  function signalEdges(snapshot) {
    const signals = snapshot.signals || [];
    let out = "";
    let lane = 0;

    signals.forEach((sig) => {
      const direct = isDirect(sig);
      const viaBench = sig.route && sig.route.kind === "via-dev-bench";
      if (!direct && !viaBench) return;

      const y = SIGNAL_LANE_TOP + lane * SIGNAL_LANE_STEP;
      lane += 1;
      // Where the line comes back up decides what a reader concludes, so it is
      // the one geometric fact here that is not cosmetic: a direct signal
      // reaches the host, a bench-mediated one does not.
      const endX = direct ? HOST_CENTRE : BENCH_CENTRE;
      const colour = direct ? "var(--warning)" : "var(--accent)";
      const detail = direct
        ? "direct — bypasses dev-bench" +
          (sig.route.port_serial ? " · " + sig.route.port_serial : "")
        : "via dev-bench · rx " + (sig.route.rx_pin || "?") + " / tx " + (sig.route.tx_pin || "?");

      out +=
        '<path d="M' + DUT_CENTRE + " " + NODE_BOTTOM + " L" + DUT_CENTRE + " " + y +
        " L" + endX + " " + y + " L" + endX + " " + NODE_BOTTOM + '" fill="none" stroke="' + colour +
        '" stroke-width="2" stroke-linejoin="round"/>' +
        '<circle cx="' + endX + '" cy="' + NODE_BOTTOM + '" r="3.2" fill="' + colour + '"/>' +
        '<text x="' + (DUT_CENTRE - 20) + '" y="' + (y - 8) + '" text-anchor="end" fill="' + colour +
        '" font-size="11" font-weight="600" font-family="IBM Plex Mono, monospace">' +
        escapeHtml(sig.name) + " · " + escapeHtml(detail) + "</text>";
    });
    return out;
  }

  function routeCell(sig) {
    if (isDirect(sig)) {
      return '<span class="badge badge-warning">direct</span>';
    }
    if (sig.route && sig.route.kind === "via-dev-bench") {
      return '<span class="badge badge-success">via dev-bench</span>';
    }
    return '<span class="badge badge-neutral">—</span>';
  }

  function carrierCell(snapshot, sig) {
    if (isDirect(sig)) {
      const serial = (sig.route && sig.route.port_serial) || "";
      const port = (snapshot.serial_ports || []).find((p) => p.serial_number === serial);
      if (!port) {
        // A declared carrier that is not currently enumerated. Said as what it
        // is — a declared serial nothing on Core's machine answers to right
        // now — rather than as a port name this tab does not have.
        return (
          '<span class="mono">' + escapeHtml(serial) + "</span> " +
          '<span class="badge badge-warning">not enumerated on Core right now</span>'
        );
      }
      return (
        '<span class="mono">' + escapeHtml(port.port_name) + "</span> " +
        '<span class="placeholder-note">' + escapeHtml(port.product || "") + "</span>"
      );
    }
    if (sig.route && sig.route.kind === "via-dev-bench") {
      return (
        '<span class="mono">rx ' + escapeHtml(sig.route.rx_pin || "?") +
        " / tx " + escapeHtml(sig.route.tx_pin || "?") + "</span> " +
        '<span class="placeholder-note">relayed over dev-bench\'s own Core link</span>'
      );
    }
    return '<span class="placeholder-note">—</span>';
  }

  function signalsTableRows(snapshot) {
    const signals = snapshot.signals || [];
    if (!signals.length) {
      return (
        '<tr><td colspan="6" class="placeholder-note">No signal declared. A wire between two ' +
        "headers is invisible to software — declare one to put it on the diagram.</td></tr>"
      );
    }
    return signals
      .map(
        (sig) =>
          '<tr><td class="mono">' + escapeHtml(sig.name) + "</td>" +
          '<td class="mono">' + escapeHtml(sig.origin_role) + "</td>" +
          '<td class="mono">' + escapeHtml(sig.direction) + "</td>" +
          "<td>" + routeCell(sig) + "</td>" +
          "<td>" + carrierCell(snapshot, sig) + "</td>" +
          '<td style="text-align:right; white-space:nowrap;">' +
          '<button class="btn" data-signal-edit=""' + escapeHtml(sig.name) + '">Move route</button> ' +
          '<button class="btn" data-signal-remove="' + escapeHtml(sig.name) + '">Remove</button>' +
          "</td></tr>"
      )
      .join("");
  }

  function renderTopology(snapshot) {
    renderTopologyDiagram(snapshot);
    document.getElementById("topology-table-body").innerHTML = boardsTableRows(snapshot);
    document.getElementById("topology-alerts-list").innerHTML = alertsListHtml(snapshot.alerts);
    document.getElementById("signals-table-body").innerHTML = signalsTableRows(snapshot);

    // **An empty signal list and an unanswerable one are different states.**
    // A Core older than `GET /signals` answers 404, and rendering that as
    // "nothing declared" would state a fact about the bench that this tab
    // never established. `signals_error` is set only when Core itself
    // answered, so this never duplicates the unreachable banner.
    const err = document.getElementById("signals-error");
    if (snapshot.signals_error) {
      err.style.display = "block";
      err.textContent =
        "embarch-core did not answer GET /signals, so this list is not a statement about the " +
        "bench: " + snapshot.signals_error;
    } else {
      err.style.display = "none";
    }

    // A declared-but-wrong signal shows up in this tab or nowhere: a
    // SignalMismatch is deliberately not written to the alert log rendered
    // beside it (`embarch-topology/design.md` §3 decision 18's amendment —
    // Alert's shape is board-specific and a wire has none of those fields).
    // `carrierCell` is where that shows, per row.
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
  // Backlog via one `/recent` fetch on load, then live lines over a `/events`
  // SSE stream (never re-fetching `/recent` on a timer — design.md §3
  // decision 6).
  //
  // Two sources, one viewer (design.md §3 decision 13). embarch-core's lines
  // are proxied from its own HTTP surface; embarch-api's come from the
  // rolling file it writes, because it is spawned per session and gone —
  // there is no service to proxy to. Switching source is a full reset of the
  // console, not a merge: the two files rotate independently, and
  // interleaving them by arrival order would put lines in an order the
  // timestamps contradict.
  const MAX_LOG_LINES = 2000;
  const LOG_SOURCES = {
    core: {
      recent: "/api/logs/recent?tail=200",
      events: "/api/logs/events",
      subtitle: "Live-tailed embarch-core log",
      errorTitle: "embarch-core unreachable",
      empty: "waiting for embarch-core…",
    },
    api: {
      recent: "/api/api-logs/recent?tail=200",
      events: "/api/api-logs/events",
      subtitle: "Live-tailed embarch-api log — every MCP session and one-shot CLI run on this machine, pid- and mode-tagged",
      errorTitle: "embarch-api log unreadable",
      // Not an error state: embarch-api may simply never have run here.
      empty: "nothing logged by embarch-api yet",
    },
  };
  let logSource = "core";
  let logStream = null;
  let logFilterLevel = "all";
  let logSearchText = "";
  let logPaused = false;

  // embarch-core's logfile carries the same SGR escape sequences its stderr
  // does — its writer tees one ANSI-colored stream to both, so every line in
  // the file is wrapped in `\x1b[…m`. Rendered raw, those show up as literal
  // garbage around every level and target. Stripped here rather than fixed
  // only at the writer, because this viewer has to stay readable against the
  // deployed Core as well as a future one. (embarch-api's own file is
  // already clean — it writes a separate un-colored layer for exactly this
  // reason, embarch-api/design.md §3 decision 43.)
  function stripAnsi(line) {
    // eslint-disable-next-line no-control-regex
    return String(line).replace(/\x1b\[[0-9;]*m/g, "");
  }

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

  function logLineElement(rawLine) {
    const line = stripAnsi(rawLine);
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
    el.innerHTML = '<div class="card-title" style="color:var(--danger);"></div><p class="placeholder-note"></p>';
    el.querySelector(".card-title").textContent = LOG_SOURCES[logSource].errorTitle;
    el.querySelector("p").textContent = message;
  }

  async function loadLogBacklog() {
    try {
      const resp = await fetch(LOG_SOURCES[logSource].recent);
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

  // Tears down whatever was streaming, resets the console to the new
  // source's placeholder, then reloads backlog and reopens the stream. Kept
  // in this order so a slow backlog fetch can never land lines into a
  // console the user has since switched away from.
  function attachLogSource() {
    const config = LOG_SOURCES[logSource];
    if (logStream) {
      logStream.close();
      logStream = null;
    }
    const console_ = document.getElementById("log-console");
    if (console_) {
      console_.innerHTML = '<p class="placeholder-note"></p>';
      console_.querySelector("p").textContent = config.empty;
    }
    const subtitle = document.getElementById("debug-subtitle");
    if (subtitle) subtitle.textContent = config.subtitle;
    renderLogsError(null);

    loadLogBacklog();
    try {
      logStream = new EventSource(config.events);
      logStream.addEventListener("lines", (evt) => {
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
  }

  function initDebugTab() {
    attachLogSource();

    document.querySelectorAll(".chip[data-log-source]").forEach((chip) => {
      chip.addEventListener("click", () => {
        if (chip.dataset.logSource === logSource) return;
        document.querySelectorAll(".chip[data-log-source]").forEach((c) => c.classList.remove("active-filter"));
        chip.classList.add("active-filter");
        logSource = chip.dataset.logSource;
        attachLogSource();
      });
    });

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
  // Notify/indicate-capable characteristics, from the same response — what a
  // selective monitor row and a GATT tap both pick from (decisions 53/55).
  var sdSubscribable = [];
  /* Group headings for the target picker (decision 17), keyed by hyphenated
   * *service* UUID — `embarch-study-designer/design.md` §3 decision 56's
   * 2026-08-26 amendment. Same fallback rule as `charLabel`: an unnamed
   * service is its UUID head, never an invented name. */
  var sdServiceNames = {};
  /* `limits::MAX_MONITOR_TARGETS`, served rather than restated here so the
   * browser and `build_study` cannot disagree about the cap. The fallback
   * only applies to a response written before the field existed. */
  var sdMaxTargets = 16;
  /* The picker's working copy while its dialog is open (decision 17).
   * Cancel discards it; Done commits it to the row. Held outside the row so
   * a half-made selection never reaches `sdCollectRows`. */
  var sdTargetDraft = null;
  // Names from the firmware repo's own study-structs.toml (decision 52).
  var sdStructLayouts = [];
  // Characteristic display names, keyed by hyphenated characteristic UUID
  // (embarch-study-designer/design.md §3 decision 56). Empty is the honest
  // starting state and every reader falls back to the UUID.
  var sdCharNames = {};
  var sdNextRowId = 1;
  var sdRunSource = null;
  var sdLastStudyId = null;

  var SD_BUILT_INS = [
    { value: "ble_connect", label: "BleConnect — connect to the DUT" },
    // embarch-study-designer/design.md §3 decisions 50/51. Listed right
    // after BleConnect because that is where they belong in a study: a DUT
    // that requires an encrypted link needs security established before the
    // first GATT step, not after it.
    { value: "ble_security", label: "BleSecurity — establish encryption/pairing" },
    { value: "ble_unbond", label: "BleUnbond — drop the bond (disconnects)" },
    { value: "gatt_discover", label: "GattDiscover — walk the GATT table" },
    { value: "gatt_monitor_all", label: "GattMonitorAll — subscribe to everything + capture for this step" },
    { value: "gatt_monitor_start", label: "GattMonitorStart — open a capture window on everything" },
    // embarch-study-designer/design.md §3 decision 53. Listed after the
    // unfiltered pair because that is the order they are reached for: point
    // GattMonitorAll at an unfamiliar DUT to see what it does, then narrow
    // to the two characteristics the study is actually about.
    { value: "gatt_monitor_selected", label: "GattMonitorSelected — subscribe to chosen characteristics + capture for this step" },
    { value: "gatt_monitor_selected_start", label: "GattMonitorSelectedStart — open a capture window on chosen characteristics" },
    { value: "gatt_monitor_stop", label: "GattMonitorStop — close the capture window" },
  ];

  // Which built-ins take a characteristic selection (decision 53).
  function sdIsSelectiveMonitor(which) {
    return which === "gatt_monitor_selected" || which === "gatt_monitor_selected_start";
  }

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

  // The 16-bit-ish head of a 128-bit UUID, which is what an engineer
  // actually reads off a vendor's table — `6e400003-…` rather than the whole
  // 36 characters, in a checkbox label that has to fit several per row. The
  // full value stays in the element's title.
  function shortUuid(hyphenated) {
    return String(hyphenated || "").split("-")[0];
  }

  /* What a picker's option is labelled with (embarch-study-designer/design.md
   * §3 decision 56): the vendor's name for the characteristic, or the C
   * identifier the firmware declared it under, or — when nothing named it —
   * the UUID head this showed for everything before decision 56.
   *
   * The UUID never stops being the identity: it is what the checkbox's value
   * carries, what gets sent to the server, and what `charTitle` puts in every
   * tooltip. Decision 56 changes only what an engineer *reads*. */
  function charLabel(uuid) {
    var name = sdCharNames[uuid];
    return name ? name.label : shortUuid(uuid);
  }

  /* The label's provenance, for the tooltip. An engineer has to be able to
   * tell "the vendor publishes this name" from "your own source spells it
   * this way" — and both from a UUID nothing named, where the label *is* the
   * UUID. A name here is an identity, never a claim about what the
   * characteristic does. */
  function charTitle(uuid, serviceUuid) {
    var parts = [];
    var name = sdCharNames[uuid];
    if (name) {
      parts.push(name.origin);
      parts.push(name.source === "vendor" ? "vendor-published name" : "name from firmware source");
    }
    parts.push(uuid);
    if (serviceUuid) parts.push("service " + serviceUuid);
    return parts.join(" · ");
  }

  /* A service's group heading (decision 17): the vendor's name for it, or
   * the C identifier the firmware declared it under, or — when nothing names
   * it — the UUID head. Same three-step fallback as `charLabel`, because it
   * is the same mechanism one level up
   * (`embarch-study-designer/design.md` §3 decision 56, amended 2026-08-26). */
  function serviceLabel(uuid) {
    var name = sdServiceNames[uuid];
    return name ? name.label : shortUuid(uuid);
  }

  function serviceTitle(uuid) {
    var name = sdServiceNames[uuid];
    return (name ? name.origin + " · " : "") + uuid;
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

  /* Takes the two lists that are about *capture* rather than about writing
   * an action: the notify/indicate-capable characteristics a selective
   * monitor row and a GATT tap both pick from (decision 53), and the payload
   * layouts a tap can decode with (decision 52).
   *
   * A characteristic disappearing between discoveries (a different DUT, a
   * Kconfig-gated one) leaves a row's target checked and unrenderable, which
   * is why `sdCollectRows` resolves each UUID against this list at submit
   * time and drops what it can't resolve — refusing loudly rather than
   * sending a pair it had to invent a service for. */
  function sdAdoptDiscovery(data) {
    sdSubscribable = (data && data.subscribable) || [];
    sdStructLayouts = (data && data.struct_layouts) || [];
    sdCharNames = (data && data.characteristic_names) || {};
    sdServiceNames = (data && data.service_names) || {};
    if (data && typeof data.max_monitor_targets === "number") {
      sdMaxTargets = data.max_monitor_targets;
    }
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
      // Only meaningful for a `ble_security` row (decision 44). L4 is the
      // level that decision was written for; L1 *is* offered, because
      // decision 44 makes it the honest way to say "this DUT needs none"
      // rather than leaving the step out and hoping.
      securityLevel: "l4",
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
      // Only meaningful for a selective monitor row (decision 53): the
      // characteristic UUIDs this step subscribes to. Empty is refused
      // server-side rather than promoted to "everything" — quietly
      // subscribing to the whole table is the flood the action exists to
      // avoid.
      targets: [],
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
      if (row.which === "ble_security") {
        // The labels say what each level actually *is*, because "L2" alone
        // tells an engineer nothing about whether it satisfies a DUT that
        // demands authentication. L1 is listed last rather than omitted:
        // decision 44 makes it a real answer, and one an author has to be
        // able to give in the UI or the decision is only half implemented.
        var levels = [
          ["l4", "L4 — authenticated LE Secure Connections, 128-bit key"],
          ["l3", "L3 — encrypted + authenticated"],
          ["l2", "L2 — encrypted, unauthenticated (Just Works)"],
          ["l1", "L1 — none needed (said out loud, not skipped)"],
        ];
        var opts = levels
          .map(function (l) {
            var sel = row.securityLevel === l[0] ? " selected" : "";
            return '<option value="' + l[0] + '"' + sel + ">" + escapeHtml(l[1]) + "</option>";
          })
          .join("");
        return (
          '<div class="sd-params"><label class="sd-param" style="flex:1 1 320px;">' +
          "<span>Level</span>" +
          '<select data-field="securityLevel" ' +
          'title="the step fails if the level actually reached is lower — set continue-on-fail to ' +
          'attempt it without aborting the study">' + opts + "</select>" +
          "</label></div>"
        );
      }
      if (row.which === "ble_unbond") {
        return '<span class="placeholder-note">no parameters — this drops the link</span>';
      }
      if (sdIsSelectiveMonitor(row.which)) {
        // One line whatever the DUT's table looks like (decision 17): a
        // summary of what is picked, and a button opening the picker. The
        // options themselves are still only ever what discovery found, and
        // an empty selection is still refused rather than promoted to
        // "everything" — decision 15's two invariants are unchanged, this
        // decision changes only where the picking happens.
        if (!sdSubscribable.length) {
          return (
            '<span class="placeholder-note">no notify-capable characteristic known yet — run ' +
            '<span class="mono">Discover GATT</span>, or use GattMonitorAll to see what this DUT has</span>'
          );
        }
        return sdTargetsSummaryHtml(row);
      }
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
    if (field === "securityLevel") { row.securityLevel = ev.target.value; return; }
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
    if (act === "pick-targets") return openTargetDialog(sdRows[index]);
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
          // Sent for every built-in row, not only a security one: the
          // server ignores it for the rest, exactly as it already ignores
          // `role` and `target_name` outside `ble_connect`.
          security_level: row.securityLevel || null,
          // Resolved from checked UUIDs back to the {service, characteristic}
          // pairs the server parses (decision 53). The service comes from the
          // same discovery entry the checkbox was rendered from, so the two
          // can never disagree.
          targets: (row.targets || [])
            .map(function (uuid) {
              var found = sdSubscribable.find(function (c) {
                return c.characteristic_uuid === uuid;
              });
              return found
                ? { service_uuid: found.service_uuid, characteristic_uuid: uuid }
                : null;
            })
            .filter(Boolean),
        };
        if (sdIsSelectiveMonitor(row.which) && !action.targets.length) {
          throw new Error(
            label + ": pick at least one characteristic, or use GattMonitorAll to subscribe to everything"
          );
        }
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
      // Named where anything names it (decision 56). The UUID moves to the
      // second line rather than out of sight: this pool is the route into the
      // registration form, and matching a characteristic against a vendor's
      // own table is done by UUID.
      var uuid = uuidStr(item.uuid);
      var named = !!sdCharNames[uuid];
      chip.innerHTML =
        '<div class="mono" style="font-size:12px;">' + escapeHtml(charLabel(uuid)) + "</div>" +
        '<div style="font-size:11px; color:var(--text-tertiary);">' +
        (named ? escapeHtml(shortUuid(uuid)) + " · " : "") +
        escapeHtml(propsLabel(item.properties)) + " · " + sources.join("+") + "</div>";
      chip.title = charTitle(uuid, uuidStr(item.service_uuid));
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
    sdAdoptDiscovery(data);
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
      body: JSON.stringify({ name: name, rows: rows, requires: sdRequiresPayload(), taps: sdTaps }),
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
    if (loaded.requires) sdApplyRequires(loaded.requires);
    sdTaps = loaded.taps || [];
    renderSdTaps();
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
        // Restored, unlike before decision 17: a saved selective-monitor
        // step round-trips its targets through `TableRow`, and this used to
        // read `which`/`role`/`target_name` and drop the rest — so
        // reopening such a study came back with an empty selection and the
        // security level reset, both without a word.
        base.securityLevel = a.security_level || base.securityLevel;
        base.targets = (a.targets || []).map(function (t) {
          return t.characteristic_uuid;
        });
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
      // The device name comes from the step table's own `ble_connect` row.
      // A discovery that connects to whatever advertises first is a coin
      // flip on a bench with more than one device in range, and against a
      // named DUT it simply failed — so the name the operator has already
      // typed is the one this should use. Blank stays blank, which the
      // server reads as "any device", still a legitimate single-device bench.
      var connectRow = sdRows.filter(function (r) {
        return r.kind === "built_in" && r.which === "ble_connect" && (r.targetName || "").trim();
      })[0];
      var resp = await fetch("/api/study-designer/discover", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          target_name: connectRow ? connectRow.targetName.trim() : null,
        }),
      });
      if (!resp.ok) {
        sdShowBuildError("discover failed: " + resp.status + " " + (await resp.text()));
        return;
      }
      var data = await resp.json();
      sdActions = data.actions || [];
      sdRegistry = sdRegisteredActions();
      sdAdoptDiscovery(data);
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

  /* ---- Selective-monitor target picker (design.md §3 decision 17) --------
   *
   * The step table's Parameters cell used to hold one checkbox per
   * notify/indicate-capable characteristic — eleven on the reference DUT,
   * fourteen once the repo-wide GATT scan found its third service — which
   * made one row taller than the rest of the table put together and only
   * ever grew. What replaces it is a one-line summary plus a dialog, using
   * the same `.dialog`/`.dialog-backdrop` pattern four other places in this
   * app already use.
   *
   * What decision 15 bought is deliberately untouched: options come from
   * `sdSubscribable` and nowhere else, a picked characteristic's service
   * UUID is still read back out of the same entry the option was rendered
   * from at submit time, and an empty selection is still refused.
   */

  /* Characteristics that are picked but that the current discovery no longer
   * knows about — a study saved against one DUT and reopened against
   * another, or a Kconfig-gated characteristic that a later live discovery
   * didn't see. `sdCollectRows` drops these (it has no service UUID to pair
   * them with), so they are surfaced rather than left to vanish quietly. */
  function sdUnknownTargets(row) {
    return (row.targets || []).filter(function (uuid) {
      return !sdSubscribable.some(function (c) { return c.characteristic_uuid === uuid; });
    });
  }

  /* The cell's own content: one line, whether two characteristics are picked
   * or fourteen. */
  function sdTargetsSummaryHtml(row) {
    var picked = row.targets || [];
    var total = sdSubscribable.length;
    var unknown = sdUnknownTargets(row);
    var chips = picked
      .slice(0, 2)
      .map(function (uuid) {
        return (
          '<span class="probe-card mono" title="' + escapeHtml(charTitle(uuid, "")) + '">' +
          escapeHtml(charLabel(uuid)) + "</span>"
        );
      })
      .join("");
    if (picked.length > 2) {
      chips += '<span class="placeholder-note">+' + (picked.length - 2) + " more</span>";
    }
    var head = picked.length
      ? '<span class="mono" style="font-size:12px;">' + picked.length + " of " + total + "</span>"
      : '<span class="sd-error" style="font-size:12px;">none picked</span>';
    var note = picked.length
      ? ""
      : '<p class="placeholder-note" style="margin:6px 0 0;">pick at least one — an empty ' +
        'selection is refused rather than treated as "everything"</p>';
    if (unknown.length) {
      note +=
        '<p class="sd-error" style="margin:6px 0 0; font-size:12px;">' +
        unknown.length +
        " picked characteristic" + (unknown.length === 1 ? " is" : "s are") +
        " not in the current discovery and will be dropped — reopen the picker to clear " +
        escapeHtml(unknown.map(shortUuid).join(", ")) + "</p>";
    }
    return (
      '<div class="sd-targets-summary">' + head + chips +
      '<button class="btn" data-act="pick-targets" type="button">Choose…</button></div>' + note
    );
  }

  function openTargetDialog(row) {
    sdTargetDraft = {
      rowId: row.id,
      // A copy, so Cancel is a real cancel rather than a no-op over state
      // the checkboxes already mutated.
      selected: (row.targets || []).slice(),
      filter: "",
    };
    sdEl("sd-targets-filter").value = "";
    renderTargetDialog();
    sdEl("sd-targets-dialog").style.display = "block";
    sdEl("sd-targets-backdrop").style.display = "block";
    sdEl("sd-targets-filter").focus();
  }

  function closeTargetDialog() {
    sdTargetDraft = null;
    sdEl("sd-targets-dialog").style.display = "none";
    sdEl("sd-targets-backdrop").style.display = "none";
  }

  /* Groups the subscribable list by service, preserving the order discovery
   * returned it in rather than sorting — that order is the DUT's own, and a
   * picker that reorders it stops matching what `Discover GATT` printed.
   *
   * The service UUID travels with the group, so every option rendered under
   * a heading carries the same pair `sdCollectRows` resolves at submit time:
   * a row's two UUIDs cannot disagree (decision 15). */
  function sdTargetGroups(filter) {
    var needle = (filter || "").trim().toLowerCase();
    var groups = [];
    sdSubscribable.forEach(function (c) {
      if (needle) {
        var hay = [
          charLabel(c.characteristic_uuid),
          charTitle(c.characteristic_uuid, c.service_uuid),
          serviceLabel(c.service_uuid),
          c.service_uuid,
        ].join(" ").toLowerCase();
        if (hay.indexOf(needle) < 0) return;
      }
      var group = groups.find(function (g) { return g.serviceUuid === c.service_uuid; });
      if (!group) {
        group = { serviceUuid: c.service_uuid, items: [] };
        groups.push(group);
      }
      group.items.push(c);
    });
    return groups;
  }

  function renderTargetDialog() {
    if (!sdTargetDraft) return;
    var selected = sdTargetDraft.selected;
    var atCap = selected.length >= sdMaxTargets;
    var groups = sdTargetGroups(sdTargetDraft.filter);

    var html = groups
      .map(function (group) {
        var allPicked = group.items.every(function (c) {
          return selected.indexOf(c.characteristic_uuid) >= 0;
        });
        var nonePicked = group.items.every(function (c) {
          return selected.indexOf(c.characteristic_uuid) < 0;
        });
        var rows = group.items
          .map(function (c) {
            var on = selected.indexOf(c.characteristic_uuid) >= 0;
            // Rendered but unpickable at the cap. Hiding it would look like
            // discovery had lost the characteristic.
            var blocked = !on && atCap;
            return (
              '<label class="sd-targets-row' + (blocked ? " disabled" : "") + '" title="' +
              escapeHtml(charTitle(c.characteristic_uuid, c.service_uuid)) + '">' +
              '<input type="checkbox" data-target-uuid="' +
              escapeHtml(c.characteristic_uuid) + '"' + (on ? " checked" : "") +
              (blocked ? " disabled" : "") + " />" +
              '<span class="sd-targets-name">' + escapeHtml(charLabel(c.characteristic_uuid)) +
              "</span>" +
              '<span class="placeholder-note">' + escapeHtml(propsLabel(c.properties)) + " · " +
              (c.live ? "live" : "from source") + "</span></label>"
            );
          })
          .join("");
        return (
          '<div class="sd-targets-group"><div class="sd-targets-group-head">' +
          '<span class="sd-targets-group-name" title="' +
          escapeHtml(serviceTitle(group.serviceUuid)) + '">' +
          escapeHtml(serviceLabel(group.serviceUuid)) + "</span>" +
          '<button class="sd-targets-bulk" type="button" data-bulk="all" data-service="' +
          // Also disabled at the cap: an enabled control that would add
          // nothing is a small lie about what the cap allows.
          escapeHtml(group.serviceUuid) + '"' + (allPicked || atCap ? " disabled" : "") +
          ">all</button>" +
          '<button class="sd-targets-bulk" type="button" data-bulk="none" data-service="' +
          escapeHtml(group.serviceUuid) + '"' + (nonePicked ? " disabled" : "") + ">none</button>" +
          "</div>" + rows + "</div>"
        );
      })
      .join("");

    sdEl("sd-targets-list").innerHTML =
      html ||
      '<p class="placeholder-note" style="padding:14px;">nothing matches that filter</p>';
    syncTargetDialog();
  }

  /* Brings the already-built list in line with the draft **without rebuilding
   * it** — checked/disabled states, the counter, the note.
   *
   * Not an optimization: replacing the list's `innerHTML` on every click
   * destroys the node that was just clicked, so `document.activeElement`
   * falls back to `<body>` and a keyboard user pressing Space on a checkbox
   * is returned to the top of the tab order. Measured against the running
   * app before this existed. (The inline checkbox list decision 17 replaces
   * had the same flaw and worse — it re-rendered the entire step table.) */
  function syncTargetDialog() {
    if (!sdTargetDraft) return;
    var selected = sdTargetDraft.selected;
    var atCap = selected.length >= sdMaxTargets;

    sdEl("sd-targets-list")
      .querySelectorAll("input[type=checkbox][data-target-uuid]")
      .forEach(function (box) {
        var on = selected.indexOf(box.dataset.targetUuid) >= 0;
        var blocked = !on && atCap;
        box.checked = on;
        box.disabled = blocked;
        box.closest(".sd-targets-row").classList.toggle("disabled", blocked);
      });

    sdEl("sd-targets-list").querySelectorAll("button[data-bulk]").forEach(function (btn) {
      var items = sdSubscribable.filter(function (c) {
        return c.service_uuid === btn.dataset.service;
      });
      if (btn.dataset.bulk === "all") {
        var allPicked = items.every(function (c) {
          return selected.indexOf(c.characteristic_uuid) >= 0;
        });
        btn.disabled = allPicked || atCap;
      } else {
        btn.disabled = items.every(function (c) {
          return selected.indexOf(c.characteristic_uuid) < 0;
        });
      }
    });

    sdEl("sd-targets-count").textContent =
      selected.length + " of " + sdSubscribable.length + " · max " + sdMaxTargets;

    var note = "";
    if (atCap) {
      note =
        "at the cap of " + sdMaxTargets +
        " — a study wanting more than this wants GattMonitorAll, which is what that action is for";
    } else if (!selected.length) {
      note = 'an empty selection is refused, never treated as "everything"';
    }
    var unknown = selected.filter(function (uuid) {
      return !sdSubscribable.some(function (c) { return c.characteristic_uuid === uuid; });
    });
    if (unknown.length) {
      note +=
        (note ? " · " : "") + unknown.length + " picked characteristic" +
        (unknown.length === 1 ? " is" : "s are") +
        " no longer in discovery (" + unknown.map(shortUuid).join(", ") +
        ") and Done will drop " + (unknown.length === 1 ? "it" : "them");
    }
    sdEl("sd-targets-note").textContent = note;
  }

  function onTargetDialogChange(ev) {
    if (!sdTargetDraft) return;
    var uuid = ev.target.dataset && ev.target.dataset.targetUuid;
    if (!uuid) return;
    var at = sdTargetDraft.selected.indexOf(uuid);
    if (ev.target.checked && at < 0) sdTargetDraft.selected.push(uuid);
    else if (!ev.target.checked && at >= 0) sdTargetDraft.selected.splice(at, 1);
    // Patched, not rebuilt — keeps the clicked checkbox focused.
    syncTargetDialog();
  }

  function onTargetDialogClick(ev) {
    if (!sdTargetDraft) return;
    var btn = ev.target.closest("button[data-bulk]");
    if (!btn) return;
    ev.preventDefault();
    var service = btn.dataset.service;
    var items = sdSubscribable.filter(function (c) { return c.service_uuid === service; });
    if (btn.dataset.bulk === "none") {
      sdTargetDraft.selected = sdTargetDraft.selected.filter(function (uuid) {
        return !items.some(function (c) { return c.characteristic_uuid === uuid; });
      });
    } else {
      items.forEach(function (c) {
        // Stops at the cap rather than overshooting it and having the
        // server refuse the study afterwards.
        if (sdTargetDraft.selected.length >= sdMaxTargets) return;
        if (sdTargetDraft.selected.indexOf(c.characteristic_uuid) < 0) {
          sdTargetDraft.selected.push(c.characteristic_uuid);
        }
      });
    }
    syncTargetDialog();
  }

  function commitTargetDialog() {
    if (!sdTargetDraft) return;
    var row = sdRows.find(function (r) { return r.id === sdTargetDraft.rowId; });
    if (row) {
      // Written back in the order `sdSubscribable` lists them, not in click
      // order, so two rows picking the same set produce the same study.
      var picked = sdTargetDraft.selected;
      row.targets = sdSubscribable
        .filter(function (c) { return picked.indexOf(c.characteristic_uuid) >= 0; })
        .map(function (c) { return c.characteristic_uuid; });
    }
    closeTargetDialog();
    renderSdRows();
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
    // The link's security level at the end of this step
    // (embarch-study-designer/design.md §3 decision 44). Shown on *every*
    // step, not only a security one, and that is the point: the same
    // failure at L1 and at L4 are different findings, and this column is
    // the only place a reader can tell them apart. Rendered verbatim from
    // the server's own value — this file never maps a level to a claim
    // about it.
    if (step.security_level) {
      parts.push(String(step.security_level).toUpperCase());
    }
    return parts.length ? escapeHtml(parts.join(" · ")) : '<span class="placeholder-note">—</span>';
  }

  // Decision 11: a result renders **how** each version was established, not
  // just what it was. `verified` is decided server-side by
  // `VersionSource::is_verified` — re-deriving it here is the easiest place to
  // accidentally reintroduce the exact defect decision 40 exists to close, so
  // this file never looks at which variant it is, only at the boolean.
  function provCell(what, version, source, verified) {
    return (
      '<div class="prov-cell ' + (verified ? "prov-verified" : "prov-unverified") + '">' +
      '<div class="prov-what">' + escapeHtml(what) + "</div>" +
      '<div class="prov-version">' + escapeHtml(version || "—") + "</div>" +
      '<div class="prov-source">' + escapeHtml(source || "") +
      (verified ? "" : " · unverified") + "</div></div>"
    );
  }

  function renderProvenance(prov) {
    var el = sdEl("sd-provenance");
    if (!prov) {
      el.style.display = "none";
      return;
    }
    var overrides = prov.overrides || [];
    el.style.display = "block";
    el.innerHTML =
      '<div class="card-title" style="margin-bottom:8px;">What this run actually ran against</div>' +
      '<div class="prov-grid">' +
      provCell("dev-bench", prov.dev_bench_version, prov.dev_bench_source, prov.dev_bench_verified) +
      provCell("DUT firmware", prov.firmware_version, prov.firmware_source, prov.firmware_verified) +
      "</div>" +
      (overrides.length
        ? '<div class="sd-error" style="margin-top:12px;">' +
          overrides
            .map(function (o) {
              // Both strings, because the whole content of an override is the
              // gap between them.
              return (
                "This run was allowed past <span class=\"mono\">" + escapeHtml(o.subject) +
                '</span>: it required <span class="mono">' + escapeHtml(o.required) +
                '</span> and ran against <span class="mono">' + escapeHtml(o.actual) + "</span>."
              );
            })
            .join("<br>") +
          "</div>"
        : "");
  }

  // A completed run's taps, with a link straight into the Trace view for an
  // outpost one — the Trace tab is post-hoc and takes a study_id, so handing
  // it over from here is the difference between a view somebody can reach and
  // one they have to copy a UUID into.
  function renderRunStreams(studyId, streams) {
    var el = sdEl("sd-run-streams");
    if (!streams || !streams.length) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.innerHTML =
      '<div class="card-title" style="margin-bottom:8px;">Captured streams</div>' +
      '<table class="data-table"><thead><tr><th>Tap</th><th>Bytes</th><th>Complete</th><th></th>' +
      "</tr></thead><tbody>" +
      streams
        .map(function (ref) {
          return (
            '<tr><td class="mono">' + escapeHtml(ref.name) + "</td>" +
            '<td class="mono">' + ref.bytes_written + "</td>" +
            "<td>" +
            (ref.truncated
              ? '<span class="badge badge-warning">short of what the source produced</span>'
              : '<span class="badge badge-success">complete</span>') +
            "</td>" +
            '<td style="text-align:right;"><button class="btn" data-open-trace="' +
            escapeHtml(ref.name) + '" data-open-study="' + escapeHtml(studyId || "") +
            '">Open in Trace</button></td></tr>'
          );
        })
        .join("") +
      "</tbody></table>";
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
    sdEl("sd-provenance").style.display = "none";
    sdEl("sd-run-streams").style.display = "none";

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
    renderProvenance(state.provenance);
    renderRunStreams(state.study_id, result.streams);
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

  // --- decision 11: `requires`, taps, and the mismatch shown before a run --
  //
  // The one string that means "deliberately unconstrained" is not written
  // here: it comes from the server (`REQUIREMENT_ANY`), so the suite has one
  // definition of it rather than a copy in JavaScript that could drift.
  var sdAnyLiteral = "any";
  var sdTaps = [];

  function sdReqFields() {
    return [
      { any: "sd-req-bench-any", input: "sd-req-bench", live: "sd-req-bench-live", key: "dev_bench" },
      { any: "sd-req-dut-any", input: "sd-req-dut", live: "sd-req-dut-live", key: "dut" },
    ];
  }

  // An "any build" tick takes the field over, visibly: the input keeps showing
  // the literal and goes disabled rather than being cleared or hidden, so the
  // checkbox *is* the statement instead of a way of not making one.
  function sdSyncReqAny() {
    sdReqFields().forEach(function (f) {
      var checked = sdEl(f.any).checked;
      var input = sdEl(f.input);
      input.disabled = checked;
      if (checked) {
        input.dataset.stated = input.dataset.stated || input.value;
        input.value = sdAnyLiteral;
      } else if (input.value === sdAnyLiteral) {
        input.value = input.dataset.stated || "";
      }
    });
  }

  function sdRequiresPayload() {
    return {
      dev_bench_version: sdEl("sd-req-bench").value.trim(),
      firmware_version: sdEl("sd-req-dut").value.trim(),
    };
  }

  function sdApplyRequires(requires) {
    var pairs = [
      ["sd-req-bench", "sd-req-bench-any", requires.dev_bench_version],
      ["sd-req-dut", "sd-req-dut-any", requires.firmware_version],
    ];
    pairs.forEach(function (pair) {
      var isAny = pair[2] === sdAnyLiteral;
      sdEl(pair[1]).checked = isAny;
      var input = sdEl(pair[0]);
      input.value = pair[2] || "";
      if (!isAny) input.dataset.stated = pair[2] || "";
    });
    sdSyncReqAny();
  }

  // Prefilling is what makes a mandatory field a help rather than a tax: the
  // common case is "the builds currently in front of me", and typing a hash by
  // hand to say that would guarantee people paste `any` to get past it,
  // defeating the decision this field exists for.
  async function sdLoadBenchState(prefill) {
    var resp = await fetch("/api/study-designer/bench-state");
    if (!resp.ok) return;
    var state = await resp.json();
    if (state.any) sdAnyLiteral = state.any;

    var rows = [
      { live: "sd-req-bench-live", input: "sd-req-bench", any: "sd-req-bench-any",
        value: state.dev_bench, error: state.dev_bench_error,
        how: "read back off the bench over HelloAck" },
      { live: "sd-req-dut-live", input: "sd-req-dut", any: "sd-req-dut-any",
        value: state.dut, error: state.dut_error,
        how: "what the firmware repo's git describe says" },
    ];
    rows.forEach(function (row) {
      var el = sdEl(row.live);
      if (row.value) {
        el.classList.remove("req-live-bad");
        el.textContent = "live: " + row.value + " — " + row.how;
        el.title = row.how;
        if (prefill && !sdEl(row.any).checked && !sdEl(row.input).value.trim()) {
          sdEl(row.input).value = row.value;
          sdEl(row.input).dataset.stated = row.value;
        }
      } else {
        // Unavailable is not the same as "any", and must not prefill as one.
        el.classList.add("req-live-bad");
        el.textContent = "unavailable: " + (row.error || "no reason given");
        el.title = row.error || "";
      }
    });
  }

  // --- tap rows -----------------------------------------------------------

  function sdSignalNames() {
    return ((latestSnapshot && latestSnapshot.signals) || []).map(function (sig) {
      return sig.name;
    });
  }

  /* One outpost-trace tap row: a topology-declared signal, and an encoding
   * that is the one thing a trace tap can be. */
  function sdOutpostTapCells(tap, i) {
    var signals = sdSignalNames();
    var options = signals.length
      ? signals
          .map(function (name) {
            return (
              '<option value="' + escapeHtml(name) + '"' +
              (name === tap.signal ? " selected" : "") + ">" + escapeHtml(name) + "</option>"
            );
          })
          .join("")
      : "";
    // A tap whose signal is no longer declared keeps showing the name it
    // was authored against rather than silently snapping to another one —
    // Core will reject the study, and the row is where a human sees why.
    if (tap.signal && signals.indexOf(tap.signal) === -1) {
      options =
        '<option value="' + escapeHtml(tap.signal) + '" selected>' + escapeHtml(tap.signal) +
        " — not declared</option>" + options;
    }
    return (
      '<td><select class="sd-input mono" data-tap-signal="' + i + '">' +
      (signals.length || tap.signal ? options : '<option value="">no signal declared — declare one in Topology</option>') +
      "</select></td>" +
      '<td class="mono placeholder-note">OutpostTrace · WholeStudy — an outpost capture is ' +
      "study-scoped with no live feed, and this is the one thing a trace tap can be</td>"
    );
  }

  /* One GATT-notify tap row (embarch-study-designer/design.md §3 decisions
   * 52/55): which characteristic's notifications get their own file, and
   * which declared layout — if any — renders them as columns.
   *
   * "raw bytes" is the honest default and stays selectable. A payload nobody
   * has described gets a `.bin` and no CSV, rather than a CSV of numbers
   * this tool guessed the width of. */
  function sdGattTapCells(tap, i) {
    var options = sdSubscribable
      .map(function (c) {
        var sel = c.characteristic_uuid === tap.characteristic_uuid ? " selected" : "";
        return (
          '<option value="' + escapeHtml(c.characteristic_uuid) + '"' + sel +
          ' title="' + escapeHtml(charTitle(c.characteristic_uuid, c.service_uuid)) + '">' +
          escapeHtml(charLabel(c.characteristic_uuid) + " · " + propsLabel(c.properties)) +
          "</option>"
        );
      })
      .join("");
    if (!sdSubscribable.length) {
      options =
        '<option value="">no notify-capable characteristic known — run Discover GATT</option>';
    } else if (
      tap.characteristic_uuid &&
      !sdSubscribable.some(function (c) { return c.characteristic_uuid === tap.characteristic_uuid; })
    ) {
      // Same rule the signal column follows: keep what was authored, say it
      // isn't there, let the server refuse it.
      options =
        '<option value="' + escapeHtml(tap.characteristic_uuid) + '" selected>' +
        escapeHtml(charLabel(tap.characteristic_uuid)) + " — not discovered</option>" + options;
    }

    var decoders =
      '<option value="">raw bytes — no layout declared</option>' +
      sdStructLayouts
        .map(function (name) {
          return (
            '<option value="' + escapeHtml(name) + '"' +
            (name === tap.decoder ? " selected" : "") + ">" + escapeHtml(name) + "</option>"
          );
        })
        .join("");

    return (
      '<td><select class="sd-input mono" data-tap-char="' + i + '">' + options + "</select></td>" +
      '<td><select class="sd-input mono" data-tap-decoder="' + i + '">' + decoders + "</select>" +
      '<div class="placeholder-note" style="margin-top:4px;">' +
      (sdStructLayouts.length
        ? "layouts come from the firmware repo's <span class=\"mono\">embarch/study-structs.toml</span>"
        : "no layout declared in this repo's <span class=\"mono\">embarch/study-structs.toml</span> yet — this tap captures raw bytes") +
      "</div></td>"
    );
  }

  function renderSdTaps() {
    var tbody = sdEl("sd-taps");
    if (!tbody) return;
    tbody.innerHTML = sdTaps
      .map(function (tap, i) {
        var isGatt = tap.kind === "gatt_notify";
        return (
          "<tr><td>" + (i + 1) + "</td>" +
          '<td><input class="sd-input mono" data-tap-name="' + i + '" value="' +
          escapeHtml(tap.name) + '" spellcheck="false" /></td>' +
          (isGatt ? sdGattTapCells(tap, i) : sdOutpostTapCells(tap, i)) +
          '<td style="text-align:right;"><button class="sd-icon-btn" data-tap-remove="' + i +
          '" title="Remove this tap">✕</button></td></tr>'
        );
      })
      .join("");
    sdEl("sd-taps-empty").style.display = sdTaps.length ? "none" : "block";
  }

  function initSdTaps() {
    var tbody = sdEl("sd-taps");
    if (!tbody) return;
    sdEl("sd-add-tap").addEventListener("click", function () {
      var signals = sdSignalNames();
      sdTaps.push({
        kind: "outpost",
        name: "outpost",
        signal: signals.length ? signals[0] : "",
      });
      renderSdTaps();
    });
    sdEl("sd-add-gatt-tap").addEventListener("click", function () {
      var first = sdSubscribable[0];
      sdTaps.push({
        kind: "gatt_notify",
        // Named after the characteristic rather than "gatt": this is the
        // file name, and a study capturing two characteristics needs two
        // names an author can tell apart at a glance.
        // Sliced to MAX_STREAM_NAME_LEN: a name is a file name here, and a
        // long identifier would be refused by `build_study` at submit time
        // rather than here, where it was chosen.
        name: first ? charLabel(first.characteristic_uuid).slice(0, 32) : "notify",
        service_uuid: first ? first.service_uuid : "",
        characteristic_uuid: first ? first.characteristic_uuid : "",
        decoder: "",
      });
      renderSdTaps();
    });
    tbody.addEventListener("input", function (ev) {
      var i = ev.target.getAttribute("data-tap-name");
      if (i !== null) sdTaps[Number(i)].name = ev.target.value;
    });
    tbody.addEventListener("change", function (ev) {
      var i = ev.target.getAttribute("data-tap-signal");
      if (i !== null) {
        sdTaps[Number(i)].signal = ev.target.value;
        return;
      }
      var c = ev.target.getAttribute("data-tap-char");
      if (c !== null) {
        var tap = sdTaps[Number(c)];
        tap.characteristic_uuid = ev.target.value;
        // The service comes from the same discovery entry the option was
        // rendered from, so a tap's two UUIDs can never disagree — the
        // author never types or picks a service at all.
        var found = sdSubscribable.find(function (x) {
          return x.characteristic_uuid === ev.target.value;
        });
        if (found) tap.service_uuid = found.service_uuid;
        renderSdTaps();
        return;
      }
      var d = ev.target.getAttribute("data-tap-decoder");
      if (d !== null) {
        sdTaps[Number(d)].decoder = ev.target.value;
        renderSdTaps();
      }
    });
    tbody.addEventListener("click", function (ev) {
      var btn = ev.target.closest("[data-tap-remove]");
      if (!btn) return;
      sdTaps.splice(Number(btn.getAttribute("data-tap-remove")), 1);
      renderSdTaps();
    });
  }

  // --- the pre-run check --------------------------------------------------

  function mismatchRow(what, field) {
    if (field.satisfied === null || field.satisfied === undefined) {
      // Unreadable is not a mismatch, and rendering it as one would be a
      // claim about a discrepancy nobody established.
      return (
        '<div class="prov-cell prov-unverified"><div class="prov-what">' + escapeHtml(what) +
        '</div><div class="prov-version">requires ' + escapeHtml(field.required) + "</div>" +
        '<div class="prov-source">actual version unreadable</div>' +
        '<p class="placeholder-note" style="margin:6px 0 0;">' +
        escapeHtml(field.unavailable || "no reason given") + "</p></div>"
      );
    }
    var ok = field.satisfied;
    return (
      '<div class="prov-cell ' + (ok ? "prov-verified" : "prov-unverified") + '">' +
      '<div class="prov-what">' + escapeHtml(what) + "</div>" +
      '<div class="prov-version">requires ' + escapeHtml(field.required) + "</div>" +
      '<div class="prov-version">actual&nbsp;&nbsp; ' + escapeHtml(field.actual || "") + "</div>" +
      '<div class="prov-source">' + (ok ? "satisfied" : "does not match") + "</div></div>"
    );
  }

  function closeRunCheck() {
    sdEl("sd-runcheck-backdrop").style.display = "none";
    sdEl("sd-runcheck-dialog").style.display = "none";
  }

  // Decision 11: the mismatch is shown *before* the run, with both strings, so
  // the choice is made against the actual discrepancy rather than in the
  // abstract. Core's gate is still the enforcement point — this reports, it
  // does not decide.
  async function sdOpenRunCheck() {
    var requires = sdRequiresPayload();
    var params = new URLSearchParams(requires).toString();
    var resp = await fetch("/api/study-designer/version-check?" + params);
    if (!resp.ok) {
      // No pre-flight read available: run and let Core's own gate answer,
      // rather than blocking on a check this tab could not perform.
      return sdSubmitRun(false);
    }
    var check = await resp.json();
    var mismatched = check.dev_bench.satisfied === false || check.dut.satisfied === false;
    sdEl("sd-runcheck-body").innerHTML =
      '<div class="prov-grid" style="margin-top:12px;">' +
      mismatchRow("dev-bench", check.dev_bench) +
      mismatchRow("DUT firmware", check.dut) +
      "</div>";
    var allowWrap = sdEl("sd-runcheck-allow-wrap");
    allowWrap.style.display = mismatched ? "flex" : "none";
    sdEl("sd-runcheck-allow").checked = false;
    sdEl("sd-runcheck-backdrop").style.display = "block";
    sdEl("sd-runcheck-dialog").style.display = "block";
  }

  async function sdRunStudy() {
    sdShowBuildError("");
    if (!sdRows.length) return sdShowBuildError("a study needs at least one step");
    try {
      sdCollectRows();
    } catch (e) {
      return sdShowBuildError(e.message);
    }
    var requires = sdRequiresPayload();
    if (!requires.dev_bench_version || !requires.firmware_version) {
      return sdShowBuildError(
        "state the builds this study is for, or tick \"any build\" — a blank field is the " +
        "not-thought-about case, which is exactly what these fields exist to rule out"
      );
    }
    return sdOpenRunCheck();
  }

  async function sdSubmitRun(allowMismatch) {
    closeRunCheck();
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
        body: JSON.stringify({
          name: sdEl("sd-name").value.trim() || "untitled-study",
          rows: rows,
          requires: sdRequiresPayload(),
          taps: sdTaps,
          // A run parameter, never a study field: a saved study must not carry
          // a waiver into every later re-read of its own results.
          allow_version_mismatch: !!allowMismatch,
        }),
      });
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
    } catch (e) {
      return sdShowBuildError(String(e));
    } finally {
      btn.disabled = false;
    }
  }

  // --- projects (design.md §3 decision 14) -------------------------------
  //
  // The tab used to decide once, at first paint, whether it was usable at
  // all: `[study_designer]` absent in config meant every route answered 404
  // and this file rendered a "Not configured" card and stopped. That was the
  // right call over *guessing* which firmware repo was meant, and it is not
  // being reversed — an explicit pick is not a guess. What changed is that
  // "no project" is now a state with a way out of it.

  // Listeners are wired exactly once, on the first project opened. Re-wiring
  // them on every switch would leave one live handler per project opened this
  // session, so a click would fire N times — the class of bug a
  // re-initialisable panel invites and the reason this flag exists rather
  // than the wiring living inside the load path.
  var sdWired = false;

  function sdProjectSummary(state) {
    if (!state.path) {
      return "No project open. Open a firmware repo above, or set " +
        "[study_designer].firmware_repo_path in embarch-ui's config.";
    }
    var s = state.survey || {};
    var bits = [];
    // "no studies yet" and "not a repo" are deliberately different sentences:
    // an empty `embarch/` is a first-time state, and reading it as a fault is
    // exactly the confusion the server's own survey exists to prevent.
    if (!s.has_embarch_config) {
      bits.push("no embarch/ config yet — the first save creates it");
    } else {
      bits.push(s.saved_studies + (s.saved_studies === 1 ? " saved study" : " saved studies"));
      bits.push(s.has_action_registry ? "action registry present" : "no action registry yet");
    }
    if (!s.is_git_repo) bits.push("not a git checkout");
    if (state.static_extractor) bits.push("static extractor: " + state.static_extractor);
    return state.path + " — " + bits.join(" · ");
  }

  function sdRenderProject(state) {
    var current = sdEl("sd-project-current");
    current.textContent = sdProjectSummary(state);
    current.className = state.path ? "placeholder-note mono" : "sd-error";

    var select = sdEl("sd-project-recents");
    select.innerHTML = '<option value="">Recent projects…</option>';
    (state.recents || []).forEach(function (r) {
      var opt = document.createElement("option");
      opt.value = r.path;
      opt.textContent = r.path + (r.static_extractor ? " (" + r.static_extractor + ")" : "");
      opt.dataset.extractor = r.static_extractor || "";
      select.appendChild(opt);
    });

    // Opened straight away when nothing is open, so the first thing an
    // engineer sees on a fresh machine is the field they need rather than a
    // button they have to find.
    sdEl("sd-project-picker").style.display = state.path ? "none" : "";
    if (state.path && !sdEl("sd-project-path").value) {
      sdEl("sd-project-path").value = state.path;
      sdEl("sd-project-extractor").value = state.static_extractor || "";
    }
  }

  async function sdLoadProject() {
    try {
      var resp = await fetch("/api/study-designer/project");
      var state = await resp.json();
      sdRenderProject(state);
      if (state.path) {
        await sdEnterProject();
      } else {
        sdEl("sd-body").style.display = "none";
      }
    } catch (e) {
      sdEl("sd-project-current").textContent = "couldn't read the project state: " + String(e);
      sdEl("sd-project-current").className = "sd-error";
    }
  }

  async function sdOpenProject(path, extractor) {
    var err = sdEl("sd-project-error");
    err.style.display = "none";
    var body = {
      path: path !== undefined ? path : sdEl("sd-project-path").value,
      static_extractor: (extractor !== undefined ? extractor : sdEl("sd-project-extractor").value) || null,
    };
    var btn = sdEl("sd-project-open");
    btn.disabled = true;
    try {
      var resp = await fetch("/api/study-designer/project", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      var text = await resp.text();
      if (!resp.ok) {
        err.textContent = text;
        err.style.display = "block";
        return;
      }
      var state = JSON.parse(text);
      sdRenderProject(state);
      // Everything the previous project put on screen is about the previous
      // project: the table, the taps, the saved-study list and the last run.
      // The server drops its own per-project caches on the same switch.
      sdRows = [];
      sdTaps = [];
      sdLastStudyId = null;
      await sdEnterProject();
    } catch (e) {
      err.textContent = String(e);
      err.style.display = "block";
    } finally {
      btn.disabled = false;
    }
  }

  async function sdNewStudy() {
    var name = (sdEl("sd-name").value || "").trim();
    if (!name) return sdShowBuildError("give the study a name first");
    var btn = sdEl("sd-new-study");
    btn.disabled = true;
    try {
      var resp = await fetch("/api/study-designer/new-study", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: name }),
      });
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
      // The file the server wrote is already a valid, runnable, loadable
      // `Study` — so the table is reset to empty rather than to the
      // capture-window template, and what is on screen matches what is on
      // disk.
      sdRows = [];
      sdTaps = [];
      renderSdRows();
      renderSdTaps();
      sdShowBuildError("");
      await loadSdStudies();
      sdEl("sd-load-select").value = JSON.parse(text).slug;
    } catch (e) {
      sdShowBuildError(String(e));
    } finally {
      btn.disabled = false;
    }
  }

  /* Everything that depends on a project being open, run again on every
   * switch. `sdWireStudyDesigner` is the half that must not be. */
  async function sdEnterProject() {
    var body = sdEl("sd-body");
    var resp;
    try {
      resp = await fetch("/api/study-designer/actions");
    } catch (e) {
      body.style.display = "none";
      return;
    }
    if (!resp.ok) {
      body.style.display = "none";
      return;
    }
    body.style.display = "block";
    var data = await resp.json();
    sdActions = data.actions || [];
    sdRegistry = sdRegisteredActions();
    sdAdoptDiscovery(data);
    if (!sdWired) {
      sdWireStudyDesigner();
      sdWired = true;
    }
    if (!sdRows.length) sdRows = sdCaptureTemplate();
    renderSdRows();
    renderSdUnregistered();
    loadSdStudies();
    renderSdTaps();
    // Prefilled from live bench state on first paint, which is what makes a
    // mandatory field a help rather than a tax.
    sdLoadBenchState(true);
  }

  function sdWireStudyDesigner() {
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
    sdReqFields().forEach(function (f) {
      sdEl(f.any).addEventListener("change", sdSyncReqAny);
    });
    sdEl("sd-req-refresh").addEventListener("click", function () {
      sdLoadBenchState(false);
    });
    sdEl("sd-targets-cancel").addEventListener("click", closeTargetDialog);
    sdEl("sd-targets-backdrop").addEventListener("click", closeTargetDialog);
    sdEl("sd-targets-done").addEventListener("click", commitTargetDialog);
    sdEl("sd-targets-list").addEventListener("change", onTargetDialogChange);
    sdEl("sd-targets-list").addEventListener("click", onTargetDialogClick);
    sdEl("sd-targets-filter").addEventListener("input", function (ev) {
      if (!sdTargetDraft) return;
      sdTargetDraft.filter = ev.target.value;
      renderTargetDialog();
    });
    sdEl("sd-runcheck-cancel").addEventListener("click", closeRunCheck);
    sdEl("sd-runcheck-backdrop").addEventListener("click", closeRunCheck);
    sdEl("sd-runcheck-go").addEventListener("click", function () {
      sdSubmitRun(sdEl("sd-runcheck-allow").checked);
    });
    initSdTaps();
    sdEl("sd-run-streams").addEventListener("click", function (ev) {
      var btn = ev.target.closest("[data-open-trace]");
      if (!btn) return;
      // Sets the address as well as the field, so what the button does and
      // what a shared link does are the same one thing.
      location.hash =
        "trace?study=" + encodeURIComponent(btn.getAttribute("data-open-study")) +
        "&tap=" + encodeURIComponent(btn.getAttribute("data-open-trace"));
      document.getElementById("trace-study").value = btn.getAttribute("data-open-study");
      showTab("trace");
      document.getElementById("trace-load").click();
    });
    sdEl("sd-new-study").addEventListener("click", sdNewStudy);
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
    // design.md §3 decision 6's suite-wide SSE convergence. One stream for
    // the process's lifetime: `RunState` is the server's, not a project's,
    // so switching projects doesn't reopen it.
    sdRunSource = new EventSource("/api/study-designer/events");
    sdRunSource.addEventListener("run", function (ev) {
      try {
        renderRunState(JSON.parse(ev.data));
      } catch (e) {
        /* a malformed frame shouldn't kill the stream */
      }
    });
  }

  function initStudyDesignerTab() {
    var body = sdEl("sd-body");
    if (!body) return;

    sdEl("sd-project-toggle").addEventListener("click", function () {
      var picker = sdEl("sd-project-picker");
      picker.style.display = picker.style.display === "none" ? "" : "none";
    });
    sdEl("sd-project-open").addEventListener("click", function () {
      sdOpenProject();
    });
    sdEl("sd-project-recents").addEventListener("change", function (ev) {
      var opt = ev.target.selectedOptions[0];
      if (!opt || !opt.value) return;
      sdEl("sd-project-path").value = opt.value;
      sdEl("sd-project-extractor").value = opt.dataset.extractor || "";
      // Picking a recent project opens it: an extra click on Open would be
      // asking twice for the same decision.
      sdOpenProject(opt.value, opt.dataset.extractor || "");
    });

    sdLoadProject();
  }

  // --- signal routes (design.md §3 decision 10, first half) --------------
  //
  // Every write goes through embarch-ui's own `/api/signals`, which proxies
  // Core over HTTP+Bearer — never `embarch_topology::hardware::declare_signal`
  // in-process (decision 5), and never the browser holding Core's token.

  function sigEl(id) {
    return document.getElementById(id);
  }

  function sigSyncRouteFields() {
    var direct = sigEl("sig-route").value === "direct";
    sigEl("sig-direct-fields").style.display = direct ? "" : "none";
    sigEl("sig-bench-fields").style.display = direct ? "none" : "";
    sigEl("sig-note").textContent = direct
      ? "A direct route bypasses dev-bench entirely, which is what the outpost uses today — for a hardware reason (the bench has no spare pins or pass-through firmware yet), not a design preference. The port list is embarch-core's own enumeration, because a port on this machine is not a port on Core's."
      : "A via-dev-bench route terminates on declared pins and is relayed over dev-bench's existing Core link, passing bytes through and interpreting nothing. Nothing on this bench has the pins for it yet.";
  }

  function sigFillPorts(snapshot) {
    var select = sigEl("sig-port");
    var ports = (snapshot && snapshot.serial_ports) || [];
    var previous = select.value;
    if (!ports.length) {
      // A carrier is declared by USB serial and resolved by it later, so a
      // port with no serial could never be declared as one. Saying that is
      // better than offering a choice nothing could act on.
      select.innerHTML =
        '<option value="">' +
        (snapshot && snapshot.serial_ports_error
          ? "embarch-core did not answer GET /serial-ports"
          : "no USB serial port is enumerated on embarch-core's machine") +
        "</option>";
      return;
    }
    select.innerHTML = ports
      .map(function (p) {
        var serial = p.serial_number || "";
        var label = p.port_name + (p.product ? " — " + p.product : "") +
          (serial ? " · " + serial : " · (no USB serial — cannot be declared)");
        return (
          '<option value="' + escapeHtml(serial) + '"' + (serial ? "" : " disabled") + ">" +
          escapeHtml(label) + "</option>"
        );
      })
      .join("");
    if (previous) select.value = previous;
  }

  function openSignalDialog(existing) {
    sigEl("sig-result").style.display = "none";
    sigEl("sig-name").value = existing ? existing.name : "";
    sigEl("sig-name").readOnly = !!existing;
    sigEl("sig-origin").value = existing ? existing.origin_role : "dut";
    sigEl("sig-direction").value = existing ? existing.direction : "dut-to-host";
    sigEl("sig-route").value = existing && existing.route ? existing.route.kind : "direct";
    sigEl("sig-rx").value = (existing && existing.route && existing.route.rx_pin) || "";
    sigEl("sig-tx").value = (existing && existing.route && existing.route.tx_pin) || "";
    sigFillPorts(latestSnapshot);
    if (existing && existing.route && existing.route.port_serial) {
      sigEl("sig-port").value = existing.route.port_serial;
    }
    sigSyncRouteFields();
    sigEl("sig-save").textContent = existing ? "Move route" : "Declare";
    sigEl("sig-dialog-backdrop").style.display = "block";
    sigEl("sig-dialog").style.display = "block";
  }

  function closeSignalDialog() {
    sigEl("sig-dialog-backdrop").style.display = "none";
    sigEl("sig-dialog").style.display = "none";
  }

  async function submitSignal() {
    var result = sigEl("sig-result");
    var name = sigEl("sig-name").value.trim();
    if (!name) {
      result.style.display = "block";
      result.textContent = "a signal needs the name a study will tap it by";
      return;
    }
    var route;
    if (sigEl("sig-route").value === "direct") {
      var serial = sigEl("sig-port").value;
      if (!serial) {
        result.style.display = "block";
        result.textContent =
          "pick the port carrying this signal. Without a USB serial nothing could resolve the " +
          "route later, which is why a port that reports none cannot be declared.";
        return;
      }
      route = { kind: "direct", port_serial: serial };
    } else {
      var rx = sigEl("sig-rx").value.trim();
      var tx = sigEl("sig-tx").value.trim();
      if (!rx || !tx) {
        result.style.display = "block";
        result.textContent = "name both dev-bench pins this signal terminates on";
        return;
      }
      route = { kind: "via-dev-bench", rx_pin: rx, tx_pin: tx };
    }

    var body = {
      name: name,
      origin_role: sigEl("sig-origin").value.trim() || "dut",
      direction: sigEl("sig-direction").value,
      route: route,
    };
    var resp = await fetch("/api/signals", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    var text = await resp.text();
    if (!resp.ok) {
      result.style.display = "block";
      result.textContent = resp.status + " " + text;
      return;
    }
    closeSignalDialog();
  }

  async function removeSignal(name) {
    var resp = await fetch("/api/signals/" + encodeURIComponent(name), { method: "DELETE" });
    if (!resp.ok) {
      var err = document.getElementById("signals-error");
      err.style.display = "block";
      err.textContent = resp.status + " " + (await resp.text());
    }
  }

  function initSignals() {
    var table = document.getElementById("signals-table-body");
    if (!table) return;
    document.getElementById("sig-declare").addEventListener("click", function () {
      openSignalDialog(null);
    });
    sigEl("sig-cancel").addEventListener("click", closeSignalDialog);
    sigEl("sig-dialog-backdrop").addEventListener("click", closeSignalDialog);
    sigEl("sig-route").addEventListener("change", sigSyncRouteFields);
    sigEl("sig-save").addEventListener("click", submitSignal);
    table.addEventListener("click", function (ev) {
      var edit = ev.target.closest("[data-signal-edit]");
      if (edit) {
        var name = edit.getAttribute("data-signal-edit");
        var sig = ((latestSnapshot && latestSnapshot.signals) || []).find(function (x) {
          return x.name === name;
        });
        // Re-declaring the same name *is* the migration path the decision
        // promises: one call moves the route, and no saved study changes.
        openSignalDialog(sig || { name: name, origin_role: "dut", direction: "dut-to-host" });
        return;
      }
      var remove = ev.target.closest("[data-signal-remove]");
      if (remove) removeSignal(remove.getAttribute("data-signal-remove"));
    });
  }

  // --- Trace view (design.md §3 decision 10, second half) ----------------
  //
  // Every number drawn here was decoded server-side, through
  // `embarch-study-designer`'s own `outpost` module (src/trace.rs). No trace
  // knowledge lives in this file: not the column order, not the record kinds,
  // not what `IRQ_UNKNOWN` means. What lives here is the drawing.

  var traceView = null;

  function trEl(id) {
    return document.getElementById(id);
  }

  function traceShowError(message) {
    var el = trEl("trace-error");
    if (!message) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.textContent = message;
  }

  async function traceLoadTaps() {
    var studyId = trEl("trace-study").value.trim();
    var select = trEl("trace-tap");
    if (!studyId) return traceShowError("give the study_id a run reported");
    traceShowError("");
    var resp = await fetch("/api/trace/" + encodeURIComponent(studyId));
    var text = await resp.text();
    if (!resp.ok) {
      select.disabled = true;
      select.innerHTML = '<option value="">—</option>';
      return traceShowError(resp.status + " " + text);
    }
    var data = JSON.parse(text);
    var taps = (data.taps || []).filter(function (t) { return t.is_outpost_trace; });
    if (!taps.length) {
      select.disabled = true;
      select.innerHTML = '<option value="">no outpost trace tap in this study</option>';
      return traceShowError(
        "this study declared " + (data.taps || []).length + " tap(s), none of them an outpost " +
        "trace. Author one in the Study Designer's Streams card."
      );
    }
    select.disabled = false;
    select.innerHTML = taps
      .map(function (t) {
        return '<option value="' + escapeHtml(t.name) + '">' + escapeHtml(t.name) +
          (t.named ? "" : " — unnamed") + "</option>";
      })
      .join("");
    return traceLoadView();
  }

  async function traceLoadView() {
    var studyId = trEl("trace-study").value.trim();
    var tap = trEl("trace-tap").value;
    if (!studyId || !tap) return;
    traceShowError("");
    var resp = await fetch(
      "/api/trace/" + encodeURIComponent(studyId) + "/" + encodeURIComponent(tap)
    );
    var text = await resp.text();
    if (!resp.ok) {
      trEl("trace-body").style.display = "none";
      trEl("trace-refusal").style.display = "none";
      return traceShowError(resp.status + " " + text);
    }
    traceView = JSON.parse(text);
    renderTrace();
  }

  // An axis value, in whatever unit the view is actually in.
  //
  // Three tiers, and `view.unit` is the only thing to key off (`src/trace.rs`):
  // `"us"` is the DUT's own counter, read per record — microseconds, and what
  // measures; `"ms"` is embarch-core's receipt time per frame — milliseconds,
  // and what places a trace against other streams; `"frame"` is a frame index,
  // a real coordinate, labelled as one. **Nothing here converts between
  // tiers** — they are different clocks, not different scales of one.
  function fmtT(view, t) {
    if (view.unit === "frame") return "frame " + String(t);
    if (view.unit === "us") {
      if (Math.abs(t) >= 1000000) return (t / 1000000).toFixed(3) + " s";
      if (Math.abs(t) >= 1000) return (t / 1000).toFixed(3) + " ms";
      return String(t) + " \u00b5s";
    }
    if (Math.abs(t) >= 1000) return (t / 1000).toFixed(3) + " s";
    return String(t) + " ms";
  }

  // A width or duration, same units. Split from fmtT because "frame 3" is a
  // position and "3 frames" is a length, and a tooltip that says the first
  // where it means the second is how a bound gets read as a measurement.
  function fmtSpanLen(view, t) {
    if (view.unit === "frame") return String(t) + " frame(s)";
    if (view.unit === "us") {
      if (t >= 1000000) return (t / 1000000).toFixed(3) + " s";
      if (t >= 1000) return (t / 1000).toFixed(3) + " ms";
      return String(t) + " \u00b5s";
    }
    if (t >= 1000) return (t / 1000).toFixed(3) + " s";
    return String(t) + " ms";
  }

  // A position on the axis, with **as many digits as the current zoom can
  // distinguish**. `fmtT` picks its tier from the magnitude of `t` alone,
  // which is right for a table and wrong for a zoomed chart: at a 40 µs
  // window, every position in this capture is somewhere in the 88th second,
  // and three decimals renders both ends of the window as the same number.
  // The tier still comes from `t`; only the precision comes from `span`, so
  // an axis never claims a digit finer than the window it is labelling.
  function fmtAxisT(view, t, span, tierRef) {
    if (view.unit === "frame") return "frame " + String(t);
    // The tier comes from the **largest** position on screen rather than from
    // this one, so every label in one drawing carries the same unit: an axis
    // reading "0 µs · 24.577 s · 49.154 s" makes a reader convert in their
    // head to compare its own ends.
    var ref = Math.abs(tierRef === undefined ? t : tierRef);
    var div = 1;
    var suffix = view.unit === "us" ? " \u00b5s" : " ms";
    if (view.unit === "us") {
      if (ref >= 1000000) { div = 1000000; suffix = " s"; }
      else if (ref >= 1000) { div = 1000; suffix = " ms"; }
    } else if (ref >= 1000) {
      div = 1000;
      suffix = " s";
    }
    // The finest tier is already the axis's own integer unit — there is no
    // sub-unit digit to show, and a "123.00 µs" would be inventing one.
    if (div === 1) return String(Math.round(t)) + suffix;
    // Never finer than the axis's own unit either: `Math.log10(div)` is
    // exactly how many decimals of the display unit one axis unit is worth,
    // and a digit past it is a precision the DUT's counter does not have.
    var digits = Math.min(
      Math.log10(div),
      Math.max(3, Math.ceil(-Math.log10(Math.max(1e-9, span / div))) + 3)
    );
    return (t / div).toFixed(digits) + suffix;
  }

  function statCard(label, value, sub, tone) {
    return (
      '<div class="card"><div class="card-title"><span style="font-size:10.5px; font-weight:650; ' +
      'letter-spacing:0.06em; text-transform:uppercase; color:var(--text-tertiary);">' +
      escapeHtml(label) + "</span></div>" +
      '<div class="stat-value"' + (tone ? ' style="color:' + tone + ';"' : "") + ">" +
      escapeHtml(value) + "</div>" +
      '<div class="stat-sub">' + escapeHtml(sub) + "</div></div>"
    );
  }

  // A percentage rendered from a 0..1 fraction, with enough precision that a
  // small-but-real share does not round to "0%".
  function fmtShare(f) {
    var pct = f * 100;
    if (pct > 0 && pct < 0.01) return "<0.01%";
    return pct.toFixed(2) + "%";
  }

  // The load repartition (`src/trace.rs`'s `LoadSummary`). Arithmetic done in
  // Rust against the shared crate's own vocabulary; this only lays it out.
  //
  // **Coverage is rendered above the table, not under it.** The number that
  // decides whether the rest of the card is a measurement is the fraction of
  // the window the firmware said it lost records across, so it is not a
  // footnote.
  function renderTraceLoad(view) {
    var s = view.summary;
    var lossy = s.gap_fraction > 0 || s.records_lost > 0;
    var coverage = trEl("trace-load-coverage");
    var basis =
      view.unit === "us"
        ? "Totals are microseconds of the DUT's own counter, read per record — so both ends of " +
          "every span carry their own stamp and nothing here is below the resolution." +
          (view.dual_clock
            ? " This capture also carries embarch-core's receipt time per frame (" +
              (view.resolution_ms == null ? "unmeasured" : view.resolution_ms + " ms") +
              " apart), which is what places it against the other streams in this study — it is " +
              "not what measures these durations."
            : " Nobody stamped this capture's frames, so it measures itself but cannot be laid " +
              "against another stream in this study.")
        : view.unit === "ms"
          ? "This capture carries no DUT clock, so totals are milliseconds of embarch-core's own " +
            "receipt clock — the same clock every other stream in this study is stamped with. A " +
            "frame is " +
            (view.resolution_ms == null ? "this capture's" : view.resolution_ms + " ms") +
            " wide and nothing inside one frame has a measurable duration."
          : "Neither clock reached this capture, so it has no time base at all: every total below " +
            "counts frames, and every share is a fraction of frames rather than of time. The " +
            "order is real; the durations are not available.";
    // The firmware's own statement that this trace is deliberately incomplete
    // (`embarch-outpost/design.md` §3 decision 19). Rendered against the
    // unaccounted total specifically, because that is the number it explains:
    // without it a reader sees a hole between the idle thread switching out
    // and switching back in, and has nothing to attribute it to.
    var selfNote =
      view.self_excluded === true
        ? " The firmware kept the outpost's own drain thread and its own UART's interrupt out " +
          "of this trace (CONFIG_EMBARCH_OUTPOST_TRACE_SELF=n), so the unaccounted total above " +
          "is largely the instrument's own runs rather than anything unexplained."
        : view.self_excluded === false
          ? " This trace includes the outpost's own drain thread and its own UART's interrupt " +
            "(CONFIG_EMBARCH_OUTPOST_TRACE_SELF=y), so a large part of what it describes is the " +
            "instrument describing its own transmission."
          : "";
    var subFrame = s.below_resolution_spans > 0
      ? " " + s.below_resolution_spans + " span(s) fall below what this capture's clock can " +
        "resolve — they are counted as entries and excluded from every total. On the frame clock " +
        "most ISR spans are this by construction."
      : "";
    coverage.innerHTML =
      '<div class="card" style="border-color:' + (lossy ? "var(--warning)" : "var(--border)") +
      '; padding:12px 14px;">' +
      '<div style="font-weight:650; color:' + (lossy ? "var(--warning)" : "var(--success)") + ';">' +
      (lossy
        ? fmtShare(s.gap_fraction) + " of this window is covered by a reported-loss band"
        : "The firmware reported no losses in this window") +
      "</div>" +
      '<div class="stat-sub" style="margin-top:6px;">' +
      escapeHtml(
        s.records_lost + " record(s) lost across " + view.gaps.length + " band(s). " +
        "Window " + fmtSpanLen(view, s.window_extent) + "; " +
        fmtSpanLen(view, s.thread_extent) + " of it accounted for by measured thread spans, " +
        fmtSpanLen(view, s.unaccounted_extent) + " not. " + basis + subFrame + selfNote
      ) +
      "</div>" +
      (s.idle_record_extent > 0
        ? '<div class="stat-sub" style="margin-top:6px;">' +
          escapeHtml(
            "Cross-check: the cpu-idle records account for " +
            fmtSpanLen(view, s.idle_record_extent) + " (" + fmtShare(s.idle_record_extent / Math.max(1, s.window_extent)) +
            "), reported independently of the idle thread's own switch records. The two measure " +
            "the same time two ways and are not added together; where they disagree, the " +
            "disagreement is the finding."
          ) + "</div>"
        : "") +
      "</div>";

    // `trace-load-rows`, not `trace-load`: the Load *button* held that id
    // first, so this wrote every summary row into the button and the table
    // stayed empty — invisible to a Rust test, obvious the first time the tab
    // was rendered in a browser.
    trEl("trace-load-rows").innerHTML = s.subjects.length
      ? s.subjects
          .map(function (x) {
            var nameCell = x.unnamed
              ? '<td class="mono" style="font-style:italic; color:var(--text-tertiary);" ' +
                'title="the manifest resolved no name for this subject — this is the number the ' +
                'firmware reported">' + escapeHtml(x.label) + "</td>"
              : "<td>" + escapeHtml(x.label) + "</td>";
            var excluded = x.excluded_spans
              ? '<td style="text-align:right;" title="' +
                escapeHtml(
                  x.gap_crossing_spans + " crossing a gap, " + x.open_ended_spans +
                  " with no closing record, " + x.open_started_spans + " with no opening record, " +
                  x.below_resolution_spans + " below the clock's resolution" +
                  " — " + fmtSpanLen(view, x.excluded_extent) + " of extent, not counted as duration"
                ) + '"><span style="color:var(--warning);">' + x.excluded_spans + " span(s)</span></td>"
              : '<td style="text-align:right; color:var(--text-tertiary);">&mdash;</td>';
            return (
              "<tr>" + nameCell +
              '<td class="mono">' + escapeHtml(x.kind) + "</td>" +
              '<td class="mono" style="text-align:right;">' + x.entries + "</td>" +
              '<td class="mono" style="text-align:right;">' + x.measured_spans + "</td>" +
              '<td class="mono" style="text-align:right;">' + escapeHtml(fmtSpanLen(view, x.total_extent)) + "</td>" +
              '<td class="mono" style="text-align:right;">' + escapeHtml(fmtShare(x.share)) + "</td>" +
              excluded + "</tr>"
            );
          })
          .join("")
      : '<tr><td colspan="7" class="placeholder-note">No traced subjects in this capture.</td></tr>';
  }

  function renderTrace() {
    var view = traceView;
    if (!view) return;
    trEl("trace-body").style.display = "block";

    // **The refusal banner, and it is not a warning decorating a named
    // trace.** Decision 10 says an unnamed trace must never read as a named
    // one, so this says the trace is unnamed, says Core's reason verbatim, and
    // the lanes below render as the numbers they are.
    var refusal = trEl("trace-refusal");
    if (!view.named) {
      refusal.style.display = "block";
      refusal.innerHTML =
        '<div class="card-title" style="color:var(--warning);">This trace has no names</div>' +
        '<p class="placeholder-note">embarch-core decoded the capture into a real timeline but did ' +
        "not apply a manifest to it, so every thread, vector and marker below is the number the " +
        "firmware reported and nothing more. The structure is real; the labels are absent, not " +
        "guessed.</p>" +
        '<p class="mono" style="font-size:12px; color:var(--text-secondary);">' +
        escapeHtml(view.note || "embarch-core recorded no reason.") + "</p>";
    } else {
      refusal.style.display = "none";
    }

    var lostTone = view.records_lost > 0 ? "var(--warning)" : "var(--success)";
    trEl("trace-stats").innerHTML =
      statCard("Records", String(view.rows),
        view.rows_dropped_by_cap > 0
          ? view.rows_dropped_by_cap + " more not read — this view caps at 250,000"
          : "every row in the capture") +
      statCard("Records lost", String(view.records_lost),
        view.gaps.length + " gap(s) reported by the firmware", lostTone) +
      statCard("Span", fmtSpanLen(view, view.t_to - view.t_from),
        view.unit === "us"
          ? "the DUT's own counter, " + view.frames + " frames"
          : view.unit === "ms"
            ? "embarch-core's receipt clock, " + view.frames + " frames"
            : "neither clock — " + view.frames + " frames, no time base") +
      statCard("Resolution",
        view.unit === "us" ? "1 \u00b5s" : view.unit === "ms" ? view.resolution_ms + " ms" : "1 frame",
        view.unit === "us"
          ? "per record; " + view.rows + " of " + view.rows + " stamped by the DUT"
          : view.unit === "ms"
            ? "one frame; nothing inside one is measurable"
            : "frame index; nothing has a duration");

    // A backwards arrival stamp means the host clock stepped mid-capture (an
    // NTP correction is the realistic cause). It was unreportable while the
    // axis was the DUT's own monotonic counter; on a wall clock it is a real
    // failure mode, so it is said out loud rather than drawn over.
    // A DUT clock that stepped back further than the capture lasted means the
    // counter restarted mid-capture. Said out loud, because the axis silently
    // became the coarser clock and the reader needs to know which one they are
    // looking at.
    var dutNote = view.dut_clock_refused
      ? " · the DUT's own clock jumped by " +
        (view.dut_step_max_us / 1000000).toFixed(3) + " s between two consecutive records, which " +
        "is longer than this capture lasted — so this trace splices two separate stretches of " +
        "DUT time together and is drawn on embarch-core's clock instead"
      : view.dut_backsteps > 0
        ? " · the DUT's clock inverted " + view.dut_backsteps + " time(s), at most " +
          view.dut_backstep_max_us + " \u00b5s: a hook stamps before it reserves its ring slot, " +
          "so an interrupt preempting a thread can publish after it and be stamped before it"
        : "";
    // **Only on the host's clock.** `out_of_order_rows` counts rows whose
    // *axis position* went backwards, and on the DUT's clock that is the same
    // benign hook-stamp inversion `dutNote` has already explained a line
    // above — saying "this host's clock stepped backwards" about it made one
    // sentence contradict the one before it. On the host's clock it really is
    // the wall clock stepping, which is a different and reportable thing.
    var clockNote = view.unit === "ms" && view.out_of_order_rows > 0
      ? " · " + view.out_of_order_rows + " row(s) arrived with a stamp earlier than the row " +
        "before them, which means this host's clock stepped backwards during the capture — " +
        "positions across that step are not comparable"
      : "";
    trEl("trace-axis-note").textContent = (view.unit === "us"
      ? "the DUT's own cycle counter, read per record — microsecond-exact, and what measures" +
        (view.dual_clock
          ? "; embarch-core's receipt time places this capture against the study's other " +
            "streams to within " + view.resolution_ms + " ms"
          : "; nothing stamped its frames, so it cannot be placed against another stream")
      : view.unit === "ms"
        ? "embarch-core's own receipt time per frame — a frame is " + view.resolution_ms +
          " ms wide, and every record in one shares its instant" +
          (view.undated_rows > 0
            ? "; " + view.undated_rows + " of " + view.rows + " rows carried no DUT stamp, so the " +
              "finer axis is unavailable"
            : "")
        : view.unstamped_rows > 0 && view.timed
          ? view.unstamped_rows + " of " + view.rows + " rows carried no arrival stamp and " +
            view.undated_rows + " carried no DUT stamp, so the axis is frame index rather than " +
            "half a millisecond axis"
          : "neither clock reached this capture, so the axis is frame index: order without " +
            "duration") +
      dutNote + clockNote;

    trEl("trace-gaps").innerHTML = view.gaps.length
      ? view.gaps
          .map(function (g) {
            return (
              "<tr><td>" + fmtT(view, g.from - view.t_from) + "</td>" +
              "<td>" + fmtT(view, g.to - view.t_from) + "</td>" +
              '<td class="mono">' + g.records_lost + "</td>" +
              '<td class="mono">' + g.row_index + "</td></tr>"
            );
          })
          .join("")
      : '<tr><td colspan="4" class="placeholder-note">The firmware reported no losses.</td></tr>';

    trEl("trace-markers").innerHTML = view.markers.length
      ? view.markers
          .map(function (m) {
            return (
              "<tr><td>" + fmtT(view, m.t - view.t_from) + "</td>" +
              '<td class="' + (m.unnamed ? "mono" : "") + '"' +
              (m.unnamed ? ' style="font-style:italic; color:var(--text-tertiary);"' : "") + ">" +
              escapeHtml(m.label) + "</td>" +
              '<td class="mono">' + m.arg + "</td></tr>"
            );
          })
          .join("")
      : '<tr><td colspan="3" class="placeholder-note">No markers in this capture. Markers are ' +
        "opt-in: an application registers them with OUTPOST_MARKERS(X), and an image that " +
        "declares none has nothing to report here. This is not a missing measurement.</td></tr>";

    // The step row's own sentence, whether or not the row can be drawn. When
    // it cannot — no time base, so no axis to project onto — this is where the
    // reader is told, rather than being shown bands at invented positions.
    var stepsNote = trEl("trace-steps-note");
    if (stepsNote) {
      stepsNote.textContent = view.steps
        ? view.steps.note
        : "This study's events.json carries no per-step arrival stamps, so which step was running " +
          "at a given instant cannot be drawn. embarch-core has recorded them since 2026-08-27; a " +
          "capture from before that has no such record to read.";
    }

    renderTraceLoad(view);
    // A freshly loaded view starts at the whole capture with every lane
    // shown and in the order `trace.rs` built them — the window and the lane
    // set are the reader's state, and carrying either across from the last
    // trace they looked at would silently hide part of this one.
    traceResetWindow(view);
    traceResetLanes(view);
    traceRenderLanePanel(view);
    drawTraceChart(view);
  }

  /// The lane-name gutter's floor. The gutter itself is measured per draw
  /// against the *visible* lanes' longest label (`traceGutter`), because a
  /// name that runs off the left edge makes a lane unidentifiable — and a
  /// reader who has just hidden twenty lanes to look at three ISRs should get
  /// the width back that those twenty were forcing.
  var TRACE_GUTTER_MIN = 230;
  var TRACE_GUTTER_MAX = 380;
  var traceGutter = TRACE_GUTTER_MIN;
  var TRACE_AXIS_H = 30;
  var TRACE_ROW_H = 24;
  var TRACE_BAR_H = 13;
  var TRACE_PAD_RIGHT = 14;
  var TRACE_BODY_PAD = 18;
  /// The study-action row's own band height, and the gap under it before the
  /// lanes begin. It lives in the pinned header rather than in the scrolling
  /// body: which step was running is the context for *every* lane, so it must
  /// not scroll away from the lane a reader has scrolled down to.
  var TRACE_STEP_H = 26;
  var TRACE_STEP_GAP = 8;

  // ---- the visible window ---------------------------------------------------
  //
  // Everything below draws one window of the capture rather than the whole of
  // it, and `traceWin` is that window in the view's **own axis units** — DUT
  // microseconds, host milliseconds or frame indices, whichever
  // `view.axis_clock` says. Never a fraction and never pixels: a window stored
  // as a fraction of the capture would have to be re-derived against `t_from`
  // on every draw, and a window stored in pixels would change meaning when the
  // panel resizes.
  var traceWin = null;
  /// Lane order (by `lane.key`) and the hidden set. Both are the reader's, not
  /// the data's — `view.lanes` is never reordered or filtered in place, so the
  /// Load repartition below keeps computing over every lane and a filtered
  /// timeline cannot quietly change a denominator.
  var traceLaneOrder = null;
  var traceHidden = null;
  var traceDrawQueued = false;
  /// Reused per-column aggregation buffers. Allocated once per plot width and
  /// cleared per lane — a fresh `Uint8Array` per lane per frame would allocate
  /// 26 arrays sixty times a second while a reader is dragging.
  var traceScratch = null;

  function traceFullWin(view) {
    return { from: view.t_from, to: Math.max(view.t_from + 1, view.t_to) };
  }

  /// The finest window a reader may zoom to. A floor is needed because the
  /// axis is integers: a window narrower than a few units would put several
  /// pixel columns inside one unit, and the aggregation below would draw a
  /// span's *rounding* rather than its extent. The floors differ per clock
  /// because the units do — 40 µs of the DUT's counter, 4 ms of the host's,
  /// 4 frames when there is no time base at all.
  function traceMinWin(view) {
    if (view.unit === "us") return 40;
    if (view.unit === "ms") return 4;
    return 4;
  }

  /// Clamps a proposed window into the capture. Zoom never goes wider than the
  /// whole capture and pan never leaves it, so there is no way to end up
  /// looking at empty axis and wondering whether the trace stopped.
  function traceClampWin(view, win) {
    var full = traceFullWin(view);
    var extent = full.to - full.from;
    var w = Math.min(extent, Math.max(traceMinWin(view), win.to - win.from));
    var from = Math.min(Math.max(win.from, full.from), full.to - w);
    return { from: from, to: from + w };
  }

  function traceResetWindow(view) {
    traceWin = traceFullWin(view);
  }

  function traceResetLanes(view) {
    traceLaneOrder = view.lanes.map(function (l) { return l.key; });
    traceHidden = {};
  }

  /// The lanes to draw, in the reader's order, minus the hidden ones.
  function traceLanesInOrder(view) {
    if (!traceLaneOrder) traceResetLanes(view);
    var byKey = {};
    view.lanes.forEach(function (l) { byKey[l.key] = l; });
    var out = [];
    traceLaneOrder.forEach(function (k) {
      if (byKey[k] && !traceHidden[k]) out.push(byKey[k]);
    });
    return out;
  }

  function traceHiddenCount(view) {
    var n = 0;
    view.lanes.forEach(function (l) { if (traceHidden && traceHidden[l.key]) n += 1; });
    return n;
  }

  // ---- windowed culling and pixel-column aggregation ------------------------
  //
  // **What bounds this drawing is pixels times lanes, not the dataset.** The
  // reference capture holds 112,801 spans and, aggregated onto a 1,170 px plot
  // across 26 lanes, draws under 2,600 rects at *every* zoom level — fewer as
  // you zoom in, because fewer spans are in the window. That measurement is
  // why this view is still SVG rather than a canvas: a canvas would have cost
  // CSS-variable theming, hand-written hit-testing and the browser harness's
  // ability to inspect what was drawn, to solve an element count that
  // aggregation had already bounded.
  //
  // Spans arrive sorted by `from` within a lane (`trace.rs` builds them in
  // record order off a clock the view has already vetted for monotonicity), so
  // finding the window is a binary search rather than a scan of the lane.

  /// Index of the first span whose `from` is at or after `t`.
  function traceLowerBound(spans, t) {
    var lo = 0;
    var hi = spans.length;
    while (lo < hi) {
      var mid = (lo + hi) >> 1;
      if (spans[mid].from < t) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  // The four facts a merged run has to carry out of aggregation. They are
  // flags on a byte rather than fields on an object because a run is split
  // wherever they change: merging a vouched-for span into the same block as
  // one that crosses a gap would fold an unestablished continuity into a
  // clean-looking bar, which is exactly the quiet lie decision 10 exists to
  // prevent. Measured on the reference capture, splitting on all four costs
  // **nothing** — 1,937 rects either way, because open and gap-crossing spans
  // are rare — so the honest version is also the cheap one.
  var TRACE_F_ON = 1;
  var TRACE_F_GAP = 2;
  var TRACE_F_SUBRES = 4;
  var TRACE_F_OPEN = 8;

  function traceScratchFor(cols) {
    if (!traceScratch || traceScratch.cols !== cols) {
      traceScratch = {
        cols: cols,
        flags: new Uint8Array(cols),
        starts: new Int32Array(cols),
        one: new Array(cols),
      };
    }
    return traceScratch;
  }

  /// One lane's visible spans, merged into per-pixel-column occupancy runs.
  ///
  /// A span is attributed to every column it covers (so the drawing is right)
  /// and *counted* only on the column it starts in (so a run's "N runs here"
  /// counts each span once rather than once per column it spans).
  function traceAggregateLane(lane, win, cols) {
    var spans = lane.spans;
    var s = traceScratchFor(cols);
    s.flags.fill(0);
    s.starts.fill(0);
    var scale = cols / Math.max(1, win.to - win.from);
    var visible = 0;
    if (spans.length) {
      var i = traceLowerBound(spans, win.from);
      // One step back covers the span already running when the window opens.
      // Exactly one, because spans within a lane do not overlap: the one
      // before that ends no later than this one begins.
      while (i > 0 && spans[i - 1].to >= win.from) i -= 1;
      for (; i < spans.length; i += 1) {
        var sp = spans[i];
        if (sp.from > win.to) break;
        if (sp.to < win.from) continue;
        var c0 = Math.floor((Math.max(sp.from, win.from) - win.from) * scale);
        var c1 = Math.floor((Math.min(sp.to, win.to) - win.from) * scale);
        if (c0 < 0) c0 = 0;
        if (c0 > cols - 1) c0 = cols - 1;
        if (c1 < c0) c1 = c0;
        if (c1 > cols - 1) c1 = cols - 1;
        var f = TRACE_F_ON;
        if (sp.crosses_gap) f |= TRACE_F_GAP;
        if (sp.below_resolution) f |= TRACE_F_SUBRES;
        if (sp.open_start || sp.open_end) f |= TRACE_F_OPEN;
        for (var c = c0; c <= c1; c += 1) s.flags[c] |= f;
        s.starts[c0] += 1;
        s.one[c0] = s.starts[c0] === 1 ? sp : null;
        visible += 1;
      }
    }
    var runs = [];
    var col = 0;
    while (col < cols) {
      var flags = s.flags[col];
      if (!flags) {
        col += 1;
        continue;
      }
      var start = col;
      var count = 0;
      var only = null;
      while (col < cols && s.flags[col] === flags) {
        count += s.starts[col];
        if (s.starts[col] === 1 && only === null) only = s.one[col];
        col += 1;
      }
      runs.push({
        c0: start,
        c1: col - 1,
        flags: flags,
        count: count,
        one: count === 1 ? only : null,
      });
    }
    return { runs: runs, visible: visible };
  }

  // ---- the study-action row -------------------------------------------------

  /// Whether this view's step row can be drawn at all. `view.steps` is
  /// `trace.rs`'s own projection of embarch-core's per-step arrival stamps
  /// onto this axis — `placeable: false` means there is no time base to
  /// project onto, and the row is then **not drawn**, with the reason said in
  /// `#trace-steps-note` rather than bands invented at guessed positions.
  function traceStepsPlaced(view) {
    return !!(view.steps && view.steps.placeable && view.steps.bands && view.steps.bands.length);
  }

  /// Outcome colours, in the vocabulary Core reports them in. `Pass`, `Fail`
  /// and `TimedOut` are all present in a single real capture, so all three are
  /// distinguishable rather than "green or not green".
  function traceOutcomeColor(outcome) {
    if (outcome === "Pass") return "var(--success)";
    if (outcome === "Fail") return "var(--danger)";
    if (outcome === "TimedOut") return "var(--warning)";
    return "var(--info)";
  }

  // ---- drawing --------------------------------------------------------------

  /// Coalesces redraws onto the next animation frame. A wheel gesture fires
  /// dozens of events and a drag fires one per mouse sample; redrawing per
  /// event would draw frames the compositor never shows.
  function traceScheduleDraw() {
    if (traceDrawQueued) return;
    traceDrawQueued = true;
    requestAnimationFrame(function () {
      traceDrawQueued = false;
      if (traceView) drawTraceChart(traceView);
    });
  }

  function drawTraceChart(view) {
    var svg = trEl("trace-chart");
    var head = trEl("trace-head");
    if (!svg) return;
    if (!traceWin) traceResetWindow(view);
    var win = traceClampWin(view, traceWin);
    traceWin = win;

    // Both SVGs must share one x mapping or the axis would label a position
    // the lanes do not draw at, so the header is sized from the *body's* own
    // measured width — which is the one that shrinks when the lane list grows
    // tall enough to raise a vertical scrollbar.
    var width = Math.max(640, svg.clientWidth || svg.parentElement.clientWidth || 900);
    var lanes = traceLanesInOrder(view);
    // ~6.6 px per character at 11.5 px IBM Plex Mono, plus the 12 px the
    // label is inset from the plot and a little breathing room.
    var widest = 0;
    lanes.forEach(function (l) { widest = Math.max(widest, l.label.length); });
    traceGutter = Math.max(
      TRACE_GUTTER_MIN,
      Math.min(TRACE_GUTTER_MAX, Math.ceil(widest * 6.6) + 24)
    );
    var plotLeft = traceGutter;
    var plotRight = width - TRACE_PAD_RIGHT;
    var plotW = Math.max(1, plotRight - plotLeft);
    var cols = Math.max(1, Math.round(plotW));
    var span = Math.max(1, win.to - win.from);
    function x(t) {
      return plotLeft + ((t - win.from) / span) * plotW;
    }
    function clampX(v) {
      return Math.max(plotLeft, Math.min(plotRight, v));
    }
    // Every *position* drawn or described below, at the precision this window
    // can distinguish. Durations stay on `fmtSpanLen` — a length is legible at
    // its own magnitude whatever the window is.
    var tier = Math.max(Math.abs(win.to - view.t_from), Math.abs(win.from - view.t_from));
    function at(t) {
      return fmtAxisT(view, t - view.t_from, span, tier);
    }

    var stepped = traceStepsPlaced(view);
    var headH = TRACE_AXIS_H + (stepped ? TRACE_STEP_H + TRACE_STEP_GAP * 2 : 0);
    var bodyH = Math.max(TRACE_ROW_H, lanes.length * TRACE_ROW_H) + TRACE_BODY_PAD;

    // ---- body: lanes ------------------------------------------------------
    var parts = [];
    // Two hatches, and they mean different things: a span the data cannot
    // vouch for the continuity of, and an interval the firmware said it lost
    // records in. Defined once here and referenced by `url(#…)` from the
    // header SVG too — one document, one pair of ids.
    parts.push(
      '<defs>' +
      '<pattern id="tr-gap" width="8" height="8" patternTransform="rotate(45)" patternUnits="userSpaceOnUse">' +
      '<rect width="8" height="8" fill="var(--danger-soft-bg)"/>' +
      '<line x1="0" y1="0" x2="0" y2="8" stroke="var(--danger)" stroke-width="2"/></pattern>' +
      '<pattern id="tr-cross" width="7" height="7" patternTransform="rotate(45)" patternUnits="userSpaceOnUse">' +
      '<rect width="7" height="7" fill="var(--accent-soft-bg)"/>' +
      '<line x1="0" y1="0" x2="0" y2="7" stroke="var(--accent)" stroke-width="2.4"/></pattern>' +
      '<pattern id="tr-delay" width="6" height="6" patternTransform="rotate(45)" patternUnits="userSpaceOnUse">' +
      '<rect width="6" height="6" fill="var(--bg-surface-inset)"/>' +
      '<line x1="0" y1="0" x2="0" y2="6" stroke="var(--text-tertiary)" stroke-width="1.4"/></pattern>' +
      "</defs>"
    );

    // Vertical grid, at the same six positions the header labels.
    var ticks = 6;
    var t;
    for (t = 0; t <= ticks; t += 1) {
      var gx = x(win.from + (span * t) / ticks);
      parts.push(
        '<line x1="' + gx + '" y1="0" x2="' + gx + '" y2="' + (bodyH - 4) +
        '" stroke="var(--border)" stroke-width="1" opacity="0.7"/>'
      );
    }

    // Gap bands, drawn first so records that survived at their edges stay
    // visible on top of them. A gap is "records were lost somewhere in here",
    // not "nothing happened here": the records at both ends of the band
    // survived, and the band itself is a **bound** — a gap record is the first
    // record of its frame, so the losses fall between the previous frame's
    // arrival and its own. Erasing what survived to make the picture tidier
    // would be its own lie.
    var visibleGaps = view.gaps.filter(function (g) {
      return g.to >= win.from && g.from <= win.to;
    });
    visibleGaps.forEach(function (g) {
      var gx0 = clampX(x(g.from));
      var gw = Math.max(2, clampX(x(g.to)) - gx0);
      parts.push(
        '<rect x="' + gx0 + '" y="0" width="' + gw + '" height="' + (bodyH - 4) +
        '" fill="url(#tr-gap)" stroke="var(--danger)" ' +
        'stroke-width="1" stroke-dasharray="3 3"><title>' +
        escapeHtml(g.records_lost + " records lost somewhere inside " +
          fmtSpanLen(view, g.to - g.from) +
          (g.unbounded_start
            ? " — reported by the first frame in the capture, so there is no earlier arrival to " +
              "bound it with: its extent is unknown, not zero"
            : " — a bound between two frame arrivals, not a measurement") +
          ". What is drawn at this band's edges is what survived, not what happened") +
        "</title></rect>"
      );
    });

    var drawnRects = 0;
    lanes.forEach(function (lane, i) {
      var top = i * TRACE_ROW_H;
      var mid = top + TRACE_ROW_H / 2;
      var barTop = mid - TRACE_BAR_H / 2;

      parts.push(
        '<text x="' + (traceGutter - 12) + '" y="' + (mid + 4) + '" text-anchor="end" ' +
        'fill="' + (lane.unnamed ? "var(--text-tertiary)" : "var(--text-primary)") + '" ' +
        'font-size="11.5" font-family="IBM Plex Mono, monospace"' +
        (lane.unnamed ? ' font-style="italic"' : "") + ">" +
        "<title>" + escapeHtml(lane.label + " — " + lane.kind + ", " + lane.spans.length + " run(s) in the whole capture") +
        "</title>" + escapeHtml(lane.label) + "</text>"
      );
      if (lane.unnamed) {
        parts.push(
          '<line x1="' + (traceGutter - 12 - Math.min(240, lane.label.length * 6.6)) + '" y1="' +
          (mid + 7) + '" x2="' + (traceGutter - 12) + '" y2="' + (mid + 7) +
          '" stroke="var(--text-tertiary)" stroke-width="1" stroke-dasharray="2 2"/>'
        );
      }
      parts.push(
        '<line x1="' + plotLeft + '" y1="' + mid + '" x2="' + plotRight + '" y2="' + mid +
        '" stroke="var(--border)" stroke-width="1" stroke-dasharray="2 4"/>'
      );

      var agg = traceAggregateLane(lane, win, cols);
      agg.runs.forEach(function (run) {
        var rx = plotLeft + run.c0;
        var rw = Math.max(1.5, run.c1 - run.c0 + 1);
        var crosses = (run.flags & TRACE_F_GAP) !== 0;
        var subres = (run.flags & TRACE_F_SUBRES) !== 0;
        var open = (run.flags & TRACE_F_OPEN) !== 0;
        var fill = crosses ? "url(#tr-cross)" : "var(--accent)";
        var title;
        if (run.one) {
          var sp = run.one;
          // A run that is exactly one span says exactly what that span says —
          // aggregation must not cost a reader the numbers they zoomed in for.
          title =
            at(sp.from) + " → " + at(sp.to) +
            (sp.below_resolution
              ? " (one frame: duration below this capture's resolution)"
              : " (" + fmtSpanLen(view, sp.to - sp.from) + ")") +
            (sp.open_start ? " · no switch-in record: this run was already going when it became observable" : "") +
            (sp.open_end ? " · no closing record: the bar ends at the next traced event, which is not when it ended" : "") +
            (sp.crosses_gap ? " · overlaps a gap: events inside it were lost, so continuity is not established" : "");
        } else {
          // A merged block, said as one. The count is of runs that *begin*
          // inside this block, and the width is the block's, not any one
          // span's — so neither number can be read as a duration.
          var blockFrom = win.from + (run.c0 / cols) * span;
          var blockTo = win.from + ((run.c1 + 1) / cols) * span;
          title =
            run.count + " run(s) of this subject merged into " + (run.c1 - run.c0 + 1) +
            " pixel column(s), " + at(Math.round(blockFrom)) + " → " + at(Math.round(blockTo)) +
            " — zoom in to separate them; this block's width is not any one run's duration" +
            (crosses ? " · at least one of them overlaps a gap, so continuity is not established across this block" : "") +
            (subres ? " · at least one of them is below this capture's resolution" : "") +
            (open ? " · at least one of them has no opening or closing record, so its extent is not a measurement" : "");
        }
        parts.push(
          '<rect x="' + rx + '" y="' + barTop + '" width="' + rw + '" height="' + TRACE_BAR_H +
          '" rx="2" fill="' + fill + '"' +
          (open || subres ? ' opacity="0.62"' : "") +
          "><title>" + escapeHtml(title) + "</title></rect>"
        );
        drawnRects += 1;
        // Ragged edges: dashed where a record is missing, so an extent never
        // reads as a measurement. Drawn only for an unmerged run — a dashed
        // edge on a block of forty merged runs would point at a column, not at
        // the span whose record is missing, and the block's own tooltip says
        // it instead.
        if (run.one && run.one.open_start) {
          parts.push(
            '<line x1="' + rx + '" y1="' + barTop + '" x2="' + rx + '" y2="' + (barTop + TRACE_BAR_H) +
            '" stroke="var(--warning)" stroke-width="2" stroke-dasharray="2 2"/>'
          );
        }
        if (run.one && run.one.open_end) {
          parts.push(
            '<line x1="' + (rx + rw) + '" y1="' + barTop + '" x2="' + (rx + rw) + '" y2="' +
            (barTop + TRACE_BAR_H) + '" stroke="var(--warning)" stroke-width="2" stroke-dasharray="2 2"/>'
          );
        }
      });

      lane.points.forEach(function (pt) {
        if (pt.t < win.from || pt.t > win.to) return;
        var px = x(pt.t);
        parts.push(
          '<path d="M' + px + " " + (mid - 6) + " L" + (px + 5) + " " + mid + " L" + px + " " +
          (mid + 6) + " L" + (px - 5) + " " + mid + ' Z" fill="var(--info)"><title>' +
          escapeHtml(pt.kind + " · " + pt.label) + "</title></path>"
        );
      });
    });

    // Markers last, over everything: they are the engineer's own annotations
    // and the reason a trace is worth reading against a specific run. A very
    // faint full-height rule here; the bright locatable tick is in the header,
    // where it stays put while the lanes scroll.
    var visibleMarkers = view.markers.filter(function (m) {
      return m.t >= win.from && m.t <= win.to;
    });
    visibleMarkers.forEach(function (m) {
      var mx = x(m.t);
      parts.push(
        '<line x1="' + mx + '" y1="0" x2="' + mx + '" y2="' + (bodyH - 4) +
        '" stroke="var(--warning)" stroke-width="1" opacity="0.16"/>'
      );
    });

    if (!lanes.length) {
      parts.push(
        '<text x="' + (plotLeft + 12) + '" y="' + (TRACE_ROW_H / 2 + 4) + '" ' +
        'fill="var(--text-tertiary)" font-size="12">' +
        escapeHtml("Every lane is hidden — nothing is being drawn. Use Lanes… to show some.") +
        "</text>"
      );
    }

    svg.setAttribute("viewBox", "0 0 " + width + " " + bodyH);
    svg.setAttribute("height", String(bodyH));
    svg.innerHTML = parts.join("");

    // ---- header: the axis, the study-action row, marker ticks -------------
    if (head) {
      var hp = [];
      var stepTop = TRACE_AXIS_H + TRACE_STEP_GAP;

      // Gap bands and marker rules continue through the header, so a band a
      // reader sees in the lanes is the same band under the axis label that
      // dates it.
      visibleGaps.forEach(function (g) {
        var hx = clampX(x(g.from));
        var hw = Math.max(2, clampX(x(g.to)) - hx);
        hp.push(
          '<rect x="' + hx + '" y="' + TRACE_AXIS_H + '" width="' + hw + '" height="' +
          (headH - TRACE_AXIS_H) + '" fill="url(#tr-gap)" opacity="0.5"/>'
        );
      });

      for (t = 0; t <= ticks; t += 1) {
        var tickAt = win.from + (span * t) / ticks;
        var tx = x(tickAt);
        hp.push(
          '<line x1="' + tx + '" y1="' + (TRACE_AXIS_H - 6) + '" x2="' + tx + '" y2="' + headH +
          '" stroke="var(--border)" stroke-width="1" opacity="0.7"/>' +
          // The end labels are anchored inward rather than centred: at full
          // zoom the last one is the capture's own length, and half of it
          // hanging off the right edge of the viewBox is exactly the number a
          // reader came for.
          '<text x="' + tx + '" y="' + (TRACE_AXIS_H - 10) + '" text-anchor="' +
          (t === 0 ? "start" : t === ticks ? "end" : "middle") + '" ' +
          'fill="var(--text-tertiary)" font-size="10.5" font-family="IBM Plex Mono, monospace">' +
          escapeHtml(at(Math.round(tickAt))) + "</text>"
        );
      }

      if (stepped) {
        // **The row's own clock is named in the gutter, not in a tooltip.**
        // On `dut-cycles` these bands are Core's host-clock stamps *projected*
        // onto the DUT's counter and are good to about `resolution_ms`; the
        // lanes beneath them are microsecond-exact. A row that looked
        // microsecond-aligned to those lanes would be claiming a precision
        // this projection does not have.
        hp.push(
          '<text x="' + (traceGutter - 12) + '" y="' + (stepTop + TRACE_STEP_H / 2 + 4) +
          '" text-anchor="end" fill="var(--text-secondary)" font-size="11.5" ' +
          'font-family="IBM Plex Mono, monospace">' +
          escapeHtml(view.steps.projected ? "study step (±" +
            (view.steps.accuracy_ms === null || view.steps.accuracy_ms === undefined
              ? "?"
              : view.steps.accuracy_ms) + " ms)" : "study step") +
          "</text>"
        );
        view.steps.bands.forEach(function (b) {
          if (b.to < win.from || b.from > win.to) return;
          var bx0 = clampX(x(b.from));
          var bx1 = clampX(x(b.to));
          var bxd = clampX(x(b.exec_from));
          var color = traceOutcomeColor(b.outcome);
          var detail =
            "step " + b.index + " · " + b.name + " → " + b.outcome +
            (b.reason ? " (" + b.reason + ")" : "") +
            " · " + (b.delay_before_ms > 0
              ? b.delay_before_ms + " ms declared delay, then "
              : "no declared delay, ") +
            fmtSpanLen(view, Math.max(0, b.to - b.exec_from)) + " running" +
            (view.steps.projected
              ? " · placed from embarch-core's own receipt clock, projected onto the DUT's counter to about " +
                view.steps.accuracy_ms + " ms — not microsecond-aligned to the lanes below"
              : " · embarch-core's own clock, which is also this axis, so this band is exactly aligned") +
            (b.clipped_start ? " · began before this capture starts" : "") +
            (b.clipped_end ? " · ended after this capture ends" : "");

          // The delay is drawn as part of the step and visibly not as part of
          // its execution. Without the split a step reads as having taken its
          // own delay — `close-nus-window` carries 5 s of it and
          // `drain-bds-2min` 3 s, which is most of what those bands would
          // otherwise appear to measure.
          if (bxd > bx0 + 0.5) {
            hp.push(
              '<rect x="' + bx0 + '" y="' + stepTop + '" width="' + (bxd - bx0) + '" height="' +
              TRACE_STEP_H + '" fill="url(#tr-delay)" stroke="var(--border)" stroke-width="1"><title>' +
              escapeHtml(detail) + "</title></rect>"
            );
          }
          if (bx1 > bxd + 0.5) {
            hp.push(
              '<rect x="' + bxd + '" y="' + stepTop + '" width="' + (bx1 - bxd) + '" height="' +
              TRACE_STEP_H + '" rx="2" fill="' + color + '" opacity="0.42" stroke="' + color +
              '" stroke-width="1"><title>' + escapeHtml(detail) + "</title></rect>"
            );
          }
          if (b.clipped_start) {
            hp.push(
              '<line x1="' + bx0 + '" y1="' + stepTop + '" x2="' + bx0 + '" y2="' +
              (stepTop + TRACE_STEP_H) + '" stroke="var(--text-tertiary)" stroke-width="2" ' +
              'stroke-dasharray="2 2"/>'
            );
          }
          if (b.clipped_end) {
            hp.push(
              '<line x1="' + bx1 + '" y1="' + stepTop + '" x2="' + bx1 + '" y2="' +
              (stepTop + TRACE_STEP_H) + '" stroke="var(--text-tertiary)" stroke-width="2" ' +
              'stroke-dasharray="2 2"/>'
            );
          }
          var labelW = bx1 - bxd;
          if (labelW > 44) {
            hp.push(
              '<text x="' + (bxd + labelW / 2) + '" y="' + (stepTop + TRACE_STEP_H / 2 + 4) +
              '" text-anchor="middle" fill="var(--text-primary)" font-size="10.5" ' +
              'font-family="IBM Plex Mono, monospace" pointer-events="none">' +
              escapeHtml(traceFitLabel(b.name, labelW)) + "</text>"
            );
          }
        });
      }

      // The bright, locatable half of a marker.
      visibleMarkers.forEach(function (m) {
        var mx = x(m.t);
        hp.push(
          '<line x1="' + mx + '" y1="' + (headH - 10) + '" x2="' + mx + '" y2="' + headH +
          '" stroke="var(--warning)" stroke-width="1.6"><title>' +
          escapeHtml(m.label + " (arg " + m.arg + ") in frame " + m.frame_index + ", at " +
            at(m.t)) + "</title></line>"
        );
      });

      head.setAttribute("viewBox", "0 0 " + width + " " + headH);
      head.setAttribute("height", String(headH));
      head.style.width = width + "px";
      head.innerHTML = hp.join("");
    }

    traceUpdateReadout(view, win, span, lanes, drawnRects);
  }

  /// Truncates a step name to what its band can actually hold, with an
  /// ellipsis so a clipped name never reads as a shorter one. ~6.1 px per
  /// character at 10.5 px IBM Plex Mono.
  function traceFitLabel(name, widthPx) {
    var fits = Math.floor((widthPx - 8) / 6.1);
    if (fits >= name.length) return name;
    if (fits < 4) return "";
    return name.slice(0, fits - 1) + "…";
  }

  /// The one line that says where in the capture the reader is. The axis
  /// labels alone are offsets; this says how much of the whole they are
  /// looking at, which is what makes a 34 µs span's zoom level legible.
  function traceUpdateReadout(view, win, span, lanes, rects) {
    var el = trEl("trace-window");
    if (!el) return;
    var full = traceFullWin(view);
    var extent = full.to - full.from;
    var w = win.to - win.from;
    var zoomed = w < extent;
    el.textContent =
      (zoomed
        ? "showing " + fmtSpanLen(view, w) + " of " + fmtSpanLen(view, extent) + " — " +
          fmtAxisT(view, win.from - view.t_from, span, win.to - view.t_from) + " to " +
          fmtAxisT(view, win.to - view.t_from, span, win.to - view.t_from)
        : "showing the whole capture, " + fmtSpanLen(view, extent)) +
      " · " + lanes.length + " of " + view.lanes.length + " lanes · " + rects +
      " marks drawn";
  }

  // ---- navigation: wheel-zooms at the cursor, drag pans ---------------------
  //
  // No overview strip, no zoom buttons and no scrollbar for the time axis:
  // the axis labels are what say where the reader is, and they are correct at
  // every scale because `fmtSpanLen` formats against `view.unit`. What the
  // chrome does carry is a way back — double-click anywhere on the plot, or
  // the Fit button — because a reader who has zoomed into a 34 µs ISR span
  // has no other way to find the whole capture again.

  var traceDrag = null;

  function traceUnitsPerPx(view, win) {
    var svg = trEl("trace-chart");
    var width = Math.max(640, (svg && svg.clientWidth) || 900);
    var plotW = Math.max(1, width - TRACE_PAD_RIGHT - traceGutter);
    return (win.to - win.from) / plotW;
  }

  /// The axis value under a pointer event, in view units. Returns `null` when
  /// the pointer is left of the plot (over the lane-name gutter), where a
  /// zoom anchor would be meaningless.
  function traceAxisAt(view, win, ev) {
    var svg = trEl("trace-chart");
    if (!svg) return null;
    var rect = svg.getBoundingClientRect();
    var px = ev.clientX - rect.left;
    if (px < traceGutter) return null;
    var plotW = Math.max(1, rect.width - TRACE_PAD_RIGHT - traceGutter);
    var f = Math.max(0, Math.min(1, (px - traceGutter) / plotW));
    return win.from + f * (win.to - win.from);
  }

  function traceOnWheel(ev) {
    var view = traceView;
    if (!view || !traceWin) return;
    // Shift-wheel is left alone so the lane list can still be scrolled with
    // the wheel while the pointer is over the plot — which is where a reader's
    // pointer already is.
    if (ev.shiftKey) return;
    var anchor = traceAxisAt(view, traceWin, ev);
    if (anchor === null) return;
    ev.preventDefault();
    // `deltaMode` 1 is lines and 2 is pages; normalising them keeps a
    // Firefox notch and a Chrome notch roughly the same gesture.
    // Firefox on Linux reports 3 *lines* per notch and Chrome reports ~100
    // pixels; 33 px a line puts both on roughly the same gesture, so the same
    // flick of the same wheel zooms by the same amount in either.
    var delta = ev.deltaY * (ev.deltaMode === 1 ? 33 : ev.deltaMode === 2 ? 700 : 1);
    var factor = Math.exp(delta * 0.0028);
    var w = traceWin.to - traceWin.from;
    var next = w * factor;
    var frac = (anchor - traceWin.from) / w;
    traceWin = traceClampWin(view, { from: anchor - frac * next, to: anchor + (1 - frac) * next });
    traceScheduleDraw();
  }

  function traceOnPointerDown(ev) {
    var view = traceView;
    if (!view || !traceWin) return;
    if (ev.button !== 0) return;
    if (traceAxisAt(view, traceWin, ev) === null) return;
    traceDrag = { x: ev.clientX, win: { from: traceWin.from, to: traceWin.to } };
    var plot = trEl("trace-plot");
    if (plot) plot.classList.add("is-panning");
    if (ev.currentTarget.setPointerCapture && ev.pointerId !== undefined) {
      try { ev.currentTarget.setPointerCapture(ev.pointerId); } catch (e) { /* not fatal */ }
    }
    ev.preventDefault();
  }

  function traceOnPointerMove(ev) {
    if (!traceDrag || !traceView) return;
    var perPx = traceUnitsPerPx(traceView, traceDrag.win);
    var dt = (ev.clientX - traceDrag.x) * perPx;
    traceWin = traceClampWin(traceView, {
      from: traceDrag.win.from - dt,
      to: traceDrag.win.to - dt,
    });
    traceScheduleDraw();
  }

  function traceOnPointerUp() {
    traceDrag = null;
    var plot = trEl("trace-plot");
    if (plot) plot.classList.remove("is-panning");
  }

  function traceFit() {
    if (!traceView) return;
    traceResetWindow(traceView);
    traceScheduleDraw();
  }

  // ---- lane filtering, collapse and order ----------------------------------
  //
  // The reading that found the connection-interval fault was "watch
  // `rtc0_nrf5_isr`, `swi_lll_nrf5_isr` and `radio_nrf5_isr` and ignore
  // everything else", and doing that by eye across 26 lanes is the friction
  // this removes.
  //
  // **Filtering here changes the drawing and nothing else.** The Load
  // repartition below is computed in Rust across every lane, and it stays that
  // way — a hidden lane must not quietly leave a denominator. When a filter is
  // active the table says so in its own words rather than silently agreeing.

  function traceRenderLanePanel(view) {
    var list = trEl("trace-lane-list");
    if (!list) return;
    if (!traceLaneOrder) traceResetLanes(view);
    var byKey = {};
    view.lanes.forEach(function (l) { byKey[l.key] = l; });
    list.innerHTML = traceLaneOrder
      .map(function (key, i) {
        var lane = byKey[key];
        if (!lane) return "";
        var hidden = !!traceHidden[key];
        return (
          '<div class="trace-lane-row' + (hidden ? " is-hidden" : "") + '" data-lane="' +
          escapeHtml(key) + '">' +
          '<input type="checkbox" data-lane-toggle="' + escapeHtml(key) + '"' +
          (hidden ? "" : " checked") + ' aria-label="show lane ' + escapeHtml(lane.label) + '" />' +
          '<span class="trace-lane-name' + (lane.unnamed ? " trace-key-unnamed" : "") + '">' +
          escapeHtml(lane.label) + "</span>" +
          '<span class="trace-lane-kind">' + escapeHtml(lane.kind) + "</span>" +
          '<span class="trace-lane-count mono">' + lane.spans.length + "</span>" +
          '<button class="btn btn-tiny" data-lane-up="' + escapeHtml(key) + '"' +
          (i === 0 ? " disabled" : "") + ' title="move up">&#9650;</button>' +
          '<button class="btn btn-tiny" data-lane-down="' + escapeHtml(key) + '"' +
          (i === traceLaneOrder.length - 1 ? " disabled" : "") +
          ' title="move down">&#9660;</button>' +
          "</div>"
        );
      })
      .join("");

    var note = trEl("trace-load-filter");
    if (note) {
      var hidden = traceHiddenCount(view);
      if (hidden > 0) {
        note.style.display = "block";
        note.innerHTML =
          "<strong>" + hidden + " of " + view.lanes.length + " lanes are hidden on the timeline " +
          "above.</strong> This table is not filtered with it — every share below is still of the " +
          "whole window across every lane, so hiding a lane cannot quietly change a denominator here.";
      } else {
        note.style.display = "none";
      }
    }
  }

  function traceSetLaneVisible(key, visible) {
    if (visible) delete traceHidden[key];
    else traceHidden[key] = true;
  }

  function traceMoveLane(key, by) {
    var i = traceLaneOrder.indexOf(key);
    var j = i + by;
    if (i < 0 || j < 0 || j >= traceLaneOrder.length) return;
    traceLaneOrder.splice(j, 0, traceLaneOrder.splice(i, 1)[0]);
  }

  function traceLanePanelClick(ev) {
    var view = traceView;
    if (!view) return;
    var t = ev.target;
    var key;
    if (t.hasAttribute && t.hasAttribute("data-lane-toggle")) {
      key = t.getAttribute("data-lane-toggle");
      traceSetLaneVisible(key, t.checked);
    } else if (t.closest && t.closest("[data-lane-up]")) {
      traceMoveLane(t.closest("[data-lane-up]").getAttribute("data-lane-up"), -1);
    } else if (t.closest && t.closest("[data-lane-down]")) {
      traceMoveLane(t.closest("[data-lane-down]").getAttribute("data-lane-down"), 1);
    } else {
      return;
    }
    traceRenderLanePanel(view);
    traceScheduleDraw();
  }

  function traceLaneQuickAction(action) {
    var view = traceView;
    if (!view) return;
    if (action === "all") {
      traceHidden = {};
    } else if (action === "none") {
      view.lanes.forEach(function (l) { traceHidden[l.key] = true; });
    } else if (action === "no-idle") {
      // The largest lane in the reference capture by a wide margin (29,677
      // spans), and the first thing anyone wants out of the way.
      view.lanes.forEach(function (l) {
        if (l.kind === "idle" || l.label === "idle") traceHidden[l.key] = true;
      });
    } else if (action === "isrs") {
      view.lanes.forEach(function (l) {
        if (l.kind === "isr") delete traceHidden[l.key];
        else traceHidden[l.key] = true;
      });
    } else if (action === "reset") {
      traceResetLanes(view);
    }
    traceRenderLanePanel(view);
    traceScheduleDraw();
  }

  function initTraceTab() {
    if (!trEl("trace-chart")) return;
    trEl("trace-load").addEventListener("click", traceLoadTaps);
    // `#trace?study=<id>&tap=<name>` opens a specific trace directly — the
    // same deep-link shape `#topology` already gives `embarch-topology`'s
    // `fix_it_url` (decision 19). A trace is the one thing in this UI worth
    // sending somebody a link to: it is post-hoc and belongs to one run.
    //
    // In the **fragment**, not the query string, for two reasons: the fragment
    // already selects the tab here, so the whole address stays one mechanism;
    // and a fragment never reaches the server, so a link to a trace costs no
    // round trip and leaks no study id into a log. A `?study=` query is
    // honoured too, for a link somebody hand-writes that way.
    var params = hashParams();
    var study = params.get("study") || new URLSearchParams(location.search).get("study");
    if (study) {
      trEl("trace-study").value = study;
      var wantedTap = params.get("tap") || new URLSearchParams(location.search).get("tap");
      traceLoadTaps().then(function () {
        if (wantedTap && trEl("trace-tap").querySelector('[value="' + CSS.escape(wantedTap) + '"]')) {
          trEl("trace-tap").value = wantedTap;
          traceLoadView();
        }
      });
    }
    trEl("trace-study").addEventListener("keydown", function (ev) {
      if (ev.key === "Enter") traceLoadTaps();
    });
    trEl("trace-tap").addEventListener("change", traceLoadView);
    var pending = null;
    window.addEventListener("resize", function () {
      if (!traceView) return;
      clearTimeout(pending);
      pending = setTimeout(function () { traceScheduleDraw(); }, 120);
    });

    // Navigation. `passive: false` on the wheel because zooming has to stop
    // the page from scrolling under the gesture — and only then, which is why
    // `traceOnWheel` returns without preventing the default when the pointer
    // is over the lane-name gutter or Shift is held.
    var plot = trEl("trace-plot");
    if (plot) {
      plot.addEventListener("wheel", traceOnWheel, { passive: false });
      plot.addEventListener("pointerdown", traceOnPointerDown);
      plot.addEventListener("pointermove", traceOnPointerMove);
      plot.addEventListener("pointerup", traceOnPointerUp);
      plot.addEventListener("pointercancel", traceOnPointerUp);
      plot.addEventListener("dblclick", function (ev) {
        ev.preventDefault();
        traceFit();
      });
    }
    var fit = trEl("trace-fit");
    if (fit) fit.addEventListener("click", traceFit);

    var lanesToggle = trEl("trace-lanes-toggle");
    var lanesPanel = trEl("trace-lanes-panel");
    if (lanesToggle && lanesPanel) {
      lanesToggle.addEventListener("click", function () {
        var open = lanesPanel.style.display !== "none";
        lanesPanel.style.display = open ? "none" : "block";
        lanesToggle.setAttribute("aria-expanded", open ? "false" : "true");
      });
    }
    if (lanesPanel) {
      lanesPanel.addEventListener("click", traceLanePanelClick);
      lanesPanel.addEventListener("change", traceLanePanelClick);
    }
    var quick = document.querySelectorAll("[data-lane-action]");
    Array.prototype.forEach.call(quick, function (btn) {
      btn.addEventListener("click", function () {
        traceLaneQuickAction(btn.getAttribute("data-lane-action"));
      });
    });
  }

  document.addEventListener("DOMContentLoaded", () => {
    initNav();
    initEnrollTab();
    initSignals();
    initStudyDesignerTab();
    initTraceTab();
    initDebugTab();
    initEvents();
    const toggle = document.querySelector(".theme-toggle");
    if (toggle) toggle.addEventListener("click", toggleTheme);
  });
})();
